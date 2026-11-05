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
| `LiveOwnerIsAlone` (`RecoveryLock.tla`) | A record with a live owner is never scanned, listed, restored or discarded by another process. | Another window lists a running document's copy. Discarding it there, or a scan deleting its temporary file, takes away what a crash would have recovered; restoring it there gives two processes one record. |

## When to run it

Run it for any change to the files under Coverage: the order of the steps in `write_snapshot` or `discard`, what `Slot` records, when `tick` queues a job, what `open_held_restore` checks, how `lock`, `acquire` or `scan` use `owner.lock`, and any new way `dirty`, `rev` or `password` change.

```bash
sh .claude/skills/recovery-store/check.sh
```

It takes about a minute. On a first run it downloads `tla2tools.jar` 1.7.4 from github.com to `~/.cache/wordcraft-tla` (each run, if that folder is refused) and verifies its SHA-256; it needs Java 11 or later. Set `TLA2TOOLS` and `JAVA` to skip finding them. A sandboxed agent must allow github.com for that download, and `.claude/skills/` may refuse Bash writes there, so edit these files with the Write and Edit tools. In such a shell, a `cargo test` that replays a finding needs `CARGO_HOME` in the scratch folder with a real `registry` folder (a symlink to `~/.cargo/registry` cannot be written) and `CARGO_NET_GIT_FETCH_WITH_CLI=true` (cargo's built-in git stalls behind the proxy; the first fetch takes about 10 minutes).

| Exit | Meaning | What to do |
|---|---|---|
| 0 | The rules hold, each known finding still breaks its rule, the model matches the sync commit. | Nothing. |
| 1 | A rule broke in the main config. | Read the result, then decide bug or modelling error. |
| 2 | A known finding no longer breaks its rule. | See Known findings. |
| 3 | Covered code changed after the sync commit. | See Keep it in step with the code. |
| 4 | Java or TLC could not run. | Fix the tools. No result was produced. |

## Read every result with `/statechart`

Give `/statechart` this folder's `RecoveryStore.tla`, the config that broke, and `view.mjs`. It shows each break as its shortest path in plain words. When the main config passes, it reports that the graph is over its chart budget (about 9 million states) and writes no chart; lower the constants in a scratch copy to see one. For `RecoveryLock.tla`, give it `view-lock.mjs`. Tell the user a counterexample as that numbered path, in the code's words.

## Bug or modelling error

A counterexample is a claim about the code until it is replayed. Walk it through the Rust, one action at a time, using the table below, then write it as a test in `crates/engine/src/recovery/desktop/tests.rs` (`findings-replay.rs` shows the shape; `Temp`, `session`, `settle` and `save_now` are that file's helpers).

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
| `ReceiveAndOpen`, `OpenHeld`, `Open` | `Session::recovery_tick`, `open_held_restore`, `set_document`, `Slot::adopt` |
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
| `LockRecheck` | Not in the code: only with `NoRecheckAfterLock = FALSE`. |
| `SaveStart` | `write_snapshot`: `dir.exists()`, `private_dir`, then `acquire`, which locks only if `locks` has no entry |
| `SaveFiles`, `SaveTemp`, `SaveRename` | `write_snapshot`: `Self::files`, `private_file(&tmp, true)`, `fs::rename` with the sync and prune after it |
| `SaveFailed` | `save`: discard the record if a failed write left it without a generation |
| `ScanStart` | `scan`: `directories()`, then `lock` unless `locks` has an entry (`guard` is `None`) |
| `ScanRead` | `scan`: the loops that delete `pending-*.tmp` and read the generations |
| `ScanRelease`, `ScanEmpty` | `scan`: `drop(guard)`, then `Self::files(&dir)?.is_empty()` with no lock held |
| `LoadStart`, `LoadRead` | `load`: `acquire`, `read_snapshot`, and `Slot::adopt` on success |
| `DiscardJob` | `Job::Discard`; `Applied::Discarded` clears the slot |
| `DiscardStart`, `DiscardDelete` | `discard`: `dir.exists()`, `acquire`, the entry check, deleting generations and temporary files, `sync_dir`, `entries.remove` |
| `DiscardRelease`, `DiscardUnlink`, `DiscardRmdir` | `discard`: `locks.remove(id)`, `fs::remove_file(owner.lock)`, `fs::remove_dir(&dir)` |
| `Crash` | A process dies: its descriptors close, its files stay |

`LiveOwner` ends once a process's own discard has deleted the record's files. Without that, TLC reports a process finishing its own discard while another makes a new `owner.lock`, which only makes the first one's `remove_dir` fail.

## Scope

`RecoveryStore.tla` is checked with `MaxEdits = 2`, `MaxFaults = 1`, `MaxCrashes = 1`, three record ids: about 28 million states generated, 9 million distinct. `RecoveryLock.tla` is checked with three processes, `MaxOps = 8`, `MaxCrashes = 2`: about 6 million states generated, 1.1 million distinct. A pass covers these models at these bounds, not the whole app.

Left out of `RecoveryLock.tla`:

- Other records: one record, whose id `Creator` made up. `new_id` is 128 random bits, so two processes only meet on a record through a scan or a restore.
- Document contents and generations: `copies` says only whether any `snapshot-*.json` exists.
- Time: nothing says how likely an order is. See Known findings for how likely this one is.
- I/O failures, and discards interrupted partway other than by `Crash`: `DiscardDelete` is one step.
- Filesystems where flock does not behave as it does on a local APFS or ext4 disk (NFS, some network home folders), and Windows, where `try_lock` is `LockFileEx` and the replay does not run.
- `ReclaimOnlyEmpty`, which the current code also breaks: `scan` looks whether a record is empty with no lock held, and `discard` deletes what it finds without looking again. See the Known findings note below; it is a different promise and has no config here.

Left out of `RecoveryStore.tla`:

- A second running process: `RecoveryLock.tla` covers `owner.lock` instead.
- More than one open document. Another document exists only as `active = FALSE`; parked documents, closing one (`Recovery::close`) and a restore that opens beside a used document are out.
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
| `wants_copy` ignores the password | `Tick` tests `dirty` alone | `NoPlaintextOfProtected` |
| `open_held_restore` skips the revision check | `Open` tests `TRUE` | `RestoreOnlyReplacesBlank` |
| `try_lock` ignores another process's lock | `LockTry` does not test `holder` | `LiveOwnerIsAlone` |
| `discard` keeps the lock until after `remove_dir`, and `lock` does not recheck | `ReleaseBeforeUnlink = FALSE`, `NoRecheckAfterLock = TRUE` | `LiveOwnerIsAlone` |
| `lock` rechecks the path, and `discard` still releases first | `ReleaseBeforeUnlink = TRUE`, `NoRecheckAfterLock = FALSE` | `LiveOwnerIsAlone` |

Moving `sync_dir` after the prune while still sparing `previous` is not a break: the last confirmed copy stays on disk whichever way a crash lands. Reason a change through the model before calling it safe or unsafe.

## Known findings

The current code breaks `UnsavedWorkHasCopy` in two ways, both replayed on the real code by `findings-replay.rs`, and `LiveOwnerIsAlone` in one, replayed by a test in `tests.rs`. Each has a flag so the main config can stay green and still catch anything new. The flag turns on behaviour the code really has; the config with the flag on is the honest statement of the current code, and `check.sh` requires it to keep failing. So a green `RecoveryLock.cfg` describes the design with both of that finding's changes, which the code does not have yet.

**`finding-lock-orphan.cfg`.** `Store::lock` keeps a descriptor open across its retries, and a flock on an unlinked file succeeds.

1. A starts its first copy: it makes the record's folder, and `Store::lock` opens `owner.lock`.
2. B's `Store::scan` got there first, in the tens of microseconds before A's `try_lock`. It finds the record empty and reclaims it through `Store::discard`, so A's `try_lock` is told to wait 10 ms.
3. `discard` finishes: `locks.remove`, unlink, `remove_dir`. Nothing tells A.
4. A's next retry locks the file its descriptor still names, which is deleted. `Store.locks` keeps it, and `Store::acquire` never locks again. A's write in progress fails on the missing folder, and its next save makes the folder again, with no `owner.lock` in it.
5. Any later scan makes a new `owner.lock` and locks it with nothing to stop it. It deletes A's `pending-*.tmp`, and lists A's live copy, which that process can then restore or discard. If it is discarded and A crashes, A's unsaved work is gone.

`finding_a_lock_won_on_a_discarded_file_opens_a_live_record_to_other_processes` replays this with the real functions and passes while the finding is there. `finding_measure_lock_race_on_unmodified_code` (ignored, run by hand) measures it. With B scanning non-stop, a scan beat A to the first lock in about 20% of first copies. Once it had, A held the deleted file in 3,928 of 3,928 cases, and a later scan listed A's live copy in 3,928 of 3,928. Step 2 is the rare one: scans run when a process starts and on the Recover page's refresh, not non-stop, so one must land in the gap by chance, about 50 µs out of hours. Two windows open all day give roughly 2 in 10 million per day, and the harm then still needs someone to discard or restore in the other window.

Neither change alone is enough. With the lock held through `remove_dir` but no recheck, step 4 is unchanged (679 of 680 waits on the real code). With a recheck but the lock released first, A can pass the recheck just before B's unlink: the model shows it, the replay still reproduces it, and the measurement found it in 0 of 9,139 waits. With both, the replay stops reproducing and the measurement finds 0 of 4,057 waits. Those were temporary patches; none of them is in the code.

The same race has a smaller visible effect, measured but not modelled: `scan` returns an error for the whole folder when a record it has locked loses its folder, so its list stays as it was. Between 0.4% and 1% of the non-stop scans hit it, and none with both changes.

Also found, and not a finding here because it is another promise: `scan` looks whether a record is empty after dropping its guard, and `discard` deletes every generation without looking again. If A commits its first copy and dies in the 100 ms B's `lock` retries for, B deletes the only copy of A's work. TLC finds that path with the rule `ReclaimOnlyEmpty == \A p \in Procs : ~(proc[p].pc = "discardDelete" /\ proc[p].why = "reclaim" /\ copies)`, and `Store::discard` deletes such a copy when called in that order. It starts from the same rare step 2. A real scan has not been seen doing it.

**`finding-held-discard.cfg`.** `recovery_documents` still lists a copy waiting in `Recovery::held`, so the user can discard it.

1. A blank document asks to restore a copy.
2. You switch to another document while it loads, so the copy waits in `held`.
3. You discard the same copy from the list. The record is deleted.
4. You switch back. The copy opens from memory, dirty, and `Slot::adopt` records it as covered.
5. No copy is written until the next edit. A crash now loses the restored text.

**`finding-late-discard.cfg`.** `Store::discard` deletes the generations before steps that can still fail, and `Slot::apply(Failed { discard: true })` keeps `revision`.

1. A dirty document has a current copy.
2. You set a password. `purge_recovery` calls `Store::discard`, which deletes the copy and then fails (`sync_dir`, unlinking `owner.lock`, `remove_dir`).
3. The password is refused, and the document stays dirty and unprotected.
4. The slot still says this revision is covered, so no copy is written until the next edit. A crash now loses the unsaved work.

When `check.sh` exits 2, replay the finding with `findings-replay.rs`, or for `finding-lock-orphan.cfg` with the `finding_` tests in `tests.rs`. If the code really is fixed, delete the flag from the model and its config from `check.sh` and this file, turn the replay into a regression test, and make the model's action match the fix. A fix that only changes the code leaves the flag turning on behaviour the code no longer has.

## Keep it in step with the code

The sync commit is the last commit at which someone confirmed that the model describes the covered code. Keep it on `main`, because `check.sh` compares against it and a squash merge removes a branch's commits.

sync-commit: 3f555bc85dc9cc742ddae9f287cfc7b93a87480b

covered-files:
- crates/engine/src/recovery.rs
- crates/engine/src/recovery/desktop.rs
- crates/engine/src/session/documents.rs
- crates/engine/src/session/query.rs
- crates/engine/src/session/snapshot.rs
- crates/engine/src/cmd/file.rs

When `check.sh` exits 3:

1. Read the diff of the files it names, against the sync commit and the mapping table.
2. If the change affects a step, change the matching action first, so that the check shows what the change does. Run it. If a rule breaks, read the result and decide bug or modelling error.
3. If it doesn't affect a step (another command in `cmd/file.rs`, say), the model needs no edit.
4. Move `sync-commit` forward only after confirming that the model describes the code at the new commit, in the commit that does so.

The check cannot read the Rust. It sees only that a covered file changed; the agent carries the change into the model. So an exit 3 is never something to clear by moving the commit without reading the diff.
