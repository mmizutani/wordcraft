# pstack model configuration

Per-role model overrides for pstack skills, for every agent working in this repository. Edit this file by hand: `/setup-pstack` writes a different one, in the user's Claude config folder. Each pstack SKILL.md names its defaults in a Models section; the values here override those defaults. Delete a line to fall back to the skill default. A value of `inherit-parent` or `auto` runs that role on the parent session's model (the `Agent` call omits `model`); an alias entry in a panel list still counts toward that panel's fan-out. A model may carry a reasoning effort, as in `opus @xhigh` (levels: low, medium, high, xhigh, max); the role's subagent then runs at that level, each entry of a panel list on its own. `default effort` sets the level for a value without one; `session` uses the runtime's platform mapping to start from the parent session's effort. On Codex this requires active-turn metadata or an explicit effort choice.

feature, refactoring: sonnet
bug-fix: opus
perf-issue: opus
hillclimb: opus
judgment and prose: opus
strongest judgment: fable
how explorer: opus
how explainer: opus
why investigators: opus
why synthesizer: opus
reflect tooling: opus
reflect judgment, divergent, synthesizer: opus
arena runners: opus, fable, sonnet, haiku
arena cross-judge pool: opus, fable, sonnet, haiku
swarm workers: opus
architect runners: opus, fable, sonnet, haiku
interrogate reviewers: opus, fable, sonnet, haiku
thermos reviewers: opus
no-comments reviewer: opus

default effort: session
