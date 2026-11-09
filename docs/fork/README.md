# This fork's change notes

`mmizutani/wordcraft` follows `storytold/wordcraft`. These upstream docs are read-only here: `ROADMAP.md`, `docs/gaps.md`, `docs/roadmap.md`, `docs/target-app-parity.md`, `docs/file-format-parity.md`, `docs/layout-parity.md`, `docs/typing-parity.md`, `docs/ui-parity.md`, `docs/hardware-parity.md` and `docs/localization-parity.md`.

Every pull request used to rewrite their status lines and append to their logs. So two open pull requests always conflicted on them, and so did every merge from upstream. Stacked pull requests had to merge the same lines upward through each link, and every push restarted CI and review.

## Adding a note

A pull request that would have updated one of those docs adds one new file instead:

```text
docs/fork/changes/YYYY-MM-DD-<issue>-<slug>.md
```

```markdown
# What a person can now do, in one sentence

- **Issue:** #86, AS7.4
- **Upstream doc it would have touched:** `docs/gaps.md` gap 25
- **Not verified:** the check by hand with a real backend

What changed, and anything a later reader can't get from the diff.
```

Name the file from the branch's own work, so two open pull requests can't pick the same name. Correct your own note in its own pull request. Don't add a shared index, a counter or a running total, because any of them would conflict the same way.

## Reading them

The date comes first, so a listing is in order:

```bash
ls docs/fork/changes
```

`git log -- <note>` gives the pull request that added it. Open work stays in GitHub issues.

## Syncing from upstream

A merge from upstream takes upstream's text for the read-only docs. The fork's lines from before this rule are in [`changes/2026-11-09-progress-before-this-policy.md`](changes/2026-11-09-progress-before-this-policy.md).
