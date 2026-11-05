---- MODULE RecoveryLock ----
\* Two or three running WordCraft processes sharing one recovery record: the advisory lock on
\* owner.lock in crates/engine/src/recovery/desktop.rs (Store::lock, acquire, scan, write_snapshot,
\* load, discard). One action is one system call, or a run of them that nothing can come between in
\* a way that changes the result. A flock belongs to the inode that was open when it was taken, not
\* to the path, which is what this model is here to look at.
EXTENDS Integers, FiniteSets

CONSTANTS
    Procs,      \* the running processes
    Creator,    \* the process that made up this record's id (Recovery::tick, new_id)
    MaxOps,     \* saves, scans, restores and discards started, across all processes
    MaxCrashes  \* processes that die; the OS releases their locks

NoInode == 0
Inodes == 1..(MaxOps + 2)

VARIABLES
    dirExists,   \* the record's folder
    lockPath,    \* the inode the path owner.lock names, NoInode when it does not exist
    nextInode,   \* the next inode a create makes
    holder,      \* inode -> the process holding its flock, or "nobody"
    copies,      \* snapshot-*.json files exist. Generations are RecoveryStore.tla's business.
    pending,     \* the processes whose pending-*.tmp is in the folder
    proc,        \* process -> what it is doing and holding, see Idle below
    ops, crashes

vars == << dirExists, lockPath, nextInode, holder, copies, pending, proc, ops, crashes >>

\* pc:     the next step
\* op:     the Store call under way: "save", "scan", "load" or "discard"
\* why:    who asked for a discard: "job", "reclaim" (Store::scan) or "failedSave" (Store::save)
\* fd:     the file Store::lock opened and is trying to lock
\* guard:  the lock scan holds while it reads the record
\* lock:   Store.locks[id]. A process with an entry never locks again (Store::acquire)
\* kept:   Store::reclaim found the lock already in Store.locks, so it leaves it there
\* slot:   an open document keeps its copy in this record (Slot.id)
\* listed: Store.entries has the record
\* alive:  the process is running
Idle(slot, alive) == [pc |-> IF alive THEN "idle" ELSE "dead", op |-> "", why |-> "", fd |-> NoInode,
                      guard |-> NoInode, lock |-> NoInode, kept |-> FALSE, slot |-> slot, listed |-> FALSE,
                      alive |-> alive]

Init ==
    /\ dirExists = FALSE /\ lockPath = NoInode /\ nextInode = 1
    /\ holder = [i \in Inodes |-> "nobody"]
    /\ copies = FALSE /\ pending = {}
    /\ proc = [p \in Procs |-> Idle(p = Creator, TRUE)]
    /\ ops = 0 /\ crashes = 0

\* The flock on `i`, if `p` is the one holding it, goes when the descriptor closes.
Release(p, i) == [holder EXCEPT ![i] = IF @ = p THEN "nobody" ELSE @]
Go(p, pc) == proc' = [proc EXCEPT ![p].pc = pc]
Finish(p) == proc' = [proc EXCEPT ![p].pc = "idle", ![p].op = "", ![p].why = ""]
Start(p) == proc[p].pc = "idle" /\ ops < MaxOps /\ ops' = ops + 1

FsUnchanged == UNCHANGED << dirExists, lockPath, nextInode, holder, copies, pending >>
Quiet == UNCHANGED << ops, crashes >>

----
\* Store::lock. private_file(owner.lock, create) does not create the folder.

\* What a discard does under the lock: a job deletes, and cleaning up after a scan or a failed save
\* looks at the record again first (Store::reclaim).
DiscardLocked(p) == IF proc[p].why = "job" THEN "discardDelete" ELSE "discardCheck"

\* What the Store call does once it has the lock. scan keeps it in a guard; everything else
\* goes through Store::acquire and keeps it in Store.locks.
Locked(p, i) ==
    IF proc[p].op = "scan"
    THEN proc' = [proc EXCEPT ![p].pc = "scanRead", ![p].fd = NoInode, ![p].guard = i]
    ELSE proc' = [proc EXCEPT ![p].fd = NoInode, ![p].lock = i,
                              ![p].pc = CASE proc[p].op = "save" -> "saveFiles"
                                          [] proc[p].op = "load" -> "loadRead"
                                          [] proc[p].op = "discard" -> DiscardLocked(p)]

