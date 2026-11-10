# The legal assistant's design is in the repository: playbook review, answering the other side, drafting from a library, and checks

- **Issue:** none yet; a parent issue for LA1–LA9 is to be opened from the design
- **Upstream doc it would have touched:** `docs/gaps.md` gap 25
- **Not verified:** everything in it is design; nothing is built or run

[`docs/legal-assistant.md`](../../legal-assistant.md) designs the Assistant pane's contract work on
top of the assistant as built at `1ab82aff`. A new pure `crates/legal` computes the clause map and
the checks. Findings quote the words they are about and are refused when the quote is not in the
document. A review is a job WordCraft runs over the clause map in batches, so coverage does not
depend on the backend. Playbooks, the clause library and templates are knowledge at the
organisation level (hosted WordCraft, read-only on a desktop), the client wall level and the case
level, and they reach the model as data, never as instructions.

The milestones are LA0 (this design) to LA9 (the evaluation), each a stacked pull request. The
owner decides the questions in the design's last table before LA1 starts.

One read-only review against `1ab82aff` checked every cited path and line. Its twelve findings are
folded in:
- a viewer role for findings-only reviews;
- ranges mapped through `ChangeSet::map` instead of a quote's nth occurrence;
- an explicit `person_only` extension;
- the other side's markup read from the document, not `get_changes`;
- acknowledged clause states instead of a coverage promise;
- invalidation keys beyond clause text;
- a pre-landing leak check that also finds short values;
- the topic table moved below `assist`;
- targets for missing clauses;
- the hosted authorisation that is new, and the Sites probe that must come first.
