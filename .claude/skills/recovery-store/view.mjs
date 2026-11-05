// Words for /statechart: what the user sees, and whether a crash now would lose work.
const covered = (v) => v.slotId !== "none" && v.disk[v.slotId].some((g) => g.text === v.text);
export const title = "One document's crash recovery";
export const state = (v) => {
  if (!v.ready) return "Starting: reading the recovery folder";
  if (v.protected) return "Password protected, no copy kept";
  if (v.held.id !== "none") return "Restored copy waiting for its document";
  if (!v.dirty) return v.text === 0 ? "Untouched blank document" : "Saved";
  return covered(v) ? "Unsaved, copy on disk" : "Unsaved, no copy on disk";
};
const words = {
  Edit: "you edit the document",
  SaveDocument: "you save",
  SwitchDocument: "you switch documents",
  EncryptDocument: "you set a password",
  RemoveEncryption: "you remove the password",
  RequestRestore: "you ask to restore a copy",
  RequestDiscard: "you discard a listed copy",
  Tick: "the interval passes and a job is queued",
  Receive: "the app takes the worker's answer",
  ReceiveAndOpen: "the loaded copy opens",
  OpenHeld: "the waiting copy opens",
  WorkerScan: "the worker reads the recovery folder",
  WorkerWriteTemp: "the worker writes a temporary copy",
  WorkerRename: "the worker renames it into place",
  WorkerSyncDir: "the worker syncs the folder",
  WorkerPrune: "the worker prunes old copies",
  WorkerLoad: "the worker loads the copy",
  WorkerDiscard: "the worker deletes the copy",
  Crash: "the app crashes",
};
export const step = (action) => words[action] ?? null;