\* A lock that failed, in each caller's words. A record scan could not lock is skipped, so the
\* `self.entries = entries` at its end drops it from the list.
LockFailed(p) ==
    IF proc[p].op = "save"
    THEN proc' = [proc EXCEPT ![p].pc = "saveFailed", ![p].fd = NoInode]
    ELSE proc' = [proc EXCEPT ![p].pc = "idle", ![p].op = "", ![p].why = "", ![p].fd = NoInode,
                              ![p].listed = IF proc[p].op = "scan" THEN FALSE ELSE @]

LockOpen(p) ==
    /\ proc[p].pc = "lockOpen"
    /\ IF ~dirExists
       THEN /\ LockFailed(p)
            /\ FsUnchanged
       ELSE IF lockPath = NoInode
            THEN /\ lockPath' = nextInode /\ nextInode' = nextInode + 1
                 /\ proc' = [proc EXCEPT ![p].pc = "lockTry", ![p].fd = nextInode]
                 /\ UNCHANGED << dirExists, holder, copies, pending >>
            ELSE /\ proc' = [proc EXCEPT ![p].pc = "lockTry", ![p].fd = lockPath]
                 /\ FsUnchanged
    /\ Quiet

\* file.try_lock(). LOCK_RETRIES and the 10 ms sleep are abstract: a blocked process may try again
\* as often as it likes, or give up.
LockTry(p) ==
    /\ proc[p].pc = "lockTry" /\ holder[proc[p].fd] = "nobody"
    /\ holder' = [holder EXCEPT ![proc[p].fd] = p]
    /\ Go(p, "lockRecheck")
    /\ UNCHANGED << dirExists, lockPath, nextInode, copies, pending >> /\ Quiet

LockGiveUp(p) ==
    /\ proc[p].pc = "lockTry" /\ holder[proc[p].fd] # "nobody"
    /\ LockFailed(p)
    /\ FsUnchanged /\ Quiet

\* names_file: the path must still name the file we locked, or that file is on its way out. The
\* descriptor closes, and the file is opened again. A folder that went with it makes that fail.
LockRecheck(p) ==
    /\ proc[p].pc = "lockRecheck"
    /\ IF lockPath = proc[p].fd
       THEN /\ Locked(p, proc[p].fd)
            /\ UNCHANGED holder
       ELSE /\ holder' = Release(p, proc[p].fd)
            /\ proc' = [proc EXCEPT ![p].pc = "lockOpen", ![p].fd = NoInode]
    /\ UNCHANGED << dirExists, lockPath, nextInode, copies, pending >> /\ Quiet

\* Store::acquire: an entry in Store.locks is believed without looking.
Acquire(p, afterwards) ==
    IF proc[p].lock # NoInode THEN afterwards ELSE "lockOpen"

----
\* Store::write_snapshot and Store::save, for a document whose copy this record keeps.

\* `if !dir.exists() { private_dir(&dir) }`, then self.acquire.
SaveStart(p) ==
    /\ Start(p) /\ proc[p].slot
    /\ dirExists' = TRUE
    /\ proc' = [proc EXCEPT ![p].op = "save", ![p].pc = Acquire(p, "saveFiles")]
    /\ UNCHANGED << lockPath, nextInode, holder, copies, pending, crashes >>

\* Self::files(&dir)?
SaveFiles(p) ==
    /\ proc[p].pc = "saveFiles"
    /\ Go(p, IF dirExists THEN "saveTemp" ELSE "saveFailed")
    /\ FsUnchanged /\ Quiet

\* private_file(&tmp, true), written and synced.
SaveTemp(p) ==
    /\ proc[p].pc = "saveTemp"
    /\ IF dirExists
       THEN /\ pending' = pending \cup {p}
            /\ Go(p, "saveRename")
       ELSE /\ Go(p, "saveFailed")
            /\ UNCHANGED pending
    /\ UNCHANGED << dirExists, lockPath, nextInode, holder, copies >> /\ Quiet

