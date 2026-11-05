---- MODULE RecoveryStore ----
\* Crash recovery of one open document: crates/engine/src/recovery.rs (Session) and
\* crates/engine/src/recovery/desktop.rs (Slot, Recovery, Store). Each action names the Rust it
\* stands for. Between any two actions the process may crash and the worker may finish.
EXTENDS Integers, FiniteSets

CONSTANTS
    MaxEdits,          \* edits the document can take, across all runs
    MaxFaults,         \* I/O failures the worker can hit
    MaxCrashes         \* process crashes or power losses

None == "none"
OldText == 100      \* the text of the copy an earlier run left
Ids == {"earlier", "new1", "new2"}
Blank0 == 0

VARIABLES
    text, rev, dirty, protected, active, \* Session: document, revision, dirty, password, shown
    savedTexts,                          \* texts that reached the document's own file (file.save)
    slotId, slotRev, copyRefused,        \* Slot.id, Slot.revision, a permanent failure was reported
    job, ready, listed, held,            \* Recovery: job under way, ready, documents, held
    entries,                             \* Store.entries: id -> generation (0 = none)
    disk,                                \* id -> the snapshot-*.json files [gen, text, synced]
    confirmed,                           \* id -> newest generation whose sync_dir returned Ok
    replacedWork,                        \* a restore overwrote a document that held text
    edits, faults, crashes

vars == << text, rev, dirty, protected, active, savedTexts, slotId, slotRev, copyRefused, job,
           ready, listed, held, entries, disk, confirmed, replacedWork, edits, faults, crashes >>

NoHeld == [id |-> None, rev |-> 0, text |-> 0]
Job(kind, id, txt, step, gen, prev, why, reqRev) ==
    [kind |-> kind, id |-> id, text |-> txt, step |-> step, gen |-> gen, prev |-> prev,
     why |-> why, reqRev |-> reqRev]
NoJob == Job("none", None, 0, "", 0, 0, "", 0)
ScanJob == Job("scan", None, 0, "run", 0, 0, "", 0)
Answer(j, why) == [j EXCEPT !.step = "answered", !.why = why]

Newest(i) == IF disk[i] = {} THEN 0
             ELSE (CHOOSE g \in disk[i] : \A h \in disk[i] : g.gen >= h.gen).gen
