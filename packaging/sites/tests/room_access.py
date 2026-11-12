#!/usr/bin/env python3
"""Exercise the hosted document access-request and revocation path in local workerd."""

import argparse
import json
from pathlib import Path
from uuid import uuid4

from cloud_documents import OTHER, OWNER, expect, local_storage, receipt, running_worker, save_headers


from hosted_room import docx, edit

def json_request(api, method, path, *, owner=OWNER, data=None, identity=None):
    headers = {"Content-Type": "application/json", **(identity or {})}
    body = json.dumps(data).encode() if data is not None else None
    return api.request(method, path, owner=owner, headers=headers, body=body)


def read(response, status=200):
    return json.loads(expect(response, status)[1])


def verify(api):
    data = docx("First")
    item, _ = receipt(api.request("POST", headers=save_headers(data, name="Access fixture.docx"), body=data), 201)
    room = f"/api/cloud/rooms/{item['id']}"
    expect(json_request(api, "POST", room + "/access", owner=OTHER), 404)
    activation = json_request(api, "POST", room, data={"file_revision": item["revision"]})
    assert activation[0] in (200, 201), (activation[0], activation[2][:200])

    expect(api.request("GET", f"/api/cloud/documents/{item['id']}?open=1"), 409, "room_required")
    expect(json_request(api, "GET", room, owner=OTHER), 404)
    expect(json_request(api, "POST", room + "/access", owner=None), 401)
    request = read(json_request(
        api, "POST", room + "/access", owner=OTHER,
        identity={"oai-authenticated-user-full-name": "Synthetic%20Tester",
                  "oai-authenticated-user-full-name-encoding": "percent-encoded-utf-8"},
    ))
    assert set(request) == {"state", "request_code"} and request["state"] == "pending", request
    assert read(json_request(api, "POST", room + "/access", owner=OTHER)) == request
    expect(json_request(api, "GET", room + "/grants", owner=OTHER), 404)
    grants = read(json_request(api, "GET", room + "/grants"))["grants"]
    assert grants == [{"id": request["request_code"], "display_name": "Synthetic Tester", "role": "pending"}], grants

    grant = room + "/grants/" + request["request_code"]
    expect(json_request(api, "PUT", grant, owner=OTHER, data={"role": "editor"}), 404)
    assert read(json_request(api, "PUT", grant, data={"role": "viewer"}))["role"] == "viewer"
    assert read(json_request(api, "GET", room + "/access", owner=OTHER))["state"] == "viewer"
    expect(json_request(api, "GET", room + "/checkpoint", owner=OTHER), 200)
    expect(json_request(api, "POST", room + "/changes", owner=OTHER, data=edit(0, "V")), 403, "room_read_only")
    heartbeat = {"session_id": str(uuid4()), "revision": 0, "position": {"anchor": None, "focus": None}}
    expect(json_request(api, "POST", room + "/presence", owner=OTHER, data=heartbeat), 204)
    participants = read(json_request(api, "GET", room + "/presence"))["participants"]
    assert participants == [{"session_id": heartbeat["session_id"], "revision": 0, "role": "viewer",
                             "display_name": "Synthetic Tester", "position": heartbeat["position"]}], participants

    assert read(json_request(api, "PUT", grant, data={"role": "editor"}))["role"] == "editor"
    assert read(json_request(api, "GET", room + "/access", owner=OTHER))["state"] == "editor"
    editor_edit = edit(0, "E")
    assert read(json_request(api, "POST", room + "/changes", owner=OTHER, data=editor_edit)) == {"revision": 1, "head": 1}
    page = read(json_request(api, "GET", room + "/changes?after=0", owner=OTHER))
    assert page["role"] == "editor" and page["changes"][0]["operation_id"] == editor_edit["operation_id"]
    expect(json_request(api, "POST", room + "/changes", data=editor_edit), 409, "operation_mismatch")
    assert read(json_request(api, "PUT", grant, data={"role": "revoked"}))["role"] == "revoked"
    expect(json_request(api, "GET", room + "/checkpoint", owner=OTHER), 404)
    expect(json_request(api, "GET", room + "/changes?after=0", owner=OTHER), 404)
    expect(json_request(api, "POST", room + "/changes", owner=OTHER, data=editor_edit), 404)
    expect(json_request(api, "GET", room + "/presence", owner=OTHER), 404)
    assert read(json_request(api, "GET", room + "/presence"))["participants"] == []
    assert read(json_request(api, "POST", room + "/access", owner=OTHER))["state"] == "revoked"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("worker", type=Path)
    parser.add_argument("--wrangler", default="wrangler")
    args = parser.parse_args()
    with local_storage(args, Path(__file__).resolve().parents[3]) as (api, cli, local, _, sandbox, _):
        with running_worker(api, cli, local, sandbox):
            verify(api)
    print("Room access request, role change and revocation passed")
