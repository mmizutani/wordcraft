---
name: recovery-store
description: Model-check WordCraft's crash recovery (recovery copies, Slot, Store, restore, password purge, the owner.lock shared by running processes) with TLA+. Use when changing crates/engine/src/recovery.rs or recovery/desktop.rs, how dirty, rev or the password change, or when asked whether unsaved work survives a crash or a second running WordCraft.
---

# Recovery store

`RecoveryStore.tla` is a small model of how one open document's recovery copy moves between the UI thread, the worker thread and the disk. TLC, the TLA+ model checker, walks every order of those steps, with a crash or an I/O failure allowed at each point.

`RecoveryLock.tla` is the part that model leaves out: two or three running processes and one record, with `owner.lock`'s inodes as model state, because a flock belongs to the inode and not to the path.

## The promises

| Rule | In plain words | The harm if it breaks |
|---|---|---|
| `UnsavedWorkHasCopy` | When the slot says a dirty document's copy is current, that text really is on disk. | A crash loses writing the app believed was safe, and nothing warned the user. |
| `NewestCopySurvives` | The newest copy a write confirmed, or a newer one, stays on disk until someone asks to discard it. | A crash during a later copy destroys the last good one. |
| `NoPlaintextOfProtected` | A password-protected document's text is never in a recovery copy. | The document's text sits unencrypted in the profile folder. |
| `RestoreOnlyReplacesBlank` | A restored copy replaces only a document that holds no text. | Restoring overwrites work. |
| `RestoreNeverReplacesShared` | A restored copy never replaces a document a room shares, even one shared after the restore was asked for. It opens beside it. | Restoring puts an old copy in place of the document a room is editing, under the people editing it. |
| `LiveOwnerIsAlone` (`RecoveryLock.tla`) | A record with a live owner is never scanned, listed, restored or discarded by another process. | Another window lists a running document's copy. Discarding it there, or a scan deleting its temporary file, takes away what a crash would have recovered; restoring it there gives two processes one record. |
| `ReclaimOnlyEmpty` (`RecoveryLock.tla`) | Cleaning up an empty record, after a scan or a failed save, never deletes a committed copy. | A process that wrote its first copy and then crashed loses it to another process's cleanup. |

## When to run it

Run it for any change to the files under Coverage: the order of the steps in `write_snapshot` or `discard`, what `Slot` records, when `tick` queues a job, what `open_held_restore` checks, how `lock`, `acquire` or `scan` use `owner.lock`, how a room shares a document or applies another participant's change (`Room::host`, `reset_shared`, `apply_theirs`), and any new way `dirty`, `rev` or `password` change.

```bash
sh .claude/skills/recovery-store/check.sh
```

