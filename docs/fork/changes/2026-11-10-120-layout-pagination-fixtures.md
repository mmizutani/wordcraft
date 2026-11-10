# Make float pagination regression fixtures independent of host fonts

- **Issue:** #120
- **Upstream doc it would have touched:** `docs/layout-parity.md`

The float-return and footnote ownership fixtures use bundled Source Sans 3 and exact line spacing. The widow fixture uses explicit line breaks and checks the page split with widow control disabled and enabled before inserting the float.
