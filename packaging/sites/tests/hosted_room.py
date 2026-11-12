#!/usr/bin/env python3
"""Exercise owner-only room activation and replay across local Worker restarts.

Pass the absolute Worker output directory and, if needed, --wrangler.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from io import BytesIO
import json
from pathlib import Path
from threading import Event
from uuid import uuid4
from zipfile import ZipFile

from cloud_documents import (
    OTHER, ROUTE, download, expect, held_body, local_db, local_storage,
    receipt, running_worker, save_headers, wait_for_save,
)


def docx(text):
    output = BytesIO()
    with ZipFile(output, "w") as archive:
        archive.writestr("[Content_Types].xml", """<?xml version="1.0"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>""")
        archive.writestr("_rels/.rels", """<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>""")
        archive.writestr("word/document.xml", f"""<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">
  <w:body><w:p w14:paraId="00000001"><w:r><w:t>{text}</w:t></w:r></w:p></w:body>
</w:document>""")
    return output.getvalue()


def post(api, path, value, **kwargs):
    return api.request("POST", path, headers={"Content-Type": "application/json", **kwargs.pop("headers", {})},
                       body=json.dumps(value).encode(), **kwargs)


def payload(response, status=200):
    return json.loads(expect(response, status)[1])


def activate(api, db):
    source = docx("First")
    first, etag = receipt(api.request("POST", headers=save_headers(source, name="Room.docx"), body=source), 201)
    file = f'{ROUTE}/{first["id"]}'
    room = f'/api/cloud/rooms/{first["id"]}'
    activate = {"file_revision": first["revision"]}
    expect(api.request("GET", room, owner=None), 401, "authentication_required")
    expect(api.request("GET", room), 404, "room_not_found")
    expect(post(api, room, activate, owner=OTHER), 404, "room_not_found")
    expect(post(api, room, activate, headers={"Origin": "https://elsewhere.invalid"}), 403, "invalid_origin")
    expect(post(api, room, {"file_revision": first["revision"] + 1}), 412, "stale_revision")

    release = Event()
    pending_docx = docx("Legacy update")
    pending = save_headers(pending_docx, etag=etag)
    with ThreadPoolExecutor(max_workers=1) as pool:
        upload = pool.submit(api.request, "PUT", file, headers=pending, body=held_body(pending_docx, release))
        try:
            wait_for_save(db, pending["Idempotency-Key"])
            metadata = payload(post(api, room, activate), 201)
            assert metadata == {"id": first["id"], "file_revision": first["revision"],
                                "published_room_revision": 0, "checkpoint_revision": 0, "head": 0, "role": "owner"}, metadata
        finally:
            release.set()
        expect(upload.result(), 409, "room_required")
    expect(api.request("PUT", file, headers=pending, body=pending_docx), 409, "room_required")
    expect(api.request("PUT", file, headers=save_headers(pending_docx, etag=etag), body=pending_docx), 409, "room_required")
    assert payload(api.request("GET", room)) == metadata
    assert payload(post(api, room, activate)) == metadata
    for suffix in ("", "/checkpoint", "/changes"):
        expect(api.request("GET", room + suffix, owner=OTHER), 404, "room_not_found")
    download(api, file, etag, source)
    checkpoint = expect(api.request("GET", room + "/checkpoint"), 200)[1]
    with ZipFile(BytesIO(checkpoint)) as archive:
        assert b"First" in archive.read("word/document.xml")
    return file, etag, room, checkpoint


def edit(base, text, offset=1):
    return {
        "operation_id": str(uuid4()), "base_revision": base,
        "change": {"edits": [{
            "container": {"story": "body"}, "id": "0000000000000001",
            "chars": {
                "at": offset, "removed": 0, "hash": "656951ce3915a1de",
                "inserted": {"text": text, "runs": [{"len": len(text), "props": {}}]},
            },
        }]},
    }


def concurrent_edits(api, room):
    changes = room + "/changes"
    a, b = edit(0, "A"), edit(0, "B")
    expect(post(api, changes, a, owner=OTHER), 404, "room_not_found")
    expect(post(api, changes, {**a, "base_revision": 1}), 409, "stale_room_revision")
    expect(post(api, changes, {**a, "change": {}}), 400, "invalid_body")
    expect(post(api, changes, {**a, "change": {"oversized": "x" * (256 * 1024)}}), 413, "body_too_large")
    with ThreadPoolExecutor(max_workers=8) as pool:
        repeated = list(pool.map(lambda _: payload(post(api, changes, a)), range(8)))
    assert repeated == [{"revision": 1, "head": 1}] * 8, repeated
    expect(post(api, changes, {**b, "operation_id": a["operation_id"]}), 409, "operation_mismatch")
    # The later edit starts at the old base. The model maps its insertion through A.
    assert payload(post(api, changes, b)) == {"revision": 2, "head": 2}
    assert payload(post(api, changes, a)) == repeated[0]
    page = payload(api.request("GET", changes + "?after=0"))
    assert page["head"] == 2 and page["next_after"] is None, page
    assert [row["operation_id"] for row in page["changes"]] == [a["operation_id"], b["operation_id"]], page
    text = "First"
    for entry in page["changes"]:
        chars = entry["change"]["edits"][0]["chars"]
        assert chars["removed"] == 0, entry
        offset = chars["at"]
        text = text[:offset] + chars["inserted"]["text"] + text[offset:]
    assert text == "FABirst", text
    print("Passed real ChangeSet validation, concurrent duplicates, stale rebase, and ordered replay", flush=True)
    return a, page


def resumed(api, db, room, checkpoint, first, page):
    changes = room + "/changes"
    assert expect(api.request("GET", room + "/checkpoint"), 200)[1] == checkpoint
    assert payload(api.request("GET", changes + "?after=0")) == page
    assert payload(post(api, changes, first)) == {"revision": 1, "head": 1}
    assert payload(post(api, changes, edit(2, "C"))) == {"revision": 3, "head": 3}
    assert payload(api.request("GET", changes + "?after=0&through=2")) == page
    assert db.execute("SELECT COUNT(*) FROM cloud_room_changes").fetchone() == (3,)
    print("Passed unchanged checkpoint, exact durable receipt, and fixed-head replay after restarting the Worker", flush=True)


def atomic_append_and_pages(api, db, room):
    changes = room + "/changes"
    next_edit = edit(3, "D")
    with db:
        db.execute("""CREATE TRIGGER reject_test_head BEFORE UPDATE OF head ON cloud_rooms
                      BEGIN SELECT RAISE(ABORT, 'simulated head failure'); END""")
    try:
        expect(post(api, changes, next_edit), 503, "storage_unavailable")
        assert db.execute("SELECT head FROM cloud_rooms").fetchone() == (3,)
        assert db.execute("SELECT COUNT(*) FROM cloud_room_changes").fetchone() == (3,)
    finally:
        with db:
            db.execute("DROP TRIGGER reject_test_head")
    assert payload(post(api, changes, next_edit)) == {"revision": 4, "head": 4}

    for base in range(4, 33):
        assert payload(post(api, changes, edit(base, "X")))["revision"] == base + 1
    first = payload(api.request("GET", changes + "?after=0"))
    assert (first["head"], first["next_after"]) == (33, 32), first
    assert [row["revision"] for row in first["changes"]] == list(range(1, 33))
    assert payload(post(api, changes, edit(33, "Y")))["revision"] == 34
    final = payload(api.request("GET", changes + "?after=32&through=33"))
    assert (final["head"], final["next_after"]) == (33, None), final
    assert [row["revision"] for row in final["changes"]] == [33]
    print("Passed atomic rollback and ordered pagination through a fixed head", flush=True)


def delete_room(api, db, file, etag, room, read_object):
    key, = db.execute("SELECT checkpoint_key FROM cloud_rooms").fetchone()
    assert read_object(key) is not None
    expect(api.request("DELETE", file, headers={"If-Match": etag}), 204)
    for suffix in ("", "/checkpoint", "/changes"):
        expect(api.request("GET", room + suffix), 404, "room_not_found")
    expect(post(api, room, {"file_revision": 1}), 404, "room_not_found")
    expect(post(api, room + "/changes", edit(34, "D")), 404, "room_not_found")
    assert read_object(key) is None, "checkpoint remains in R2"
    assert db.execute("SELECT COUNT(*) FROM cloud_room_changes").fetchone() == (0,)
    print("Passed tombstone denial and checkpoint and journal cleanup", flush=True)


def run_local(args, root):
    with local_storage(args, root) as (api, cli, local, state, sandbox, read_object):
        with running_worker(api, cli, local, sandbox), closing(local_db(state)) as db:
            file, etag, room, checkpoint = activate(api, db)
            print("Passed owner authorization, exact revision activation, and the held legacy PUT fence", flush=True)
            first, page = concurrent_edits(api, room)
        with running_worker(api, cli, local, sandbox), closing(local_db(state)) as db:
            resumed(api, db, room, checkpoint, first, page)
            atomic_append_and_pages(api, db, room)
            delete_room(api, db, file, etag, room, read_object)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("worker", type=Path)
    parser.add_argument("--wrangler", default="wrangler")
    run_local(parser.parse_args(), Path(__file__).resolve().parents[3])