\* fs::rename(&tmp, &final_path), and the sync, self.entries.insert and prune after it. The temporary
\* file is gone if another process's scan or discard cleaned it.
SaveRename(p) ==
    /\ proc[p].pc = "saveRename"
    /\ IF p \in pending
       THEN /\ copies' = TRUE /\ pending' = pending \ {p}
            /\ proc' = [proc EXCEPT ![p].pc = "idle", ![p].op = "", ![p].listed = TRUE]
       ELSE /\ Go(p, "saveFailed")
            /\ UNCHANGED << copies, pending >>
    /\ UNCHANGED << dirExists, lockPath, nextInode, holder >> /\ Quiet

\* Store::save: a failed write discards its record if that holds no generation. Self::files fails
\* on a missing folder, so a folder someone removed is not discarded, and Store.locks keeps its entry.
SaveFailed(p) ==
    /\ proc[p].pc = "saveFailed"
    /\ IF dirExists /\ ~copies
       THEN proc' = [proc EXCEPT ![p].op = "discard", ![p].why = "failedSave", ![p].pc = "discardStart"]
       ELSE Finish(p)
    /\ FsUnchanged /\ Quiet

----
\* Store::scan, for this record. `guard` is None when Store.locks has an entry: the scan then reads
\* the record without taking the lock.

\* self.directories(), then self.lock(id) unless Store.locks has an entry. A record that can't be
\* locked is skipped.
ScanStart(p) ==
    /\ Start(p)
    /\ IF ~dirExists
       THEN proc' = [proc EXCEPT ![p].listed = FALSE]
       ELSE proc' = [proc EXCEPT ![p].op = "scan",
                                 ![p].pc = IF proc[p].lock # NoInode THEN "scanRead" ELSE "lockOpen"]
    /\ UNCHANGED << dirExists, lockPath, nextInode, holder, copies, pending, crashes >>

\* The loop that deletes pending-*.tmp, then the one that reads the generations. fs::read_dir on a
\* folder someone removed fails the whole scan, and the guard goes with it.
ScanRead(p) ==
    /\ proc[p].pc = "scanRead"
    /\ IF dirExists
       THEN /\ pending' = {}
            /\ proc' = [proc EXCEPT ![p].listed = copies, ![p].pc = "scanRelease"]
            /\ UNCHANGED holder
       ELSE /\ holder' = IF proc[p].guard # NoInode THEN Release(p, proc[p].guard) ELSE holder
            /\ proc' = [proc EXCEPT ![p].guard = NoInode, ![p].pc = "idle", ![p].op = ""]
            /\ UNCHANGED pending
    /\ UNCHANGED << dirExists, lockPath, nextInode, copies >> /\ Quiet

\* drop(guard).
ScanRelease(p) ==
    /\ proc[p].pc = "scanRelease"
    /\ holder' = IF proc[p].guard # NoInode THEN Release(p, proc[p].guard) ELSE holder
    /\ proc' = [proc EXCEPT ![p].guard = NoInode, ![p].pc = "scanEmpty"]
    /\ UNCHANGED << dirExists, lockPath, nextInode, copies, pending >> /\ Quiet

\* `if Self::files(&dir)?.is_empty() && let Err(e) = self.discard(id)`: looked at with no lock held.
ScanEmpty(p) ==
    /\ proc[p].pc = "scanEmpty"
    /\ IF dirExists /\ ~copies
       THEN proc' = [proc EXCEPT ![p].op = "discard", ![p].why = "reclaim", ![p].pc = "discardStart"]
       ELSE Finish(p)
    /\ FsUnchanged /\ Quiet

----
\* Store::load: a copy from the list, for Recovery::restore.

LoadStart(p) ==
    /\ Start(p) /\ proc[p].listed /\ ~proc[p].slot
    /\ proc' = [proc EXCEPT ![p].op = "load", ![p].pc = Acquire(p, "loadRead")]
    /\ UNCHANGED << dirExists, lockPath, nextInode, holder, copies, pending, crashes >>

\* read_snapshot. On success the document opens and Slot::adopt keeps its copy here. On failure the
\* lock stays in Store.locks, with no document behind it.
LoadRead(p) ==
    /\ proc[p].pc = "loadRead"
    /\ proc' = [proc EXCEPT ![p].slot = copies, ![p].pc = "idle", ![p].op = ""]
    /\ FsUnchanged /\ Quiet

