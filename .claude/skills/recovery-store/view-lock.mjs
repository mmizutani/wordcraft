// Words for /statechart on RecoveryLock.tla: whether a record's owner is really protected.
const procs = (v) => Object.values(v.proc);
const owner = (v) =>
  procs(v).find((p) => p.alive && p.slot && p.lock !== 0 && !p.pc.startsWith("discardU") && !p.pc.startsWith("discardR"));
const reading = (p) => ["scanRead", "loadRead", "discardDelete"].includes(p.pc);
export const title = "Two running processes and one recovery record";
export const state = (v) => {
  const o = owner(v);
  if (!o) return v.dirExists ? "Record on disk, no live owner" : "No record";
  if (procs(v).some((p) => p !== o && reading(p))) return "Another process is at a live owner's record";
  return o.lock === v.lockPath ? "Live owner, locked on owner.lock" : "Live owner, locked on a deleted file";
};
const words = {
  LockOpen: "a process opens owner.lock",
  LockTry: "a process wins the lock",
  LockGiveUp: "a process gives up on the lock",
  LockRecheck: "a process checks the path after locking",
  SaveStart: "a save starts, making the folder if needed",
  SaveFiles: "the save lists the generations",
  SaveTemp: "the save writes a temporary copy",
  SaveRename: "the save renames it into place",
  SaveFailed: "the save failed",
  ScanStart: "a scan starts",
  ScanRead: "the scan reads the record",
  ScanRelease: "the scan drops its lock",
  ScanEmpty: "the scan looks whether the record is empty",
  LoadStart: "a restore starts",
  LoadRead: "the restore reads the copy",
  DiscardJob: "a discard starts",
  DiscardStart: "the discard looks for the folder",
  DiscardDelete: "the discard deletes the files",
  DiscardRelease: "the discard drops the lock",
  DiscardUnlink: "the discard unlinks owner.lock",
  DiscardRmdir: "the discard removes the folder",
  Crash: "a process dies",
  Stop: "nothing left to do",
};
export const step = (action) => words[action] ?? null;
