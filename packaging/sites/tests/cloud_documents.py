#!/usr/bin/env python3
"""Run against a built Worker in disposable local Wrangler storage.

Build with worker-build --release --out-dir /absolute/path -- --locked from
packaging/sites/worker, then pass that output directory as the first argument.
Use --wrangler /path/to/wrangler if your PATH does not have Wrangler 4.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from hashlib import sha256
from http.client import HTTPConnection
import json
from pathlib import Path
from shutil import copyfile
import socket
import sqlite3
import subprocess
from tempfile import TemporaryDirectory
from threading import Event
from time import monotonic, sleep
from urllib.parse import quote
from uuid import uuid4


ROUTE = "/api/cloud/documents"
MEDIA_TYPE = "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
CAP = 16 * 1024 * 1024
OWNER = "synthetic-owner"
OTHER = "synthetic-other"
FIRST = b"PK\x03\x04\x00synthetic first snapshot\xff"
NEXT = b"PK\x03\x04\x00synthetic next snapshot\x80"


class Api:
    def __init__(self, port):
        self.port = port
        self.origin = f"http://127.0.0.1:{port}"

    def connection(self):
        return HTTPConnection("127.0.0.1", self.port, timeout=45)

    def request(self, method, path=ROUTE, *, owner=OWNER, headers=None, body=None):
        connection = self.connection()
        fields = {"Origin": self.origin, **(headers or {})}
        if owner is not None:
            fields["oai-authenticated-user-id"] = owner
        try:
            connection.request(method, path, body, fields, encode_chunked=True)
            response = connection.getresponse()
            return response.status, dict(
                (name.lower(), value) for name, value in response.getheaders()
            ), response.read()
        finally:
            connection.close()


def expect(response, status, code=None):
    observed, headers, body = response
    assert observed == status, (status, observed, body[:300])
    assert headers.get("cache-control") == "private, no-store, no-transform", headers
    if code is not None:
        assert json.loads(body)["code"] == code, body
    return headers, body


def save_headers(body, operation=None, *, etag=None, name=None):
    headers = {
        "Content-Type": MEDIA_TYPE,
        "Idempotency-Key": operation or str(uuid4()),
        "X-WordCraft-Size": str(len(body)),
        "X-WordCraft-SHA256": sha256(body).hexdigest(),
    }
    if etag is not None:
        headers["If-Match"] = etag
    if name is not None:
        headers["X-WordCraft-Name"] = quote(name, safe="")
    return headers


def download(api, path, etag, snapshot):
    headers, body = expect(api.request("GET", path), 200)
    assert headers["etag"] == etag, headers
    assert headers["content-type"] == MEDIA_TYPE, headers
    assert headers["x-content-type-options"] == "nosniff", headers
    assert headers["content-disposition"].startswith("attachment;"), headers
    assert body == snapshot, (len(body), len(snapshot))


def receipt(response, status):
    headers, body = expect(response, status)
    entry = json.loads(body)
    assert headers["etag"] == f'"{entry["revision"]}"', (headers, entry)
    return entry, headers["etag"]


def owner_library(api, owner=OWNER):
    return json.loads(expect(api.request("GET", owner=owner), 200)[1])["documents"]


def ordinary_requests(api):
    expect(api.request("GET", owner=None), 401)
    assert owner_library(api) == []
    create = save_headers(FIRST, name="Draft あ.docx")
    expect(api.request("POST", headers={**create, "Origin": "https://untrusted.invalid"}, body=FIRST), 403)
    for status, fields, body in [
        (400, {"X-WordCraft-Name": quote("../Draft.docx", safe="")}, FIRST),
        (415, {"Content-Type": "text/plain"}, FIRST),
        (400, {"X-WordCraft-Size": str(len(FIRST) + 1)}, FIRST),
        (400, {"X-WordCraft-SHA256": "0" * 64}, FIRST),
        (413, {"X-WordCraft-Size": str(CAP + 1)}, FIRST),
    ]:
        expect(api.request("POST", headers={
            **create, **fields, "Idempotency-Key": str(uuid4()),
        }, body=body), status)
    for chunks in [(FIRST[:-1],), (FIRST, b"extra")]:
        expect(api.request("POST", headers=save_headers(FIRST, name="Wrong length.docx"),
                           body=iter(chunks)), 400, "invalid_body")
    assert owner_library(api) == []

    first, base = receipt(api.request("POST", headers=create, body=FIRST), 201)
    assert first["name"] == "Draft あ.docx" and first["byte_size"] == len(FIRST), first
    path = f'{ROUTE}/{first["id"]}'
    assert owner_library(api) == [first]
    assert owner_library(api, OTHER) == []
    download(api, path, base, FIRST)
    expect(api.request("GET", path, owner=OTHER), 404)
    expect(api.request("PUT", path, owner=OTHER, headers=save_headers(NEXT, etag=base), body=NEXT), 404)
    expect(api.request("DELETE", path, owner=OTHER, headers={"If-Match": base}), 404)
    expect(api.request("PUT", path, headers=save_headers(NEXT), body=NEXT), 428)
    expect(api.request("DELETE", path), 428)
    for invalid in ["*", 'W/"1"', '"1", "2"']:
        expect(api.request("PUT", path, headers=save_headers(NEXT, etag=invalid), body=NEXT), 400)
        expect(api.request("DELETE", path, headers={"If-Match": invalid}), 400)

    save = save_headers(NEXT, etag=base)
    second, latest = receipt(api.request("PUT", path, headers=save, body=NEXT), 200)
    assert second["id"] == first["id"] and latest != base, second
    download(api, path, latest, NEXT)
    assert owner_library(api) == [second]
    expect(api.request("PUT", path, headers=save_headers(NEXT, etag=base), body=NEXT), 412)
    expect(api.request("DELETE", path, headers={"If-Match": base}), 412)
    assert receipt(api.request("PUT", path, headers=save, body=NEXT), 200) == (second, latest)
    assert receipt(api.request("POST", headers=create, body=FIRST), 201) == (first, base)
    expect(api.request("POST", headers={**create, "X-WordCraft-Name": "Changed.docx"}, body=FIRST), 409)
    print("Passed validation, exact bytes and ETags, owner scoping, and committed receipt replay", flush=True)
    return path, create, latest


def wait_for_save(db, operation, outcome="pending"):
    deadline = monotonic() + 8
    while monotonic() < deadline:
        row = db.execute(
            "SELECT object_key, outcome FROM cloud_saves WHERE owner = ? AND operation_id = ?",
            (OWNER, operation),
        ).fetchone()
        if row is not None and row[1] == outcome:
            return row[0]
        sleep(0.05)
    raise AssertionError(f"Worker did not reach {outcome} for the test operation")


def held_body(body, release):
    yield body[:1]
    assert release.wait(timeout=40), "test did not release upload"
    yield body[1:]


def concurrent_updates(api, db, path, base):
    release = Event()
    snapshots = (FIRST + b"left", FIRST + b"right")
    writes = [save_headers(body, etag=base) for body in snapshots]
    with ThreadPoolExecutor(max_workers=2) as pool:
        futures = [pool.submit(api.request, "PUT", path, headers=headers,
                               body=held_body(body, release))
                   for headers, body in zip(writes, snapshots)]
        try:
            for headers in writes:
                wait_for_save(db, headers["Idempotency-Key"])
        finally:
            release.set()
        responses = [future.result() for future in futures]
    assert sorted(response[0] for response in responses) == [200, 412], responses
    for response, snapshot in zip(responses, snapshots):
        if response[0] == 200:
            entry, latest = receipt(response, 200)
            assert entry["revision"] == int(base.strip('"')) + 1, entry
            download(api, path, latest, snapshot)
    assert db.execute(
        "SELECT outcome FROM cloud_saves WHERE owner = ? AND operation_id IN (?, ?) ORDER BY outcome",
        (OWNER, *(headers["Idempotency-Key"] for headers in writes)),
    ).fetchall() == [("committed",), ("stale",)]
    print("Passed concurrent publication after both writes reserved candidates", flush=True)
    return latest


def rewind_create(db, document, operation, *, target=None, base=0, lease_seconds=0):
    # Rewind D1 publication while retaining the real R2 upload and checksum.
    with db:
        db.execute("DELETE FROM cloud_documents WHERE owner = ? AND id = ?", (OWNER, document))
        key, = db.execute(
            """UPDATE cloud_saves
               SET outcome = 'pending', saved_at = NULL, settled = 0,
                   document_id = ?, base_revision = ?, lease_until = unixepoch() + ?
               WHERE owner = ? AND operation_id = ?
               RETURNING object_key""",
            (target or document, base, lease_seconds, OWNER, operation),
        ).fetchone()
    return key


def interrupted_uploads(api, db):
    name = "Recovered.docx"
    interrupted = save_headers(FIRST, name=name)
    operation = interrupted["Idempotency-Key"]
    document = uuid4().hex
    candidate = f"wordcraft-cloud/{uuid4().hex}"
    # A Worker that stopped after reservation leaves the row but may never put.
    with db:
        db.execute(
            """INSERT INTO cloud_saves
               (owner, operation_id, document_id, base_revision, name, byte_size, sha256, object_key, lease_until)
               VALUES (?, ?, ?, 0, ?, ?, ?, ?, 0)""",
            (OWNER, operation, document, name, len(FIRST), sha256(FIRST).hexdigest(), candidate),
        )
    expect(api.request("POST", headers=interrupted, body=FIRST), 409, "operation_abandoned")
    expect(api.request("POST", headers=interrupted, body=FIRST), 409, "operation_abandoned")
    expect(api.request("GET", f"{ROUTE}/{document}"), 404)

    interrupted = save_headers(FIRST, name=name)
    original, etag = receipt(api.request("POST", headers=interrupted, body=FIRST), 201)
    document = original["id"]
    path = f"{ROUTE}/{document}"
    rewind_create(db, document, interrupted["Idempotency-Key"])
    expect(api.request("GET", path), 404)
    # Let recovery commit, then close the connection without reading its response.
    connection = api.connection()
    try:
        connection.request("POST", ROUTE, FIRST, {
            **interrupted, "Origin": api.origin, "oai-authenticated-user-id": OWNER,
        })
        wait_for_save(db, interrupted["Idempotency-Key"], "committed")
    finally:
        connection.close()
    recovered, latest = receipt(api.request("POST", headers=interrupted, body=FIRST), 201)
    assert recovered["id"] == document and latest == etag, recovered
    download(api, path, latest, FIRST)
    expect(api.request("DELETE", path, headers={"If-Match": latest}), 204)
    print("Passed recovery before and after R2 PUT, including an unread commit response", flush=True)


def delete_during_upload(api, db, path, create, latest, read_object):
    document = path.rsplit("/", 1)[1]
    active, = db.execute(
        "SELECT object_key FROM cloud_documents WHERE owner = ? AND id = ?",
        (OWNER, document),
    ).fetchone()

    # Model a second interruption with bytes already written to its pending key.
    donor = save_headers(FIRST, name="Pending candidate.docx")
    stored, _ = receipt(api.request("POST", headers=donor, body=FIRST), 201)
    stored_candidate = rewind_create(
        db, stored["id"], donor["Idempotency-Key"], target=document,
        base=int(latest.strip('"')), lease_seconds=120,
    )
    assert read_object(stored_candidate) == FIRST

    release = Event()
    save = save_headers(NEXT, etag=latest)
    with ThreadPoolExecutor(max_workers=1) as pool:
        upload = pool.submit(api.request, "PUT", path, headers=save,
                             body=held_body(NEXT, release))
        try:
            candidate = wait_for_save(db, save["Idempotency-Key"])
            expect(api.request("PUT", path, headers=save, body=NEXT), 202, "operation_pending")
            expect(api.request("DELETE", path, headers={"If-Match": latest}), 204)
            expect(api.request("GET", path), 404)
            assert read_object(active) is None, "deleted document bytes are still stored"
            assert read_object(candidate) == b"", "unsettled candidate lacks its cleanup marker"
            assert read_object(stored_candidate) == b"", "stored candidate was not replaced by a marker"
            assert owner_library(api) == []
        finally:
            release.set()
        expect(upload.result(), 404)
    expect(api.request("POST", headers=create, body=FIRST), 404)
    expect(api.request("DELETE", path, headers={"If-Match": latest}), 204)
    assert read_object(candidate) in (b"", None), "late upload stored document bytes"
    print("Passed deletion during a held upload, R2 marker and byte purge, and replay denial", flush=True)


def library_pages(api, db):
    owner = "synthetic-pagination"
    ids = [f"{index:032x}" for index in range(51, -1, -1)]
    with db:
        # Equal saved times force the continuation cursor to use its ID tiebreaker.
        db.executemany(
            """INSERT INTO cloud_documents
               (owner, id, first_operation_id, name, revision, object_key, byte_size, saved_at)
               VALUES (?, ?, ?, 'Page.docx', 1, ?, 1, 1)""",
            [(owner, doc, str(uuid4()), f"wordcraft-cloud/{doc}") for doc in ids],
        )
    page = json.loads(expect(api.request("GET", owner=owner), 200)[1])
    assert [entry["id"] for entry in page["documents"]] == ids[:50], page
    cursor = quote(page["next_cursor"], safe="")
    page = json.loads(expect(api.request("GET", f"{ROUTE}?cursor={cursor}", owner=owner), 200)[1])
    assert [entry["id"] for entry in page["documents"]] == ids[50:], page
    assert page["next_cursor"] is None, page
    print("Passed stable library pagination for equal timestamps", flush=True)


def exact_limit(api):
    body = bytes(range(256)) * (CAP // 256)
    def chunks(extra=b""):
        for offset in range(0, CAP, 64 * 1024):
            yield body[offset:offset + 64 * 1024]
        if extra:
            yield extra

    oversized = save_headers(body, name="Oversized.docx")
    expect(api.request("POST", headers=oversized, body=chunks(b"x")), 413, "body_too_large")
    headers = save_headers(body, name="Limit.docx")
    entry, etag = receipt(api.request("POST", headers=headers, body=chunks()), 201)
    assert entry["byte_size"] == CAP, entry
    path = f'{ROUTE}/{entry["id"]}'
    download(api, path, etag, body)
    expect(api.request("DELETE", path, headers={"If-Match": etag}), 204)
    print("Passed exact 16 MiB upload/download and 16 MiB plus one byte rejection", flush=True)


def start_api(server):
    deadline = monotonic() + 20
    while monotonic() < deadline:
        try:
            if server.request("GET", "/")[0] == 200:
                return
        except OSError:
            sleep(0.1)
    raise RuntimeError(f"Wrangler did not start on port {server.port}")


@contextmanager
def local_storage(args, root):
    with TemporaryDirectory(prefix="wordcraft-cloud-check-") as directory:
        sandbox = Path(directory)
        for name in ("index.js", "index_bg.wasm"):
            copyfile(args.worker / name, sandbox / name)
        assets = sandbox / "assets"
        assets.mkdir()
        (assets / "index.html").write_text("<p>WordCraft storage integration check</p>")
        config = sandbox / "wrangler.json"
        config.write_text(json.dumps({
            "name": "wordcraft-cloud-check",
            "main": "index.js",
            "compatibility_date": "2026-04-01",
            "assets": {"directory": "assets", "binding": "ASSETS", "run_worker_first": True},
            "d1_databases": [{
                "binding": "DB",
                "database_name": "wordcraft-cloud-check",
                "database_id": "00000000-0000-4000-8000-000000000001",
                "migrations_dir": str(root / "drizzle"),
            }],
            "r2_buckets": [{"binding": "BUCKET", "bucket_name": "wordcraft-cloud-check"}],
        }))
        cli = [args.wrangler, "--config", str(config)]
        state = sandbox / "state"
        local = ["--local", "--persist-to", str(state)]
        migration = subprocess.run(
            [*cli, "d1", "migrations", "apply", "DB", *local],
            cwd=sandbox, timeout=60, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        )
        assert migration.returncode == 0, migration.stdout.decode(errors="replace")

        def read_object(key):
            destination = sandbox / "downloaded-object"
            result = subprocess.run(
                [*cli, "r2", "object", "get", f"wordcraft-cloud-check/{key}",
                 *local, "--file", str(destination)],
                cwd=sandbox, timeout=20, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            )
            if result.returncode == 0:
                return destination.read_bytes()
            output = result.stdout.decode(errors="replace")
            assert "does not exist" in output or "not found" in output.lower(), output
            return None
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            api = Api(sock.getsockname()[1])
        yield api, cli, local, state, sandbox, read_object


@contextmanager
def running_worker(api, cli, local, sandbox):
    with (sandbox / "wrangler.log").open("w+") as log:
        process = subprocess.Popen(
            [*cli, "dev", *local, "--ip", "127.0.0.1", "--port", str(api.port), "--inspector-port", "0"],
            cwd=sandbox, stdout=log, stderr=subprocess.STDOUT,
        )
        try:
            start_api(api)
            yield
        except Exception:
            log.seek(0)
            print(log.read(), flush=True)
            raise
        finally:
            process.terminate()
            process.wait(timeout=15)


def local_db(state):
    db_path, = (file for file in (state / "v3" / "d1").glob(
        "miniflare-D1DatabaseObject/*.sqlite"
    ) if file.name != "metadata.sqlite")
    return sqlite3.connect(db_path)


def run_local(args, root):
    with local_storage(args, root) as (api, cli, local, state, sandbox, read_object):
        with running_worker(api, cli, local, sandbox):
            path, create, latest = ordinary_requests(api)
            db = local_db(state)
            try:
                latest = concurrent_updates(api, db, path, latest)
                interrupted_uploads(api, db)
                library_pages(api, db)
                exact_limit(api)
                delete_during_upload(api, db, path, create, latest, read_object)
            finally:
                db.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("worker", type=Path)
    parser.add_argument("--wrangler", default="wrangler")
    run_local(parser.parse_args(), Path(__file__).resolve().parents[3])