----
\* Store::discard.

\* Job::Discard: the user discards a listed copy, or a document no longer needs its own.
DiscardJob(p) ==
    /\ Start(p) /\ (proc[p].listed \/ proc[p].slot)
    /\ proc' = [proc EXCEPT ![p].op = "discard", ![p].why = "job", ![p].pc = "discardStart"]
    /\ UNCHANGED << dirExists, lockPath, nextInode, holder, copies, pending, crashes >>

\* What a finished job discard does to the document's slot: Applied::Discarded clears it.
Discarded(p) ==
    proc' = [proc EXCEPT ![p].pc = "idle", ![p].op = "", ![p].why = "",
                         ![p].slot = IF proc[p].why = "job" THEN FALSE ELSE @]

\* `if !dir.exists() { self.entries.remove(id); return Ok(()) }`, then self.acquire. Store.locks
\* keeps any entry it had. Store::reclaim's acquire fails on a missing folder instead, which ends
\* the same way for a reclaim: nothing else happens.
DiscardStart(p) ==
    /\ proc[p].pc = "discardStart"
    /\ IF dirExists
       THEN proc' = [proc EXCEPT ![p].pc = Acquire(p, DiscardLocked(p)),
                                 ![p].kept = proc[p].why # "job" /\ proc[p].lock # NoInode]
       ELSE /\ Discarded(p)
    /\ FsUnchanged /\ Quiet

\* Store::reclaim, under the lock: `Self::files` again, because the look its caller made may be
\* stale. A record that is not empty stays, and a lock taken only for this look is released.
DiscardCheck(p) ==
    /\ proc[p].pc = "discardCheck"
    /\ IF dirExists /\ ~copies
       THEN /\ proc' = [proc EXCEPT ![p].pc = "discardDelete", ![p].kept = FALSE]
            /\ UNCHANGED holder
       ELSE /\ holder' = IF proc[p].kept THEN holder ELSE Release(p, proc[p].lock)
            /\ proc' = [proc EXCEPT ![p].lock = IF proc[p].kept THEN @ ELSE NoInode, ![p].kept = FALSE,
                                    ![p].pc = "idle", ![p].op = "", ![p].why = ""]
    /\ UNCHANGED << dirExists, lockPath, nextInode, copies, pending >> /\ Quiet

\* The entry check, deleting the generations and the temporary files, sync_dir, self.entries.remove.
\* read_dir on a folder someone removed fails, and the lock acquire took stays in Store.locks.
DiscardDelete(p) ==
    /\ proc[p].pc = "discardDelete"
    /\ IF dirExists
       THEN /\ copies' = FALSE /\ pending' = {}
            /\ proc' = [proc EXCEPT ![p].listed = FALSE, ![p].pc = "discardUnlink"]
       ELSE /\ Finish(p)
            /\ UNCHANGED << copies, pending >>
    /\ UNCHANGED << dirExists, lockPath, nextInode, holder >> /\ Quiet

\* fs::remove_file(owner.lock). It unlinks whatever the path names, and fails if nothing does: that
\* failure is returned once the lock is released.
DiscardUnlink(p) ==
    /\ proc[p].pc = "discardUnlink"
    /\ lockPath' = NoInode
    /\ Go(p, IF lockPath # NoInode THEN "discardRmdir" ELSE "discardReleaseFailed")
    /\ UNCHANGED << dirExists, nextInode, holder, copies, pending >> /\ Quiet

FolderEmpty == ~copies /\ pending = {} /\ lockPath = NoInode

\* fs::remove_dir(&dir). It fails while anything is in the folder.
DiscardRmdir(p) ==
    /\ proc[p].pc = "discardRmdir"
    /\ IF dirExists /\ FolderEmpty
       THEN /\ dirExists' = FALSE
            /\ Go(p, "discardRelease")
       ELSE /\ Go(p, "discardReleaseRetry")
            /\ UNCHANGED dirExists
    /\ UNCHANGED << lockPath, nextInode, holder, copies, pending >> /\ Quiet

\* self.locks.remove(id), last: after the folder went ("discardRelease"), after remove_dir failed
\* ("discardReleaseRetry"), or after the unlink failed ("discardReleaseFailed").
DiscardRelease(p) ==
    /\ proc[p].pc \in {"discardRelease", "discardReleaseRetry", "discardReleaseFailed"}
    /\ holder' = Release(p, proc[p].lock)
    /\ IF proc[p].pc = "discardReleaseRetry"
       THEN proc' = [proc EXCEPT ![p].lock = NoInode, ![p].pc = "discardRmdirAgain"]
       ELSE proc' = [proc EXCEPT ![p].lock = NoInode, ![p].pc = "idle", ![p].op = "", ![p].why = "",
                                 ![p].slot = IF proc[p].pc = "discardRelease" /\ proc[p].why = "job"
                                             THEN FALSE ELSE @]
    /\ UNCHANGED << dirExists, lockPath, nextInode, copies, pending >> /\ Quiet

