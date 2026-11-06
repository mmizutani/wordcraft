# Build speed

Status: 2026-11-06, rev 1 (#44).

How the workspace is set up so that rebuilding after a small edit is quick, what was measured, and
what was tried and left out. Measured in a 4-core, 15 GB Linux container (rustc 1.99.1) with
`CRAFT_I18N_DIR` and `CRAFT_FONTS_DIR` set, as in the Claude cloud sessions.

## What the workspace does

- **The crates edited most are unoptimised in dev builds.** `wordcraft-engine`, `wordcraft-ui-egui`,
  `wordcraft-mcp`, `wordcraft-zotero` and the three apps build at `opt-level = 0`
  (`[profile.dev.package.*]` in `Cargo.toml`); the other workspace crates keep `opt-level = 1`
  and dependencies `opt-level = 2`. The tests spend their time in layout, shaping, the document
  model and the readers and writers, so the full test run is as fast as before. Release builds
  are unchanged.
- **`cargo build` and `cargo test` share one build of each crate.** Cargo turns on a
  dev-dependency's features only for tests, and a crate built with other features is another
  build, with other builds of every crate above it. Test-only features of workspace crates are
  gone (their helpers are `#[doc(hidden)]` items now), and the few third-party features only
  dev-dependencies turned on are also turned on by a normal dependency ("Feature unification"
  comments in the crates' `Cargo.toml`). `cargo xtask unified` (part of `cargo xtask ci`) fails
  when a package gets features in the test build that the normal build doesn't have, and says
  how to fix it.
- Dependencies are built without debug info, workspace crates with line tables only (file and
  line in panics and backtraces).

## Measurements

One `std::hint::black_box(N)` line added to a real function body, then the command timed (warm
target; median of two rounds). "Test build" is the build step of `cargo xtask test`:
`cargo test --workspace --lib --bins --tests --no-run`.

| Edit in | Before: build, then test build | After: build, then test build | Speedup |
|---|---|---|---|
| `ui-egui` (`app.rs`) | 18.8 s + 47.5 s = 66 s | 7.7 s + 9.9 s = 18 s | 3.8× |
| `engine` (`session/history.rs`) | 16.3 s + 46.5 s = 63 s | 11.3 s + 14.0 s = 25 s | 2.5× |
| `doc` (`document.rs`, bottom of the graph) | 26.4 s + 45.6 s = 72 s | 16.9 s + 24.8 s = 42 s | 1.7× |

The test build alone after an edit (no `cargo build` first): ui-egui 44 s → 12–15 s, engine
50 s → 20 s. The full `cargo nextest run` takes the same time as before (about 2 to 2.5 min of
test time here, the long pole being the 60–80 s interface audit). A cold `cargo build` takes about
10.5 min either way (about 9 min of it is dependencies).

## Tried and left out

| Option | Result |
|---|---|
| `opt-level = 0` for every workspace crate | Edits as fast as above, but the full test run went from ~2.5 to 5.5 min (layout and shaping unoptimised). |
| mold instead of the default linker | No gain: Rust already links with its bundled `rust-lld` on x86_64 Linux; linking the 314 MB `wordcraft` binary took 3.7–3.8 s with rust-lld and 4.0–4.3 s with mold 2.30. |
| `split-debuginfo = "unpacked"` | No gain with line tables only (4.4 s for the same link). |
| sccache | 0 Rust cache hits between two target directories (cargo passes target-dir paths into the compiler arguments it hashes), and the cloud containers keep nothing between sessions, so it saves nothing here. |
| Cranelift backend, parallel front end (`-Zthreads`) | Nightly only; the workspace builds on stable. With the top crates at `opt-level = 0`, their codegen is already a few seconds. |
| Unifying duplicate dependencies (four `read-fonts`, three `skrifa`, two `syn`) | They come from egui, vello and krilla pinning different versions; only cold builds would gain. |

## Further options, not done yet

- **Split `wordcraft-engine` (89k lines) and `wordcraft-ui-egui` (90k lines).** An edit in either
  recompiles the whole crate's front end (3–4 s) and its unit-test crate (most of the remaining
  test-build time). Moving the commands (`cmd/`) or the large in-crate test modules into their own
  crates would shrink that, at the cost of a large refactor across open branches.
- **Cold builds in fresh cloud sessions** are dominated by dependencies (about 9 of 10.5 min).
  Only a cache that outlives the container would help.

## Tips

- Give each parallel agent or worktree its own `CARGO_TARGET_DIR`, and delete
  `target/debug/incremental` and old test executables when disk runs low
  (`find target/debug/deps -maxdepth 1 -type f -perm -u+x ! -name '*.*' -delete`).
- Run one crate's tests with `cargo nextest run -p <crate>`; `-p` builds only what that crate
  needs. Its features can differ from the workspace build (another build of the crates below it),
  so alternating between `-p` and `--workspace` compiles those crates in both forms.

## Revision history

- rev 1 (2026-11-06): first version (#44): opt-level 0 for the top crates, one shared build for
  `cargo build` and `cargo test`, `cargo xtask unified`, measurements and rejected options.
