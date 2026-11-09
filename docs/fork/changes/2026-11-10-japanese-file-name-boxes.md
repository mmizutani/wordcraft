# A Japanese file name in the window caption and the Recent list is drawn with glyphs, not boxes

- **Issue:** none filed; reported with screenshots of `秘密保持契約サンプル_条番号自動連番フル活用.docx` drawn as boxes in the caption and in File › Open › Recent
- **Upstream doc it would have touched:** `docs/typing-parity.md` is not involved; no status line changes
- **Not verified:** the app window itself (the Bash sandbox has no GPU adapter for `ui_shot`); the glyph check ran headlessly against the installed fonts

A CJK file name sets `want_system_cjk`, which asks `theme::system_cjk_font` for an installed CJK font. That function only looked for one when the embedded faces left characters of the CJK interface catalogs or the language names uncovered. In a build without craft-i18n (English-only catalogs) and without craft-fonts (no embedded CJK face) both lists are empty, so it returned no font and the name was all boxes. It now returns a font chosen for a character typical of the language (`の`, `页`, `한`) when nothing is embedded and nothing is listed, and still returns none when embedded faces cover the interface.
