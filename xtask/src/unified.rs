//! `cargo xtask unified`: every package the normal build uses gets the same features in the test
//! build (#44).
//!
//! Cargo turns on a dev-dependency's features only when it builds tests, and a package built with
//! other features is another build, with other builds of everything above it. When the two sets
//! differ, `cargo build` and `cargo test --workspace` each compile every workspace library above
//! that package, so an edit gets compiled twice. The fix is a normal dependency that turns the
//! same features on (see the "Feature unification" comments in the crates' Cargo.toml), or a
//! `#[doc(hidden)]` helper in place of a test-only feature of a workspace crate.

use std::collections::{BTreeMap, BTreeSet};

/// Package (`name vX (source)`) → its features, as `cargo tree` resolves them for `edges`.
fn features(edges: &str) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let out = crate::cargo()
        .args(["tree", "--color", "never", "--workspace", "--locked", "--prefix", "none", "-f", "{p}|{f}", "-e", edges])
        .output()
        .map_err(|e| format!("cargo tree: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo tree -e {edges} failed:\n{}", String::from_utf8_lossy(&out.stderr)));
    }
    let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim_end().trim_end_matches(" (*)");
        let Some((pkg, feats)) = line.split_once('|') else { continue };
        // A proc macro or build dependency is listed as `name vX (proc-macro)`; the same version
        // can also be a normal dependency, so the marker is part of the key.
        let set = map.entry(pkg.trim().to_string()).or_default();
        set.extend(feats.split(',').map(str::trim).filter(|f| !f.is_empty()).map(String::from));
    }
    Ok(map)
}

pub fn run() -> Result<(), String> {
    let normal = features("normal,build")?;
    let test = features("normal,build,dev")?;
    let mut differ = Vec::new();
    for (pkg, feats) in &normal {
        let Some(t) = test.get(pkg) else { continue };
        if t != feats {
            let extra: Vec<&str> = t.difference(feats).map(String::as_str).collect();
            differ.push(format!("{pkg}: the test build adds [{}]", extra.join(", ")));
        }
    }
    if differ.is_empty() {
        println!("OK: the normal and test builds use the same features for all {} packages they share.", normal.len());
        return Ok(());
    }
    println!("{} package(s) get more features in the test build:", differ.len());
    for d in &differ {
        println!("  - {d}");
    }
    println!(
        "\nEach one makes `cargo build` and `cargo test --workspace` compile the workspace crates above it twice.\n\
         Turn the same features on in a normal dependency of the lowest workspace crate that uses the package\n\
         (`cargo tree -i <package> -e normal`), with a \"Feature unification\" comment, or replace a workspace\n\
         crate's test-only feature with a `#[doc(hidden)]` item."
    );
    Err(format!("{} package(s) with test-only features", differ.len()))
}
