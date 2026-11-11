# Save private WordCraft documents to the cloud

- Issue [#114](https://github.com/mmizutani/wordcraft/issues/114)
- Upstream gap 22 in [WordCraft gaps](../../gaps.md)

The private Sites editor can save a new or imported DOCX to an owner-only library. After the first Save to Cloud, ordinary Save writes a new revision. A stale editor keeps its local edits and offers a DOCX download or a confirmed reload. The [design and pilot runbook](../cloud-documents.md) describe the storage and deletion behavior.

Only an open started in the window can bring up a dialog. A script's `ui.cloudOpen` of an encrypted document opens nothing and waits: `ui.cloudApply` says a password is needed, and takes one as `password`. If the document changes while a script's open loads, the open is dropped with an error.
