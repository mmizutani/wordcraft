#!/usr/bin/env python3
"""Verify checkpoint compaction and DOCX fidelity in local Wrangler D1 and R2."""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from io import BytesIO
import json
from pathlib import Path
from random import Random
from time import monotonic
from uuid import uuid4
from xml.etree import ElementTree
from zipfile import ZIP_DEFLATED, ZipFile

from cloud_documents import ROUTE, download, expect, local_db, local_storage, receipt, running_worker, save_headers
from hosted_room import docx, edit, payload, post
from room_save import save, saved

SOFT_COUNT = 1024
W = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}"
BLOCK_ID = "{urn:wordcraft:docx:2026}blockId"


def fixture(extra_paragraphs):
    with ZipFile(BytesIO(docx("First"))) as source:
        parts = {name: source.read(name) for name in source.namelist()}
    rng = Random(0)
    words = "policy reviewer discussion change implementation proposal final section decision scope period".split()
    paragraphs = [
        f"<w:p><w:r><w:t>Appendix {index:04d} {' '.join(rng.choices(words, k=64))}</w:t></w:r></w:p>"
        for index in range(extra_paragraphs)
    ]
    tracked = '<w:p><w:ins w:id="4" w:author="Test"><w:r><w:t>Tracked</w:t></w:r></w:ins></w:p>'
    parts["word/document.xml"] = (
        f'<w:document xmlns:w="{W[1:-1]}"><w:body><w:p><w:r><w:t>First</w:t></w:r></w:p>'
        f"{tracked}{''.join(paragraphs)}</w:body></w:document>"
    ).encode()
    parts["word/settings.xml"] = (
        f'<w:settings xmlns:w="{W[1:-1]}"><w:docVars>'
        '<w:docVar w:name="RoomFixture" w:val="original"/></w:docVars></w:settings>'
    ).encode()
    parts["word/_rels/document.xml.rels"] = b"""<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/>
</Relationships>"""
    parts["[Content_Types].xml"] = parts["[Content_Types].xml"].replace(
        b"</Types>", b'<Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/></Types>',
    )
    output = BytesIO()
    with ZipFile(output, "w", ZIP_DEFLATED) as archive:
        for name, data in parts.items():
            archive.writestr(name, data)
    return output.getvalue(), len(parts["word/document.xml"])


def check_docx(data, extra_paragraphs, first_text, ids=None):
    with ZipFile(BytesIO(data)) as archive:
        document = ElementTree.fromstring(archive.read("word/document.xml"))
        settings = ElementTree.fromstring(archive.read("word/settings.xml"))
    paragraphs = document.findall(f"{W}body/{W}p")
    text = ["".join(node.text or "" for node in p.iter(W + "t")) for p in paragraphs]
    assert len(text) == extra_paragraphs + 2 and text[:2] == [first_text, "Tracked"], (len(text), text[:2])
    if extra_paragraphs:
        assert text[-1].startswith(f"Appendix {extra_paragraphs - 1:04d} "), text[-1]
    tracked = paragraphs[1].find(W + "ins")
    assert tracked is not None and tracked.get(W + "author") == "Test"
    doc_var = settings.find(f"{W}docVars/{W}docVar")
    assert doc_var is not None and doc_var.attrib == {W + "name": "RoomFixture", W + "val": "original"}
    observed_ids = [paragraph.get(BLOCK_ID) for paragraph in paragraphs]
    assert all(observed_ids) and len(set(observed_ids)) == len(paragraphs)
    if ids is not None:
        assert observed_ids == ids, "checkpoint replaced stable paragraph IDs"
    return observed_ids


def get_checkpoint(api, room, revision):
    headers, data = expect(api.request("GET", room + "/checkpoint"), 200)
    assert headers["etag"] == f'"{revision}"', headers
    return data


def insert(base, text, block_id):
    submission = edit(base, text)
    submission["change"]["edits"][0]["id"] = block_id
    return submission


def seed_tail(db, room, block_id):
    document = room.rsplit("/", 1)[1]
    change = json.dumps(insert(1, "X", block_id)["change"], separators=(",", ":"))
    # Each insertion is valid at the same offset. Seed only the intervening journal
    # to avoid hundreds of HTTP calls that would each reconstruct the whole room.
    with db:
        db.execute(
            """WITH RECURSIVE revisions(rev) AS (
                 SELECT 2 UNION ALL SELECT rev + 1 FROM revisions WHERE rev < ?
               )
               INSERT INTO cloud_room_changes
                 (owner, document_id, revision, actor, operation_id, fingerprint, change_json, change_bytes)
               SELECT r.owner, r.document_id, rev, r.owner, lower(hex(randomblob(16))), ?, ?, ?
               FROM cloud_rooms r, revisions WHERE r.document_id = ?""",
            (SOFT_COUNT - 1, "0" * 64, change, len(change), document),
        )
        db.execute(
            """UPDATE cloud_rooms SET head = ?, tail_bytes = (
                 SELECT SUM(change_bytes) FROM cloud_room_changes WHERE document_id = ?
               ) WHERE document_id = ?""",
            (SOFT_COUNT - 1, document, document),
        )