It takes about two minutes. On a first run it downloads `tla2tools.jar` 1.7.4 from github.com to `~/.cache/wordcraft-tla` (each run, if that folder is refused) and verifies its SHA-256; it needs Java 11 or later. Set `TLA2TOOLS` and `JAVA` to skip finding them. A sandboxed agent must allow github.com for that download, and `.claude/skills/` may refuse Bash writes there, so edit these files with the Write and Edit tools. In such a shell, a `cargo test` that replays a finding needs `CARGO_HOME` in the scratch folder with a real `registry` folder (a symlink to `~/.cargo/registry` cannot be written) and `CARGO_NET_GIT_FETCH_WITH_CLI=true` (cargo's built-in git stalls behind the proxy; the first fetch takes about 10 minutes).

| Exit | Meaning | What to do |
|---|---|---|
| 0 | The rules hold, each known finding still breaks its rule, the model matches the sync commit. | Nothing. |
| 1 | A rule broke in the main config. | Read the result, then decide bug or modelling error. |
| 2 | A known finding no longer breaks its rule. | See Known findings. |
| 3 | Covered code changed after the sync commit. | See Keep it in step with the code. |
| 4 | Java or TLC could not run, or TLC stopped without a result (out of memory, a full disk, a model that doesn't parse). | Read the output it names and fix the cause. Nothing was checked. |

## Read every result with `/statechart`

Give `/statechart` this folder's `RecoveryStore.tla` or `RecoveryLock.tla` and the config that broke. It shows each break as its shortest path, in the model's action names and values. When the main config passes, it reports that the graph is over its chart budget (`RecoveryStore.tla` has about 41 million states, `RecoveryLock.tla` about 1.4 million) and writes no chart; lower the constants in a scratch copy to see one. Tell the user a counterexample as that numbered path, translated through the tables under How the model maps to the code. If you want `/statechart`'s view module for plainer words, write it outside the repository: this repo holds no handwritten JavaScript.

## Bug or modelling error

A counterexample is a claim about the code until it is replayed. Walk it through the Rust, one action at a time, using the table below, then write it as a test in `crates/engine/src/recovery/desktop/tests.rs` (`a_discard_failing_late_leaves_the_dirty_document_a_new_copy` shows the shape; `Temp`, `session`, `settle` and `save_now` are that file's helpers).

- If the test shows the harm, it is a bug. Report it with the path. Don't change the model or the rule to make it pass, and don't change product code unless the user asks.
- If a step cannot happen in the code, the model is wrong. Fix the action and name which code rules the step out. Example from building this model: `Session::recovery_tick` opens a loaded copy in the frame it arrives when its document is shown, so `ReceiveAndOpen` is one action.

Text values stand for document contents, and every blank document shares `Blank0`. A break that depends on two blank documents being "the same text" is a modelling error.

## How the model maps to the code

| Action | Rust |
|---|---|
| `Edit` | `Session::run` of a mutating command, then `Session::touch` (`session/query.rs`) |
| `SaveDocument` | `save` in `cmd/file.rs`: `dirty` clears only after the write succeeds |
| `SwitchDocument` | `Session::activate` |
| `EncryptDocument`, `RemoveEncryption` | `encrypt` in `cmd/file.rs`, `clear_recovery_before_password`, `purge_recovery` (a pure command: `dirty` set, `rev` not) |
| `RequestRestore`, `RequestDiscard` | `Session::recover`, filtered by `recovery_documents` |
| `Tick` | `Recovery::tick`, with `DocRecovery` from `Session::recovery_docs` |
| `Receive` | `Recovery::receive`, `Recovery::apply`, `Slot::apply` |
| `ReceiveAndOpen`, `OpenHeld`, `Open` | `Session::recovery_tick`, `open_held_restore`, `set_document`, `Slot::adopt`; `add_document` beside a shared document |
| `ShareDocument` | `Room::host` (`room.rs`): `reset_shared` touches the document when the room's blocks need ids, then `shared_through` |
| `StopSharing` | `stop_sharing` in `crates/ui-egui/src/share.rs`: `Room::close`, then one last `Replica::receive`, which is a `TheirEdit` just before |
| `TheirEdit` | `Replica::receive` (`room/replica.rs`), through `lend_document` when another document is shown: `Session::apply_theirs` or `reset_shared`, then `touch` at the end of `follow_shared` (`session/shared.rs`) |
| `WorkerScan` | `Store::scan` |
| `WorkerWriteTemp`, `WorkerRename`, `WorkerSyncDir`, `WorkerPrune` | `Store::write_snapshot`, in that order |
| `WorkerLoad` | `Store::load` |
| `WorkerDiscard` | `Store::discard` |
| `Crash` | Process exit or power loss, then a new session's `Store::scan` |

In `RecoveryLock.tla`, one action is one system call, or a run of them nothing else can change the result of:

| Action | Rust, all in `Store` |
|---|---|
| `LockOpen` | `lock`: `private_file(owner.lock, create)`. It fails if the folder is gone. |
| `LockTry`, `LockGiveUp` | `lock`: `file.try_lock()`. `LOCK_RETRIES` and the 10 ms sleep are abstract: a blocked process may try again any number of times, or give up. |
| `LockRecheck` | `lock`: `names_file(&path, &file)`, and opening the path again if it does not name the locked file |
| `SaveStart` | `write_snapshot`: `dir.exists()`, `private_dir`, then `acquire`, which locks only if `locks` has no entry |
| `SaveFiles`, `SaveTemp`, `SaveRename` | `write_snapshot`: `Self::files`, `private_file(&tmp, true)`, `fs::rename` with the sync and prune after it |
| `SaveFailed` | `save`: reclaim the record if a failed write left it without a generation |
| `ScanStart` | `scan`: `directories()`, then `lock` unless `locks` has an entry (`guard` is `None`) |
| `ScanRead` | `scan`: the loops that delete `pending-*.tmp` and read the generations |
| `ScanRelease`, `ScanEmpty` | `scan`: `drop(guard)`, then `Self::files(&dir)?.is_empty()` with no lock held |
| `LoadStart`, `LoadRead` | `load`: `acquire`, `read_snapshot`, and `Slot::adopt` on success |
| `DiscardJob` | `Job::Discard`; `Applied::Discarded` clears the slot |
| `DiscardStart`, `DiscardDelete` | `discard`: `dir.exists()`, `acquire`, the entry check, deleting generations and temporary files, `sync_dir`, `entries.remove` |
| `DiscardCheck` | `reclaim`: `Self::files` under the lock, then `discard` if empty, or release a lock taken only for this look |
| `DiscardUnlink`, `DiscardRmdir`, `DiscardRelease`, `DiscardRmdirAgain` | `discard`: `fs::remove_file(owner.lock)`, `fs::remove_dir(&dir)`, `locks.remove(id)`, then `fs::remove_dir(&dir)` again if the first one failed |
| `Crash` | A process dies: its descriptors close, its files stay |

`LiveOwner` ends once a process's own discard has deleted the record's files. Without that, TLC reports a process finishing its own discard while another makes a new `owner.lock`, which only makes the first one's `remove_dir` fail.

## Scope

`RecoveryStore.tla` is checked with `MaxEdits = 2`, `MaxFaults = 1`, `MaxCrashes = 1`, three record ids: about 162 million states generated, 41 million distinct. `RevisionBounded` in its config is not a promise of the code. It keeps the model finite: every action that bumps `rev` spends an edit, and one that doesn't breaks it within a few steps. Without it, TLC keeps finding new states and writes them to disk until the disk is full. `RecoveryLock.tla` is checked with three processes, `MaxOps = 8`, `MaxCrashes = 2`: about 7 million states generated, 1.4 million distinct. A pass covers these models at these bounds, not the whole app.

Left out of `RecoveryLock.tla`:

- Other records: one record, whose id `Creator` made up. `new_id` is 128 random bits, so two processes only meet on a record through a scan or a restore.
- Document contents and generations: `copies` says only whether any `snapshot-*.json` exists.
- Time: nothing says how likely an order is.
- I/O failures, and discards interrupted partway other than by `Crash`: `DiscardDelete` is one step.
- Filesystems where flock does not behave as it does on a local APFS or ext4 disk (NFS, some network home folders), and Windows, where `try_lock` is `LockFileEx`. Nobody has run the lock test there. A Windows volume without POSIX delete keeps an open unlinked file in its folder until it closes, which is what `discard`'s second `remove_dir` is for; the model's unlink always takes the file out at once.

Left out of `RecoveryStore.tla`:

- A second running process: `RecoveryLock.tla` covers `owner.lock` instead.
- More than one open document. Another document exists only as `active = FALSE`; parked documents, closing one (`Recovery::close`) and a restore that opens beside a used or shared document are out. For a shared document the model checks only that it is not replaced.
- The room itself: other participants, the hub, and what they see. `TheirEdit` stands for any change that reaches this window. `room.rs` and `room/replica.rs` are not covered files, so a change to when they call `reset_shared` or `apply_theirs` does not make `check.sh` exit 3.
- The time intervals (`Slot.due`, `Slot.retry`): any tick may find them passed.
- Corrupt or foreign files in the recovery folder, size and disk limits, `MAX_FILES` and `MAX_RECORDS`.
- Discards interrupted partway: `WorkerDiscard` deletes a record's generations in one step.
- Whether a copy holds the full document: `write_snapshot` reads it back, and tests cover that.
- Liveness: the rules say a copy the slot calls current exists, not that a tick eventually runs.
- `io/safe_save.rs`, the document's own file. `SaveDocument` assumes a save is all or nothing.

## Able to fail

Each rule has been seen failing on a broken variant, made in a scratch copy of the model, with the code change it stands for:

| Change to the code | Change to the model | Rule that broke |
|---|---|---|
| Prune every old generation, `previous` included, before the rename is synced | `WorkerRename` replaces the record's generations | `NewestCopySurvives` |
| `Slot::apply` keeps `revision` after a transient failure | `Receive` keeps `slotRev` | `UnsavedWorkHasCopy` |
| `Slot::apply` keeps `revision` after a failed discard | `Receive` and `EncryptDocument` keep `slotRev` | `UnsavedWorkHasCopy` |
| `recovery_documents` lists the copy waiting in `held` | `Visible` leaves `held.id` in | `UnsavedWorkHasCopy` |
| `wants_copy` ignores the password | `Tick` tests `dirty` alone | `NoPlaintextOfProtected` |
| `open_held_restore` skips the revision check | `Open` tests `TRUE` | `RestoreOnlyReplacesBlank` |
| `open_held_restore` replaces a document shared after the restore was asked for | `Open` tests `FALSE` for `shared` | `RestoreNeverReplacesShared` |
| `follow_shared` applies another participant's change without `touch` | `TheirEdit` leaves `rev` and `dirty` | `UnsavedWorkHasCopy` |
| None: the model's own mistake, a share touch with no edit budget | `ShareDocument` bumps `rev` without spending an edit | `RevisionBounded` |
| `try_lock` ignores another process's lock | `LockTry` does not test `holder` | `LiveOwnerIsAlone` |
| `lock` trusts the file it locked | `LockRecheck` tests `TRUE` | `LiveOwnerIsAlone` |
| `discard` releases the lock before it unlinks | `DiscardDelete` releases `holder` | `LiveOwnerIsAlone` |
| `scan` and `save` call `discard` instead of `reclaim` | `DiscardLocked` is always `"discardDelete"` | `ReclaimOnlyEmpty` |

**Why `lock` and `discard` each have their half.** A flock belongs to the inode, so a process that opened `owner.lock` and is retrying can lock a file a discard has since unlinked. It then believes it owns the record while the next scan makes a new `owner.lock` and takes the record, live copy included. This happened in the code until the fix; `a_process_waiting_on_a_discard_never_keeps_the_deleted_lock` guards the recheck. Neither half is enough alone:

- Without the recheck in `lock`, a process waiting through a discard simply wins the deleted file on its next retry. Measured with B scanning non-stop: 679 of 680 waits.
- Without `discard` holding the lock until the folder is gone, a process can pass the recheck just before the unlink. That window is microseconds wide (0 of 9,139 waits measured), but the model finds it, and `discard_holds_the_lock_until_the_record_is_gone` catches it by spinning on `try_lock`.

With both, 0 of 9,001 waits across 20,000 first copies, and a process that loses this way gets an ordinary failed save, retried at the next interval.

**Why `reclaim` is not `discard`.** `scan` and `save` look whether a record is empty with no lock held, and `discard` deletes every generation, as a discard the user asked for should. Without the second look, a process that commits its first copy and dies while the other's `lock` is retrying loses that copy. `cleaning_up_an_empty_record_keeps_a_copy_written_since_the_look` tests `reclaim` itself. No test can pause `scan` or `save` at that gap, so only `ReclaimOnlyEmpty` covers those two calling it.

`is_untouched_blank` counting a shared document as used is not what keeps a shared document from being replaced. A document can be shared after its restore was asked for, so `open_held_restore`'s own check is the one that matters. Without it the model breaks, as the table shows. Without the blank check it cannot: only `Open`'s replacing branch sets `replacedShared`, and that branch needs the document not to be shared.

Moving `sync_dir` after the prune while still sparing `previous` is not a break: the last confirmed copy stays on disk whichever way a crash lands. Reason a change through the model before calling it safe or unsafe.

## Known findings

None are open.

A replayed bug that won't be fixed right away stays visible without turning the main config red. Give the model a flag that turns on the behaviour the code really has, and make a `finding-<name>.cfg` with the flag on. Add `finding-<name>` to `known_findings` in `check.sh`, which then requires that config to keep breaking `UnsavedWorkHasCopy`. Write the steps here, and keep a test that replays them on the real code.

When `check.sh` exits 2, replay the finding. If the code really is fixed, delete the flag from the model and its config from `check.sh` and this file, turn the replay into a regression test, and make the model's action match the fix. A fix that only changes the code leaves the flag turning on behaviour the code no longer has.

## Keep it in step with the code

The sync commit is the last commit at which someone confirmed that the model describes the covered code. Keep it on `main`, because `check.sh` compares against it and a squash merge removes a branch's commits.

sync-commit: 9c8a0a756d164689dfc404dcca52f17690265584

covered-files:
- crates/engine/src/recovery.rs
- crates/engine/src/recovery/desktop.rs
- crates/engine/src/session/documents.rs
- crates/engine/src/session/query.rs
- crates/engine/src/session/shared.rs
- crates/engine/src/session/snapshot.rs
- crates/engine/src/cmd/file.rs

When `check.sh` exits 3:

1. Read the diff of the files it names, against the sync commit and the mapping table.
2. If the change affects a step, change the matching action first, so that the check shows what the change does. Run it. If a rule breaks, read the result and decide bug or modelling error.
3. If it doesn't affect a step (another command in `cmd/file.rs`, say), the model needs no edit.
4. Move `sync-commit` forward only after confirming that the model describes the code at the new commit, in the commit that does so.

The check cannot read the Rust. It sees only that a covered file changed; the agent carries the change into the model. So an exit 3 is never something to clear by moving the commit without reading the diff.