Inventory == {i \in Ids : entries[i] # 0}
Fresh == {i \in Ids \ {"earlier"} : disk[i] = {} /\ entries[i] = 0 /\ i \notin listed}
\* Session::recovery_documents: listed copies, without the one this document keeps.
Visible == listed \ {slotId, held.id}
\* Session::is_untouched_blank, with no password.
Blank == text = Blank0 /\ ~dirty /\ rev = 0 /\ ~protected

Init ==
    /\ text = Blank0 /\ rev = 0 /\ dirty = FALSE /\ protected = FALSE /\ active = TRUE
    /\ savedTexts = {}
    /\ slotId = None /\ slotRev = -1 /\ copyRefused = FALSE
    /\ job = ScanJob /\ ready = FALSE /\ listed = {} /\ held = NoHeld
    /\ entries = [i \in Ids |-> 0]
    /\ disk = [i \in Ids |-> IF i = "earlier"
                             THEN {[gen |-> 1, text |-> OldText, synced |-> TRUE]} ELSE {}]
    /\ confirmed = [i \in Ids |-> IF i = "earlier" THEN 1 ELSE 0]
    /\ replacedWork = FALSE
    /\ edits = 0 /\ faults = 0 /\ crashes = 0

----
\* The user, on the UI thread.

\* Session::run of any mutating command: touch() bumps the revision and sets dirty.
Edit ==
    /\ active /\ edits < MaxEdits
    /\ edits' = edits + 1 /\ text' = edits + 1 /\ rev' = rev + 1 /\ dirty' = TRUE
    /\ copyRefused' = FALSE
    /\ UNCHANGED << protected, active, savedTexts, slotId, slotRev, job, ready, listed, held,
                    entries, disk, confirmed, replacedWork, faults, crashes >>

\* cmd/file.rs save: dirty clears only after the file is written. A failed save changes nothing.
SaveDocument ==
    /\ active /\ dirty
    /\ dirty' = FALSE /\ savedTexts' = savedTexts \cup {text}
    /\ UNCHANGED << text, rev, protected, active, slotId, slotRev, copyRefused, job, ready, listed,
                    held, entries, disk, confirmed, replacedWork, edits, faults, crashes >>

\* Another document is shown, or this one again: Session::activate. The other is not modelled.
SwitchDocument ==
    /\ active' = ~active
    /\ UNCHANGED << text, rev, dirty, protected, savedTexts, slotId, slotRev, copyRefused, job,
                    ready, listed, held, entries, disk, confirmed, replacedWork, edits, faults,
                    crashes >>

\* cmd/file.rs encrypt -> Session::clear_recovery_before_password -> purge_recovery. It blocks
\* until the worker is idle, so the action needs an idle worker. encrypt is a pure command: it
\* sets dirty itself and the revision stays.
Unchanged4Password == UNCHANGED << text, rev, active, savedTexts, copyRefused, job, ready, listed,
                                   held, replacedWork, edits, crashes >>
EncryptDocument ==
    /\ active /\ ~protected /\ job.kind = "none"
    /\ Unchanged4Password
    /\ \/ \* purge_recovery returns early: no copy. Slot.revision stays as it was.
          /\ slotId = None
          /\ protected' = TRUE /\ dirty' = TRUE
          /\ UNCHANGED << slotId, slotRev, entries, disk, confirmed, faults >>
       \/ \* The copy is deleted, then the password is applied.
          /\ slotId # None
          /\ disk' = [disk EXCEPT ![slotId] = {}]
          /\ entries' = [entries EXCEPT ![slotId] = 0]
          /\ confirmed' = [confirmed EXCEPT ![slotId] = 0]
          /\ slotId' = None /\ slotRev' = -1
          /\ protected' = TRUE /\ dirty' = TRUE
          /\ UNCHANGED faults
       \/ \* Store::discard refuses first ("unexpected entry"): the copy stays, password refused,
          \* Slot::apply(Failed { discard: true }) clears Slot.revision.
          /\ slotId # None /\ faults < MaxFaults
          /\ faults' = faults + 1
          /\ slotRev' = -1
          /\ UNCHANGED << protected, dirty, slotId, entries, disk, confirmed >>
       \/ \* Store::discard fails after the generations are gone (sync_dir, remove_dir, unlink):
          \* password refused, Slot.revision cleared the same way, so the next tick writes a copy.
          /\ slotId # None /\ faults < MaxFaults
          /\ faults' = faults + 1
          /\ slotRev' = -1
          /\ disk' = [disk EXCEPT ![slotId] = {}]
          /\ confirmed' = [confirmed EXCEPT ![slotId] = 0]
          /\ UNCHANGED << protected, dirty, slotId, entries >>

\* cmd/file.rs encrypt with no password: dirty is set, the revision is not.
RemoveEncryption ==
    /\ active /\ protected
    /\ protected' = FALSE /\ dirty' = TRUE
    /\ UNCHANGED << text, rev, active, savedTexts, slotId, slotRev, copyRefused, job, ready, listed,
                    held, entries, disk, confirmed, replacedWork, edits, faults, crashes >>

\* Session::recover { restore } from an untouched blank document (Request.replace). A restore
\* from a used document opens as another document, which this model leaves out.
RequestRestore ==
    /\ active /\ job.kind = "none" /\ held.id = None /\ Blank
    /\ \E i \in Visible : job' = Job("load", i, 0, "run", 0, 0, "", rev)
    /\ UNCHANGED << text, rev, dirty, protected, active, savedTexts, slotId, slotRev, copyRefused,
                    ready, listed, held, entries, disk, confirmed, replacedWork, edits, faults,
                    crashes >>

\* Session::recover { discard }: the user throws away a copy that the list shows.
RequestDiscard ==
    /\ job.kind = "none"
    /\ \E i \in Visible : job' = Job("discard", i, 0, "run", 0, 0, "", 0)
    /\ UNCHANGED << text, rev, dirty, protected, active, savedTexts, slotId, slotRev, copyRefused,
                    ready, listed, held, entries, disk, confirmed, replacedWork, edits, faults,
                    crashes >>

----
\* The frame loop: Session::recovery_tick.

\* Recovery::tick. The interval (Slot.due, Slot.retry) is abstract: any tick may find it passed.
Tick ==
    /\ ready /\ job.kind = "none"
    /\ IF dirty /\ ~protected
       THEN /\ slotRev # rev
            /\ \/ /\ slotId # None
                  /\ job' = Job("save", slotId, text, "temp", 0, 0, "", 0)
                  /\ slotId' = slotId
               \/ /\ slotId = None /\ Fresh # {}
                  /\ \E i \in Fresh :
                        /\ i = CHOOSE f \in Fresh : TRUE
                        /\ job' = Job("save", i, text, "temp", 0, 0, "", 0)
                        /\ slotId' = i
            /\ slotRev' = rev
       ELSE /\ slotId # None
            /\ job' = Job("discard", slotId, 0, "run", 0, 0, "own", 0)
            /\ UNCHANGED << slotId, slotRev >>
    /\ UNCHANGED << text, rev, dirty, protected, active, savedTexts, copyRefused, ready, listed,
                    held, entries, disk, confirmed, replacedWork, edits, faults, crashes >>

\* Session::open_held_restore, Request.replace.
Open(h) ==
    /\ held' = NoHeld
    /\ IF rev = h.rev /\ ~protected
       THEN \* set_document, dirty = true, Slot::adopt(id, rev).
            /\ replacedWork' = (replacedWork \/ text # Blank0)
            /\ text' = h.text /\ rev' = rev + 1 /\ dirty' = TRUE
            /\ slotId' = h.id /\ slotRev' = rev + 1 /\ copyRefused' = FALSE
       ELSE \* "Recovery cancelled: the document changed while the copy was loading."
            UNCHANGED << replacedWork, text, rev, dirty, slotId, slotRev, copyRefused >>

Loaded == job.kind = "load" /\ job.why = "ok"
LoadedCopy == [id |-> job.id, rev |-> job.reqRev, text |-> job.text]

\* Recovery::receive + Session::apply_recovery + Slot::apply. A copy loaded for a document that
\* is not shown waits in Recovery::held.
Receive ==
    /\ job.kind # "none" /\ job.step = "answered" /\ ~(Loaded /\ active)
    /\ job' = NoJob /\ ready' = TRUE /\ listed' = Inventory
    /\ slotRev' = IF \/ job.kind = "save" /\ job.why = "transient"
                     \/ job.kind = "discard" /\ job.why = "failed" /\ slotId = job.id
                  THEN -1 ELSE slotRev
    /\ copyRefused' = (copyRefused \/ (job.kind = "save" /\ job.why = "permanent"))
    /\ slotId' = IF job.kind = "discard" /\ job.why = "ok" /\ slotId = job.id THEN None ELSE slotId
    /\ held' = IF Loaded THEN LoadedCopy ELSE held
    /\ UNCHANGED << text, rev, dirty, protected, active, savedTexts, entries, disk, confirmed,
                    replacedWork, edits, faults, crashes >>

\* The same Session::recovery_tick: a copy loaded for the shown document opens in that frame.
ReceiveAndOpen ==
    /\ job.step = "answered" /\ Loaded /\ active
    /\ job' = NoJob /\ ready' = TRUE /\ listed' = Inventory
    /\ Open(LoadedCopy)
    /\ UNCHANGED << protected, active, savedTexts, entries, disk, confirmed, edits, faults, crashes >>

\* A later Session::recovery_tick, once the document that asked is shown again.
OpenHeld ==
    /\ held.id # None /\ active
    /\ Open(held)
    /\ UNCHANGED << protected, active, savedTexts, job, ready, listed, entries, disk, confirmed,
                    edits, faults, crashes >>

----
\* The worker thread: one job at a time, Store.

UnchangedWorkerUI == UNCHANGED << text, rev, dirty, protected, active, savedTexts, slotId, slotRev,
                                  copyRefused, ready, listed, held, replacedWork, edits, crashes >>

\* Store::scan: each record's newest readable generation.
WorkerScan ==
    /\ job.kind = "scan" /\ job.step = "run"
    /\ entries' = [i \in Ids |-> Newest(i)]
    /\ job' = Answer(job, "ok")
    /\ UnchangedWorkerUI /\ UNCHANGED << disk, confirmed, faults >>

\* Store::write_snapshot, up to read_snapshot(&tmp): pending-*.tmp written, synced, read back.
WorkerWriteTemp ==
    /\ job.kind = "save" /\ job.step = "temp"
    /\ \/ /\ job' = [job EXCEPT !.step = "rename", !.gen = Newest(job.id) + 1]
          /\ UNCHANGED faults
       \/ /\ faults < MaxFaults /\ faults' = faults + 1
          /\ job' = Answer(job, "transient")
       \/ /\ faults < MaxFaults /\ faults' = faults + 1
          /\ job' = Answer(job, "permanent")
    /\ UnchangedWorkerUI /\ UNCHANGED << entries, disk, confirmed >>

\* fs::rename(&tmp, &final_path).
WorkerRename ==
    /\ job.kind = "save" /\ job.step = "rename"
    /\ disk' = [disk EXCEPT ![job.id] = @ \cup {[gen |-> job.gen, text |-> job.text, synced |-> FALSE]}]
    /\ job' = [job EXCEPT !.step = "sync"]
    /\ UnchangedWorkerUI /\ UNCHANGED << entries, confirmed, faults >>

\* sync_dir(&dir), then self.entries.insert. `prev` is the `previous` the prune spares.
WorkerSyncDir ==
    /\ job.kind = "save" /\ job.step = "sync"
    /\ \/ /\ disk' = [disk EXCEPT ![job.id] = {[g EXCEPT !.synced = TRUE] : g \in @}]
          /\ entries' = [entries EXCEPT ![job.id] = job.gen]
          /\ confirmed' = [confirmed EXCEPT ![job.id] = job.gen]
          /\ job' = [job EXCEPT !.step = "prune", !.prev = entries[job.id]]
          /\ UNCHANGED faults
       \/ /\ faults < MaxFaults /\ faults' = faults + 1
          /\ job' = Answer(job, "transient")
          /\ UNCHANGED << disk, entries, confirmed >>
    /\ UnchangedWorkerUI

\* The loop after the rename: every generation goes but the new one and `previous`.
WorkerPrune ==
    /\ job.kind = "save" /\ job.step = "prune"
    /\ disk' = [disk EXCEPT ![job.id] = {g \in @ : g.gen = job.gen \/ g.gen = job.prev}]
    /\ job' = Answer(job, "ok")
    /\ UnchangedWorkerUI /\ UNCHANGED << entries, confirmed, faults >>

\* Store::load.
WorkerLoad ==
    /\ job.kind = "load" /\ job.step = "run"
    /\ IF \E g \in disk[job.id] : g.gen = entries[job.id]
       THEN job' = [Answer(job, "ok") EXCEPT
                      !.text = (CHOOSE g \in disk[job.id] : g.gen = entries[job.id]).text]
       ELSE job' = Answer(job, "failed")
    /\ UnchangedWorkerUI /\ UNCHANGED << entries, disk, confirmed, faults >>