def submit(api, changes, submission, revision):
    assert payload(post(api, changes, submission)) == {"revision": revision, "head": revision}


def reject_publication(api, db, room, first, checkpoint, crossing):
    changes = room + "/changes"
    with db:
        db.execute("""CREATE TRIGGER reject_test_compaction BEFORE INSERT ON cloud_room_identities
                      BEGIN SELECT RAISE(ABORT, 'simulated compaction failure'); END""")
    try:
        submit(api, changes, crossing, SOFT_COUNT)
        metadata = payload(api.request("GET", room))
        assert (metadata["checkpoint_revision"], metadata["head"]) == (0, SOFT_COUNT), metadata
        assert get_checkpoint(api, room, 0) == checkpoint
        submit(api, changes, first, 1)
        last = payload(api.request("GET", changes + f"?after={SOFT_COUNT - 1}"))
        assert [entry["operation_id"] for entry in last["changes"]] == [crossing["operation_id"]], last
    finally:
        with db:
            db.execute("DROP TRIGGER reject_test_compaction")
    print("Passed D1 compaction rollback and retained replay of an acknowledged edit", flush=True)


def compact(api, db, extra_paragraphs, publication_failure, rebased_trigger, read_object):
    source, expanded_size = fixture(extra_paragraphs)
    item, etag = receipt(api.request("POST", headers=save_headers(source, name="Compaction fixture.docx"), body=source), 201)
    file = f'{ROUTE}/{item["id"]}'
    room = f'/api/cloud/rooms/{item["id"]}'
    changes = room + "/changes"
    started = monotonic()
    expect(post(api, room, {"file_revision": 1}), 201)
    activation = monotonic() - started
    previous = get_checkpoint(api, room, 0)
    previous_key, = db.execute(
        "SELECT checkpoint_key FROM cloud_rooms WHERE document_id = ?", (item["id"],),
    ).fetchone()
    ids = check_docx(previous, extra_paragraphs, "First")
    first = insert(0, "A", ids[0])
    submit(api, changes, first, 1)
    seed_tail(db, room, ids[0])
    assert payload(api.request("GET", room))["checkpoint_revision"] == 0

    head = SOFT_COUNT
    trigger_text = "B"
    crossing = insert(head - 1, trigger_text, ids[0])
    checkpoint_text = "F" + "X" * (SOFT_COUNT - 2) + "Airst"
    if publication_failure:
        reject_publication(api, db, room, first, previous, crossing)
        head += 1
        checkpoint_text = "FB" + checkpoint_text[1:]
        trigger_text = "D"
        crossing = insert(head - 1, trigger_text, ids[0])
    if rebased_trigger:
        stale = insert(head - 2, "R", ids[0])
        stale["change"]["edits"][0]["chars"]["at"] = 0
        submit(api, changes, stale, head)
        page = payload(api.request("GET", changes + f"?after={head - 2}"))
        assert [entry["revision"] for entry in page["changes"]] == [head - 1, head], page
        assert page["changes"][-1]["operation_id"] == stale["operation_id"], page
        assert payload(api.request("GET", room))["checkpoint_revision"] == 0
        head += 1
        checkpoint_text = "R" + checkpoint_text
        crossing = insert(head - 1, trigger_text, ids[0])
    floor = head - 1

    started = monotonic()
    with ThreadPoolExecutor(max_workers=2) as pool:
        responses = list(pool.map(lambda _: post(api, changes, crossing), range(2)))
    compacted = monotonic() - started
    submit(api, changes, crossing, head)
    assert [payload(response) for response in responses] == [{"revision": head, "head": head}] * 2

    metadata = payload(api.request("GET", room))
    assert metadata["checkpoint_revision"] == floor, ("checkpoint was not published", floor, metadata)
    assert (metadata["head"], metadata["file_revision"], metadata["published_room_revision"]) == (head, 1, 0)
    expect(api.request("GET", changes + "?after=0"), 409, "stale_room_revision")
    expect(post(api, changes, insert(0, "E", ids[0])), 409, "stale_room_revision")
    checkpoint = get_checkpoint(api, room, floor)
    assert read_object(previous_key) == previous
    check_docx(checkpoint, extra_paragraphs, checkpoint_text, ids)
    tail = payload(api.request("GET", changes + f"?after={floor}"))
    assert tail["head"] == head and tail["next_after"] is None, tail
    assert [(entry["revision"], entry["operation_id"]) for entry in tail["changes"]] == [
        (head, crossing["operation_id"]),
    ], tail
    tail_bytes, = db.execute(
        "SELECT change_bytes FROM cloud_room_changes WHERE document_id = ? AND revision = ?",
        (item["id"], head),
    ).fetchone()
    assert db.execute(
        "SELECT head, tail_bytes FROM cloud_rooms WHERE document_id = ?", (item["id"],),
    ).fetchone() == (head, tail_bytes)
    assert db.execute(
        "SELECT COUNT(*) FROM cloud_room_changes WHERE document_id = ?", (item["id"],),
    ).fetchone() == (1,)
    assert db.execute(
        "SELECT COUNT(*) FROM cloud_room_identities WHERE document_id = ?", (item["id"],),
    ).fetchone() == (floor,)
    expect(post(api, changes, first), 409, "receipt_expired")
    expect(post(api, changes, {**insert(0, "D", ids[0]), "operation_id": first["operation_id"]}), 409, "operation_mismatch")
    download(api, file, etag, source)
    print(f"{expanded_size:,} bytes document XML ({len(source):,} compressed DOCX): "
          f"activation {activation:.3f}s, append and compaction {compacted:.3f}s", flush=True)
    current_text = checkpoint_text[:1] + trigger_text + checkpoint_text[1:]
    return file, room, checkpoint, current_text, ids, first, head