\* The second fs::remove_dir(&dir), with no lock held. This model's unlink always takes the file out
\* of the folder, so here only something another process put there, or the folder's being gone, can
\* make either remove_dir fail.
DiscardRmdirAgain(p) ==
    /\ proc[p].pc = "discardRmdirAgain"
    /\ IF dirExists /\ FolderEmpty
       THEN /\ dirExists' = FALSE
            /\ Discarded(p)
       ELSE /\ Finish(p)
            /\ UNCHANGED dirExists
    /\ UNCHANGED << lockPath, nextInode, holder, copies, pending >> /\ Quiet

----
\* A process dies: its descriptors close, its files stay.
Crash(p) ==
    /\ proc[p].alive /\ crashes < MaxCrashes /\ crashes' = crashes + 1
    /\ holder' = [i \in Inodes |-> IF holder[i] = p THEN "nobody" ELSE holder[i]]
    /\ proc' = [proc EXCEPT ![p] = Idle(FALSE, FALSE)]
    /\ UNCHANGED << dirExists, lockPath, nextInode, copies, pending, ops >>

\* Nothing left to do: only the deadlock check needs it. A blocked lock can always give up.
Done == \A p \in Procs : proc[p].pc \in {"idle", "dead"}
Stop == Done /\ UNCHANGED vars

Next ==
    \/ \E p \in Procs :
          \/ LockOpen(p) \/ LockTry(p) \/ LockGiveUp(p) \/ LockRecheck(p)
          \/ SaveStart(p) \/ SaveFiles(p) \/ SaveTemp(p) \/ SaveRename(p) \/ SaveFailed(p)
          \/ ScanStart(p) \/ ScanRead(p) \/ ScanRelease(p) \/ ScanEmpty(p)
          \/ LoadStart(p) \/ LoadRead(p)
          \/ DiscardJob(p) \/ DiscardStart(p) \/ DiscardCheck(p) \/ DiscardDelete(p) \/ DiscardUnlink(p)
          \/ DiscardRmdir(p) \/ DiscardRelease(p) \/ DiscardRmdirAgain(p)
          \/ Crash(p)
    \/ Stop

----
\* The promise.

\* A process with an open document whose copy is in this record, holding what it believes is the
\* lock. It stops being one once its own discard has deleted the record's files: all that is left
\* is removing the empty folder, and the new owner.lock another process makes only fails that step.
DiscardTail == {"discardUnlink", "discardRmdir", "discardRelease", "discardReleaseRetry",
                "discardReleaseFailed", "discardRmdirAgain"}
LiveOwner(p) == proc[p].alive /\ proc[p].slot /\ proc[p].lock # NoInode /\ proc[p].pc \notin DiscardTail

\* About to read the record's files (and list it), hand it to a document, or delete it.
Touching(q) == proc[q].pc \in {"scanRead", "loadRead", "discardCheck", "discardDelete"}

\* A record with a live owner is never scanned, listed, restored or discarded by another process.
LiveOwnerIsAlone == \A p, q \in Procs : (p # q /\ LiveOwner(p)) => ~Touching(q)

\* Cleaning up an empty record, after a scan or a failed save, never deletes a committed copy.
ReclaimOnlyEmpty == \A p \in Procs : ~(proc[p].pc = "discardDelete" /\ proc[p].why # "job" /\ copies)

====