\* Store::discard. Asking for it means the copy is no longer promised to anyone.
WorkerDiscard ==
    /\ job.kind = "discard" /\ job.step = "run"
    /\ \/ /\ disk' = [disk EXCEPT ![job.id] = {}]
          /\ entries' = [entries EXCEPT ![job.id] = 0]
          /\ confirmed' = [confirmed EXCEPT ![job.id] = 0]
          /\ job' = Answer(job, "ok")
          /\ UNCHANGED faults
       \/ /\ faults < MaxFaults /\ faults' = faults + 1
          /\ job' = Answer(job, "failed")
          /\ UNCHANGED << disk, entries, confirmed >>
       \/ /\ faults < MaxFaults /\ faults' = faults + 1
          /\ disk' = [disk EXCEPT ![job.id] = {}]
          /\ confirmed' = [confirmed EXCEPT ![job.id] = 0]
          /\ job' = Answer(job, "failed")
          /\ UNCHANGED entries
    /\ UnchangedWorkerUI

----
\* The machine. Memory is gone. A process crash keeps every renamed file; a power loss can lose
\* the renames sync_dir has not made durable. The next run starts blank and scans.
Crash ==
    /\ crashes < MaxCrashes /\ crashes' = crashes + 1
    /\ \/ disk' = disk
       \/ disk' = [i \in Ids |-> {g \in disk[i] : g.synced}]
    /\ text' = Blank0 /\ rev' = 0 /\ dirty' = FALSE /\ protected' = FALSE /\ active' = TRUE
    /\ slotId' = None /\ slotRev' = -1 /\ copyRefused' = FALSE
    /\ job' = ScanJob /\ ready' = FALSE /\ listed' = {} /\ held' = NoHeld
    /\ entries' = [i \in Ids |-> 0]
    /\ UNCHANGED << savedTexts, confirmed, replacedWork, edits, faults >>

