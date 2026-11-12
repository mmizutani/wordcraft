#!/usr/bin/env python3
"""Verify ordered room Save, recovery, and deletion in local Wrangler D1 and R2."""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from io import BytesIO
import json
from pathlib import Path
from uuid import uuid4
from xml.etree import ElementTree
from zipfile import ZipFile

from cloud_documents import OTHER, OWNER, ROUTE, expect, local_db, local_storage, receipt, running_worker, save_headers, wait_for_save
from hosted_room import docx, edit, payload, post
from room_access import json_request


def save(revision):
    return {"operation_id": str(uuid4()), "room_revision": revision}


def saved(file_revision, room_revision):
    return {"file_revision": file_revision, "room_revision": room_revision}


def text_of_docx(data):
    with ZipFile(BytesIO(data)) as archive:
        xml = ElementTree.fromstring(archive.read("word/document.xml"))
    return "".join(node.text or "" for node in xml.iter("{http://schemas.openxmlformats.org/wordprocessingml/2006/main}t"))


def verify_file(api, file, revision, text):
    headers, data = expect(api.request("GET", file), 200)
    assert headers["etag"] == f'"{revision}"', headers
    assert text_of_docx(data) == text, text_of_docx(data)


def setup(api):
    data = docx("First")
    item, _ = receipt(api.request("POST", headers=save_headers(data, name="Save fixture.docx"), body=data), 201)
    file = f'{ROUTE}/{item["id"]}'
    room = f'/api/cloud/rooms/{item["id"]}'
    expect(post(api, room, {"file_revision": 1}), 201)
    request = payload(post(api, room + "/access", {}, owner=OTHER))
    grant = room + "/grants/" + request["request_code"]
    for role in ("editor", "viewer"):
        assert payload(json_request(api, "PUT", grant, data={"role": role}))["role"] == role
        expect(post(api, room + "/save", save(0), owner=OTHER), 403, "room_owner_required")
    return file, room


def submit(api, room, base, text):
    assert payload(post(api, room + "/changes", edit(base, text)))["revision"] == base + 1


def commit_without_receipt(api, db, room, request):
    connection = api.connection()
    try:
        connection.request("POST", room + "/save", json.dumps(request).encode(), {
            "Origin": api.origin, "oai-authenticated-user-id": OWNER, "Content-Type": "application/json",
        })
        wait_for_save(db, request["operation_id"], "committed")
    finally:
        connection.close()


def interrupt_publication(api, db, room, request):
    with db:
        db.execute("""CREATE TRIGGER reject_test_save BEFORE UPDATE OF published_room_revision ON cloud_rooms
                      BEGIN SELECT RAISE(ABORT, 'simulated room publication failure'); END""")
    try:
        expect(post(api, room + "/save", request), 503, "storage_unavailable")
        return wait_for_save(db, request["operation_id"])
    finally:
        with db:
            db.execute("DROP TRIGGER reject_test_save")


def expire(db, request):
    with db:
        db.execute("UPDATE cloud_saves SET lease_until = 0 WHERE owner = ? AND operation_id = ?",
                   (OWNER, request["operation_id"]))


def ordered_saves(api, db, file, room, initial, read_object):
    path = room + "/save"
    assert payload(post(api, path, initial)) == saved(2, 1)
    verify_file(api, file, 2, "FAirst")
    state = payload(api.request("GET", room))
    assert (state["head"], state["published_room_revision"], state["file_revision"]) == (2, 1, 2), state
    tail = payload(api.request("GET", room + "/changes?after=1"))
    assert tail["head"] == 2 and tail["changes"][0]["change"]["edits"][0]["chars"]["inserted"]["text"] == "B", tail
    expect(post(api, path, {**initial, "room_revision": 2}), 409, "operation_mismatch")
    expect(post(api, path, save(3)), 409, "stale_room_revision")

    delayed = save(1)
    delayed_key = interrupt_publication(api, db, room, delayed)
    assert text_of_docx(read_object(delayed_key)) == "FAirst"
    verify_file(api, file, 2, "FAirst")
    assert payload(api.request("GET", room))["published_room_revision"] == 1
    expect(post(api, path, delayed), 202, "operation_pending")

    assert payload(post(api, path, save(2))) == saved(3, 2)
    verify_file(api, file, 3, "FBAirst")
    expire(db, delayed)
    expect(post(api, path, delayed), 409, "stale_room_revision")
    expect(post(api, path, save(1)), 409, "stale_room_revision")
    assert read_object(delayed_key) is None
    assert payload(post(api, path, initial)) == saved(2, 1)
    verify_file(api, file, 3, "FBAirst")
    print("Passed exact historical publication, restart receipt, atomic rollback, and delayed stale Save rejection", flush=True)


def concurrent_and_delete(api, db, file, room, read_object):
    path = room + "/save"
    submit(api, room, 2, "C")
    submit(api, room, 3, "D")
    delayed_higher = save(4)
    interrupt_publication(api, db, room, delayed_higher)
    assert payload(post(api, path, save(3))) == saved(4, 3)
    expire(db, delayed_higher)
    assert payload(post(api, path, delayed_higher)) == saved(5, 4)
    verify_file(api, file, 5, "FDCBAirst")

    submit(api, room, 4, "E")
    submit(api, room, 5, "F")
    requests = (save(5), save(6))
    with ThreadPoolExecutor(max_workers=2) as pool:
        lower, higher = pool.map(lambda request: post(api, path, request), requests)
    latest = payload(higher)
    assert latest["room_revision"] == 6, latest
    if lower[0] == 200:
        assert payload(lower)["file_revision"] < latest["file_revision"]
    else:
        expect(lower, 409, "stale_room_revision")
    verify_file(api, file, latest["file_revision"], "FFEDCBAirst")
    assert payload(post(api, path, requests[1])) == latest
    submit(api, room, 6, "G")
    pending = save(7)
    candidate = interrupt_publication(api, db, room, pending)
    assert text_of_docx(read_object(candidate)) == "FGFEDCBAirst"
    expect(api.request("DELETE", file, headers={"If-Match": f'"{latest["file_revision"]}"'}), 204)
    assert read_object(candidate) in (None, b"")
    expect(post(api, path, pending), 404, "room_not_found")
    expect(post(api, path, requests[1]), 404, "room_not_found")
    print("Passed concurrent ordered saves and deletion fencing of an uploaded candidate", flush=True)


def run_local(args, root):
    with local_storage(args, root) as (api, cli, local, state, sandbox, read_object):
        with running_worker(api, cli, local, sandbox), closing(local_db(state)) as db:
            file, room = setup(api)
            submit(api, room, 0, "A")
            submit(api, room, 1, "B")
            initial = save(1)
            commit_without_receipt(api, db, room, initial)
        with running_worker(api, cli, local, sandbox), closing(local_db(state)) as db:
            ordered_saves(api, db, file, room, initial, read_object)
            concurrent_and_delete(api, db, file, room, read_object)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("worker", type=Path)
    parser.add_argument("--wrangler", default="wrangler")
    run_local(parser.parse_args(), Path(__file__).resolve().parents[3])
