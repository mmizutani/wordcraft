---
name: recovery-store
description: Model-check WordCraft's crash recovery (recovery copies, Slot, Store, restore, password purge) with TLA+. Use when changing crates/engine/src/recovery.rs or recovery/desktop.rs, how dirty, rev or the password change, or when asked whether unsaved work survives a crash.
---

# Recovery store

`RecoveryStore.tla` is a small model of how one open document's recovery copy moves between the UI thread, the worker thread and the disk. TLC, the TLA+ model checker, walks every order of those steps, with a crash or an I/O failure allowed at each point.

## The promises

| Rule | In plain words | The harm if it breaks |
|---|---|---|
| `UnsavedWorkHasCopy` | When the slot says a dirty document's copy is current, that text really is on disk. | A crash loses writing the app believed was safe, and nothing warned the user. |
| `NewestCopySurvives` | The newest copy a write confirmed, or a newer one, stays on disk until someone asks to discard it. | A crash during a later copy destroys the last good one. |
| `NoPlaintextOfProtected` | A password-protected document's text is never in a recovery copy. | The document's text sits unencrypted in the profile folder. |
| `RestoreOnlyReplacesBlank` | A restored copy replaces only a document that holds no text. | Restoring overwrites work. |

## When to run it

Run it for any change to the files under Coverage: the order of the steps in `write_snapshot` or `discard`, what `Slot` records, when `tick` queues a job, what `open_held_restore` checks, and any new way `dirty`, `rev` or `password` change.

```bash
sh .claude/skills/recovery-store/check.sh
```

It takes about 30 seconds. On a first run it downloads `tla2tools.jar` 1.7.4 from github.com to `~/.cache/wordcraft-tla` (each run, if that folder is refused) and verifies its SHA-256; it needs Java 11 or later. Set `TLA2TOOLS` and `JAVA` to skip finding them. A sandboxed agent must allow github.com for that download, and `.claude/skills/` may refuse Bash writes there, so edit these files with the Write and Edit tools.

| Exit | Meaning | What to do |
|---|---|---|
| 0 | The rules hold, each known finding still breaks its rule, the model matches the sync commit. | Nothing. |
| 1 | A rule broke in the main config. | Read the result, then decide bug or modelling error. |
| 2 | A known finding no longer breaks its rule. | See Known findings. |
| 3 | Covered code changed after the sync commit. | See Keep it in step with the code. |
| 4 | Java or TLC could not run, or TLC stopped without a result (out of memory, a model that doesn't parse). | Read the output it names and fix the cause. Nothing was checked. |

## Read every result with `/statechart`

Give `/statechart` this folder's `RecoveryStore.tla` and the config that broke. It shows each break as its shortest path, in the model's action names and values. When the main config passes, it reports that the graph is over its chart budget (about 13 million states) and writes no chart; lower the constants in a scratch copy to see one. Tell the user a counterexample as that numbered path, translated through the table under How the model maps to the code. If you want `/statechart`'s view module for plainer words, write it outside the repository: this repo holds no handwritten JavaScript.

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
| `ReceiveAndOpen`, `OpenHeld`, `Open` | `Session::recovery_tick`, `open_held_restore`, `set_document`, `Slot::adopt` |
| `WorkerScan` | `Store::scan` |
| `WorkerWriteTemp`, `WorkerRename`, `WorkerSyncDir`, `WorkerPrune` | `Store::write_snapshot`, in that order |
| `WorkerLoad` | `Store::load` |
| `WorkerDiscard` | `Store::discard` |
| `Crash` | Process exit or power loss, then a new session's `Store::scan` |

## Scope

Checked with `MaxEdits = 2`, `MaxFaults = 1`, `MaxCrashes = 1`, three record ids: about 41 million states generated, 13 million distinct. A pass covers this model at these bounds, not the whole app.

Left out:

- A second running process: `owner.lock` and its advisory lock are not modelled.
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
| `Slot::apply` keeps `revision` after a failed discard | `Receive` and `EncryptDocument` keep `slotRev` | `UnsavedWorkHasCopy` |
| `recovery_documents` lists the copy waiting in `held` | `Visible` leaves `held.id` in | `UnsavedWorkHasCopy` |
| `wants_copy` ignores the password | `Tick` tests `dirty` alone | `NoPlaintextOfProtected` |
| `open_held_restore` skips the revision check | `Open` tests `TRUE` | `RestoreOnlyReplacesBlank` |

Moving `sync_dir` after the prune while still sparing `previous` is not a break: the last confirmed copy stays on disk whichever way a crash lands. Reason a change through the model before calling it safe or unsafe.

## Known findings

None are open.

A replayed bug that won't be fixed right away stays visible without turning the main config red. Give the model a flag that turns on the behaviour the code really has, and make a `finding-<name>.cfg` with the flag on. Add `finding-<name>` to `known_findings` in `check.sh`, which then requires that config to keep breaking `UnsavedWorkHasCopy`. Write the steps here, and keep a test that replays them on the real code.

When `check.sh` exits 2, replay the finding. If the code really is fixed, delete the flag from the model and its config from `check.sh` and this file, turn the replay into a regression test, and make the model's action match the fix. A fix that only changes the code leaves the flag turning on behaviour the code no longer has.

## Keep it in step with the code

The sync commit is the last commit at which someone confirmed that the model describes the covered code. Keep it on `main`, because `check.sh` compares against it and a squash merge removes a branch's commits.

sync-commit: b0e28ce4ffe6dd69a98d86fd1162eb666e1b9c19

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