\* Nothing left to do: only the deadlock check needs it, and a stuck worker still deadlocks.
Idle == job.kind = "none" /\ UNCHANGED vars

Next ==
    \/ Edit \/ SaveDocument \/ SwitchDocument \/ EncryptDocument \/ RemoveEncryption
    \/ RequestRestore \/ RequestDiscard
    \/ Tick \/ Receive \/ ReceiveAndOpen \/ OpenHeld
    \/ WorkerScan \/ WorkerWriteTemp \/ WorkerRename \/ WorkerSyncDir \/ WorkerPrune
    \/ WorkerLoad \/ WorkerDiscard
    \/ Crash \/ Idle

----
\* The promises.

\* A dirty document the slot calls covered really has that text in its record, so a crash now
\* would not lose it. A refused copy is reported to the user, so it is exempt.
UnsavedWorkHasCopy ==
    (/\ dirty /\ ~protected /\ slotRev = rev /\ job.kind = "none"
     /\ ~copyRefused /\ text \notin savedTexts)
    => /\ slotId # None
       /\ \E g \in disk[slotId] : g.text = text

\* The copy a write confirmed, or a newer one, is on disk until someone asks to discard it.
NewestCopySurvives ==
    \A i \in Ids : confirmed[i] > 0 => \E g \in disk[i] : g.gen >= confirmed[i]

\* A password-protected document's text is never in a plaintext recovery copy. Every blank
\* document has the same text value, and an empty one holds nothing to protect.
NoPlaintextOfProtected ==
    (protected /\ text # Blank0) => \A i \in Ids : \A g \in disk[i] : g.text # text

\* A restore replaces only a document that holds no text.
RestoreOnlyReplacesBlank == ~replacedWork

====
