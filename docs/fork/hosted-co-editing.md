# Hosted browser co-editing

[Issue #115](https://github.com/mmizutani/wordcraft/issues/115) covers browser-to-browser editing on the private Sites pilot. An owner shares a cloud document with named testers, two editors can edit at the same time, and a viewer can follow the room. Accepted changes survive process loss. The owner's explicit Save publishes a DOCX. [Cloud documents](cloud-documents.md) governs the file library and [the room design](../collaboration.md) governs change application, rebasing, roles, and presence.

## Editor workflow

| Action | Behavior |
| --- | --- |
| Share a cloud document | The owner enables sharing and copies the document link from File › Share. Enabling sharing captures the current published file revision as the room's starting point. Resolve any unsaved local edits before starting the room. A document whose protection the Worker cannot preserve stays in the solo cloud workflow. |
| Request access | A visitor admitted to the Site can use the link to request access. Pending visitors see only the request state. WordCraft shows the owner the authenticated visitor's display identity in the document's pending requests. |
| Grant or change access | The owner selects Editor or Viewer for the requesting account. Store the grant against the stable Site-scoped user ID. A change of display name or email does not transfer access. |
| Edit together | The owner and editors type in their local replicas. Show named participants and positions from the room. A viewer has read-only document commands. Unconfirmed edits are visible to their author and survive transient disconnects while the tab remains open. |
| Reconnect | Fetch changes after the last confirmed room revision. Replay an uncertain submission under its original operation ID. Rebase pending work using the existing room rules. Keep a recovery copy and explain when a command cannot be safely reapplied. |
| Revoke access | The owner removes the grant. The server stops authorizing subsequent requests, including requests on a previously opened room. The browser leaves the room when it learns access was revoked. A response already delivered to a client cannot be recalled. |
| Save | The owner publishes the confirmed room revision selected by Save. Show which revision was saved and keep later edits pending for the next Save. An editor's acknowledged edits remain in the room whether or not the owner has published a new DOCX. |

Site access and document access are separate checks. A WordCraft link carries an opaque document ID. Requesting access reveals no document title, contents, room activity, or members. The Worker uses the dispatcher-authenticated identity on each request. Use the documented optional authenticated profile name and the authenticated email for display to the owner. If neither is present, ask the owner to verify the account using a request code shown to the signed-in requester. The grant always stores the Site-scoped user ID. [Sites identity](https://learn.chatgpt.com/docs/sites/data-and-connections)

| Operation | Required authority |
| --- | --- |
| Request access or read its status | An admitted, signed-in Site visitor, limited to their own request. Bound and rate-limit requests. |
| List requests, grant or change roles, revoke, enable sharing, Save, delete | The cloud document owner. |
| Open the room, fetch any replay page, read presence, subscribe to notifications | Owner, Editor, or Viewer with a current grant. |
| Update presence | Owner, Editor, or Viewer with a current grant, for their own room session. |
| Submit a change | Owner or Editor with a current grant. |

Authorize each operation on the server. Condition state changes on the current grant version. If push is used, check authorization at subscription and before sending a notification.

## Room and file authority

The browser keeps a local copy and sends the Rust document model's versioned `ChangeSet` payloads. The server runs the existing apply and rebase rules against the authoritative document, checks role and payload bounds, and proposes one next change per operation. The durable receipt identifies the single accepted revision. Use those rules behind a storage boundary so the local in-process room and a future network client can share their semantics. The current `Replica` calls an in-process `Room`. A network adapter must preserve its one-in-flight and confirmed-versus-pending state without blocking the WASM UI. [Hub](../../crates/engine/src/room/hub.rs) · [Replica](../../crates/engine/src/room/replica.rs) · [ChangeSet](../../crates/doc/src/change.rs)

| Store | Authoritative records |
| --- | --- |
| D1 | Document grants and their versions, room head, checkpoint pointer, accepted changes, operation receipts, and the room revision included in the published DOCX. |
| R2 | Immutable room checkpoints and published DOCX objects. Use private, server-generated keys and verify bytes before publishing a pointer. |
| Browser | Current document replica, last confirmed revision, and bounded pending edits for the open tab. |
| Presence | Short-lived, rate-limited position and selection updates scoped to a room session and a confirmed revision. Presence expires after disconnect and is independent of the edit journal. |

An activation transaction compares the current Phase 2 file revision with the revision used to build the initial checkpoint. It marks the room active only after the checkpoint is available. The legacy file upload can race with activation, so its publish transaction also checks the active-room flag. An active room remains the write authority when all participants leave. Cloud Open and the share link both open the active room at a consistent head using its checkpoint and ordered journal. The separate file download returns the last published DOCX. The owner can still open and save the room. Delete tombstones the document, closes its room, and fences pending file and checkpoint publication before cleanup.

Room revisions order edits. File revisions identify committed DOCX artifacts. A file revision records the room revision it includes. An owner Save waits for the owner's pending submissions, chooses a confirmed room revision, and reserves a Save operation. The room authority serializes the DOCX from that exact state. It uploads through the existing R2 candidate flow and conditionally publishes the pointer and receipt in D1. A delayed save cannot advance the file pointer to an earlier room revision. Later edits may commit while a DOCX is generated. They remain in the journal and are visible on the next open. The direct Phase 2 upload path returns a room-required result for an active document, including an upload that was reserved before activation.

Room checkpoints must reconstruct stable block IDs, tracked changes, and the source XML the DOCX writer needs to preserve. A plain serialization of the document model is insufficient for this contract. Verify the chosen checkpoint representation with imported DOCX files that lack WordCraft IDs and contain retained OOXML. Keep the previous checkpoint and required journal range until the replacement pointer commits and replay is proven from the new checkpoint. An expired replay cursor returns a checkpoint and a new cursor. Keep a compact identity and fingerprint for accepted operations after pruning their full receipts. A retry whose receipt expired returns a recovery result and cannot reapply the edit. Use initial pilot bounds of 256 KiB per serialized change, 32 pending operations or 2 MiB per browser replica, and 32 operations or 512 KiB per catch-up page. Trigger a checkpoint after 1,024 accepted changes or 16 MiB of new journal data. Checkpoint through the revision before the triggering edit so its submitting browser can still replay the accepted change. A rebased submission defers this checkpoint to the next edit that starts at the current head, preserving the submitting browser’s replay window. A browser with no unsynchronized edits reloads an expired checkpoint in place. When local edits need recovery, it preserves them for download and explicit reopen. Cap the live tail at 2,048 changes or 32 MiB and keep up to 100,000 accepted operation identities for a live room. The pilot pauses accepting changes at a storage cap until a checkpoint or explicit recovery can make progress. Oversized edits stay available for local recovery and are refused before acknowledgment. Verify the bounds with representative DOCX fixtures and revise them in the implementation review if measurements require it.

## Commit, replay, and transport

1. Authorize the signed-in principal against the document grant and take a consistent read of the checkpoint and room head.
2. Validate and rebase the submitted operation against that state. A duplicate operation ID for the same room and principal returns the original receipt if the request fingerprint matches. Reuse with different content is rejected.
3. In one D1 transaction, confirm the room and grant are current, append the canonical accepted change, advance the head, and write its receipt. A compare-and-set miss changes no journal or receipt rows. On a head race, recompute against the new head. On uncertain commit, read the receipt before reporting an outcome.
4. Acknowledge only the durable receipt. Sync reads return ordered changes through a captured head, so a later append cannot shift a page boundary. Recheck the grant for each page. The browser applies remote changes and acknowledges its own by operation ID.
5. Use bounded same-origin HTTPS submit and catch-up requests as the first transport. Start with a 350 ms foreground catch-up interval and resync on focus. Use a five-second presence heartbeat and expire presence after 15 seconds. Require the peer to apply changes within one second at p95 over at least 30 scripted foreground edits on a healthy connection between two tester sessions. Sites documents WebSockets. A socket notification may prompt a catch-up request if the managed probe proves reliable delivery across sessions. The notification carries no document data or authority.

The Sites deployment declares D1 and R2. The initial design uses the D1 compare-and-set for ordering and leaves process memory available as a cache. A personal Cloudflare deployment can supply the same bindings and a server-verified identity adapter. A separate HTTPS room service is a fallback if the managed runtime cannot reconstruct and validate the WordCraft model within resource limits or meet delivery targets. A service would own the same room and Save contracts and require a server-to-server grant with the current document role. [D1 batch](https://developers.cloudflare.com/d1/worker-api/d1-database/#batch) · [Sites runtime](https://learn.chatgpt.com/docs/sites#understand-limits-and-unsupported-uses) · [Worker limits](https://developers.cloudflare.com/workers/platform/limits/)

## Desktop and offline boundary

Desktop can later use the same document identity, grants, schema version, replay cursor, operation ID, and ordered Save protocol with its own authorized sign-in. Persisted offline work needs a protected local journal with the starting revision, stable block identities, the exact unconfirmed operations, and any commands needed for safe reapplication. Reconnect first resolves uncertain receipts and fetches the server tail. **Merge** and **Suggest** follow the existing room design. If replay history has expired or an anchor was deleted, preserve the local work for comparison and an explicit recovery choice. Phase 3 covers transient reconnect in the open browser tab. Persistent offline editing, desktop participation, and a version-history interface are later work.

## Verification

| Claim | Evidence required |
| --- | --- |
| D1 can order and recover room writes | Passed in local workerd and on the separate owner-only Sites probe. The managed deployment results are recorded below. |
| The Rust room can reconstruct the document | Exercise real `ChangeSet` application, snapshot replay and compaction, and DOCX fidelity in Rust and workerd. Measure parse, replay, and serialization on synthetic typical and large Phase 2 fixtures against Worker limits. |
| C1. Access and presence | In the invited pilot, use separate signed-in accounts for owner and tester. Check pending request privacy, grant Editor and edit from both browsers, switch the tester to Viewer, then revoke while their room is open. Test ordered changes, mapped presence, and denial at each role transition. |
| C2. Durable edits | Interrupt a browser after commit and before receipt delivery, recover on the same operation ID, and check no extra revision. Restart the room runtime and verify acknowledged content and deduplicated retry. |
| C3. Ordered Save | Race an edit and a Save, reopen from another session, and verify the artifact's published-through revision and bytes. Race activation against a direct file upload and deletion against room Save. Verify no direct upload can publish past the room fence. |
| C4. Browser workflow and portability | Record the separate sessions, deployed version, measured delivery time, and replay/fidelity results. Verify the static and desktop file entry points and record the deferred offline and desktop work. |

Split review into design, durable room and ordered Save, then browser integration and private pilot acceptance. Keep the localhost Assistant milestone in [#116](https://github.com/mmizutani/wordcraft/issues/116).


### Managed runtime probe

On 2026-11-12, two deployments of the owner-only WordCraft room durability probe reused the same managed D1 binding. The Worker build marker changed between deployments.

- **v1** (`d06b40af0bad7f19d104fef1ac21388415c748ec`) accepted one of two concurrent operations at the same base and returned a stale revision for the other. Eight concurrent identical requests appended once and returned the same revision. Changed payloads under the same operation ID were refused. Two injected SQL failures rolled back the batch. Paged replay stayed within its captured head while a later edit committed. The Worker committed revision 4, then deliberately returned a retryable response in place of the receipt.
- **v2** (`e77e5adef0509716686b7adaf7b94f44eab88fec`) recovered the same revision 4 and receipt after deployment, returned it on retry with zero changed rows, replayed revision 4, and advanced to revision 5.
- The service-authenticated WebSocket upgraded and echoed a message. The Sites dispatcher returned HTTP 401 to signed-out HTTP and WebSocket requests.

This verifies the managed D1 transaction and replay contract across deployment and the proxy's WebSocket forwarding. The probe used synthetic changes and a service credential. The WordCraft Rust model, signed-in browser upgrades and delivery latency, revocation, and ordered R2 publication are covered by the remaining verification rows.

### Local implementation checks


The Rust Worker uses immutable DOCX checkpoints, a guarded D1 journal, and stable operation receipts. The [hosted room integration test](../../packaging/sites/tests/hosted_room.py) submits real WordCraft changes through Wrangler D1 and R2, restarts workerd, and checks replay, duplicate delivery, batch rollback, and the legacy file-write fence. The [access integration test](../../packaging/sites/tests/room_access.py) uses separate synthetic principals to exercise pending requests, Editor submission, Viewer denial, presence, and revocation. The [ordered Save test](../../packaging/sites/tests/room_save.py) checks exact-revision DOCX publication, recovery after a lost response and a Worker restart, concurrent saves, and deletion fencing. All three room suites and the legacy cloud-document suite passed against the same local Worker build.

Two isolated Chrome contexts exercised the full WASM editor against local Wrangler with synthetic Site identities. They converged on 32 alternating edits, with 691 ms p95 foreground peer delivery, and showed named presence. A Viewer could not edit. A revoked Editor's open browser received no later edit. The owner used the normal Save command, and the published DOCX held the 32 selected edits while a later edit remained only in the room.

The [compaction integration test](../../packaging/sites/tests/room_compaction.py) exercises checkpoint publication and rollback in local D1 and R2, stable paragraph IDs and retained OOXML in imported DOCX files, replay after a Worker restart, receipt expiry, and explicit Save. The signed-in private Sites acceptance and deployment evidence are tracked in [PR #144](https://github.com/mmizutani/wordcraft/pull/144).