def reopen_and_save(api, state):
    file, room, checkpoint, current_text, ids, first, head = state
    changes = room + "/changes"
    assert get_checkpoint(api, room, head - 1) == checkpoint
    expect(post(api, changes, first), 409, "receipt_expired")

    pending = insert(head, "C", ids[0])
    with ThreadPoolExecutor(max_workers=2) as pool:
        responses = list(pool.map(lambda _: payload(post(api, changes, pending)), range(2)))
    assert responses == [{"revision": head + 1, "head": head + 1}] * 2, responses
    tail = payload(api.request("GET", changes + f"?after={head}"))
    assert tail["head"] == head + 1 and tail["next_after"] is None, tail
    assert [entry["operation_id"] for entry in tail["changes"]] == [pending["operation_id"]], tail

    publish = save(head + 1)
    started = monotonic()
    assert payload(post(api, room + "/save", publish)) == saved(2, head + 1)
    elapsed = monotonic() - started
    print(f"Save after checkpoint replay {elapsed:.3f}s", flush=True)
    return file, room, publish, current_text[:1] + "C" + current_text[1:], ids, head


def verify_save(api, state, extra_paragraphs):
    file, room, publish, text, ids, head = state
    assert payload(post(api, room + "/save", publish)) == saved(2, head + 1)
    metadata = payload(api.request("GET", room))
    assert (metadata["head"], metadata["checkpoint_revision"], metadata["published_room_revision"]) == (
        head + 1, head - 1, head + 1,
    ), metadata
    headers, data = expect(api.request("GET", file), 200)
    assert headers["etag"] == '"2"', headers
    check_docx(data, extra_paragraphs, text, ids)


def run_local(args, root):
    with local_storage(args, root) as (api, cli, local, state, sandbox, read_object):
        with running_worker(api, cli, local, sandbox), closing(local_db(state)) as db:
            compacted = compact(api, db, args.extra_paragraphs, args.publication_failure, args.rebased_trigger, read_object)
        with running_worker(api, cli, local, sandbox):
            published = reopen_and_save(api, compacted)
        with running_worker(api, cli, local, sandbox):
            verify_save(api, published, args.extra_paragraphs)
    print("Passed compaction, replay cursor recovery, duplicate protection, and ordered Save across restarts", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("worker", type=Path)
    parser.add_argument("--wrangler", default="wrangler")
    parser.add_argument("--extra-paragraphs", type=int, default=24,
                        help="increase the synthetic fixture's expanded DOCX size for local timing")
    case = parser.add_mutually_exclusive_group()
    case.add_argument("--publication-failure", action="store_true")
    case.add_argument("--rebased-trigger", action="store_true")
    run_local(parser.parse_args(), Path(__file__).resolve().parents[3])
