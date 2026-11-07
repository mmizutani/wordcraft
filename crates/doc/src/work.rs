//! A count of the characters a paragraph's object lookups walk, the blocks
//! [`crate::Document::para_paths`] visits, and each block lookup by path with the table levels it
//! descends, for the linear-time tests of the crates that use them: an input 8x the size must cost
//! about 8x the steps, which a busy machine can't skew the way it skews a timing. The count is
//! kept in every build (one add to a thread-local cell per walk), so other crates' tests read it
//! through [`count`] without a test-only Cargo feature, and `cargo build` and `cargo test` share
//! one build of this crate (`docs/build-speed.md`).

thread_local! {
    /// The steps counted on this thread.
    static STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Count `n` steps of a walk. Public for the walks other crates do over a paragraph's text
/// themselves (Markdown and HTML comment export, for one).
#[inline(always)]
pub fn steps(n: usize) {
    STEPS.with(|s| s.set(s.get().saturating_add(n as u64)));
}

/// The steps `f` takes, for tests.
#[doc(hidden)]
pub fn count(f: impl FnOnce()) -> u64 {
    let before = STEPS.with(std::cell::Cell::get);
    f();
    STEPS.with(std::cell::Cell::get).saturating_sub(before)
}
