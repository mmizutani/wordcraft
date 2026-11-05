#!/bin/sh
# Model-checks RecoveryStore.tla. Exit codes:
#   0  the rules hold, each known finding still breaks its rule, the model is in sync with the code
#   1  a rule broke: a new bug, or a model that no longer matches the code (see SKILL.md)
#   2  a known finding stopped breaking its rule: it may be fixed (see SKILL.md)
#   3  covered code changed after the sync commit: the model needs another look (see SKILL.md)
#   4  the tools could not run, or TLC stopped without a result
# Set TLA2TOOLS to a tla2tools.jar and JAVA to a java binary to skip finding them.
set -u
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
version=1.7.4
sha=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88
tmp=$(mktemp -d "${TMPDIR:-/tmp}/recovery-store.XXXXXX") || exit 4

sha_of() { shasum -a 256 "$1" 2>/dev/null | cut -d' ' -f1; }

jar=${TLA2TOOLS:-}
if [ -z "$jar" ]; then
  # A cache folder the sandbox refuses falls back to this run's private folder, never a fixed path
  # under the temporary folder: there another local user could swap the jar after its hash check.
  cache=${XDG_CACHE_HOME:-$HOME/.cache}/wordcraft-tla
  mkdir -p "$cache" 2>/dev/null && [ -w "$cache" ] || cache=$tmp
  jar=$cache/tla2tools-$version.jar
  if [ ! -f "$jar" ]; then
    curl -fsSL --max-time 120 -o "$jar.part" \
      "https://github.com/tlaplus/tlaplus/releases/download/v$version/tla2tools.jar" \
      && mv -f "$jar.part" "$jar" || { echo "cannot download tla2tools.jar; set TLA2TOOLS" >&2; exit 4; }
  fi
fi
[ "$(sha_of "$jar")" = "$sha" ] || { echo "$jar is not tla2tools $version (SHA-256 differs)" >&2; exit 4; }

java_bin=${JAVA:-}
if [ -z "$java_bin" ] && command -v mise >/dev/null 2>&1; then
  java_bin=$(mise which java 2>/dev/null || true)
fi
[ -n "$java_bin" ] || java_bin=java
"$java_bin" -version >/dev/null 2>&1 || { echo "no working Java (11 or later); set JAVA" >&2; exit 4; }

# Java's default temporary folder can be outside the sandbox, and TLC unpacks its standard modules
# there: without this it reports only "Parsing or semantic analysis failed".
tlc() {
  (cd "$here" && "$java_bin" -XX:+UseParallelGC "-Djava.io.tmpdir=$tmp" -cp "$jar" tlc2.TLC \
    -nowarning -workers auto -metadir "$tmp/states-$1" -config "$1.cfg" RecoveryStore.tla) \
    >"$tmp/$1.out" 2>&1
  # Out of memory, a model that doesn't parse: nothing was checked, so it says nothing about a rule.
  grep -Eq 'Model checking completed\. No error has been found\.|^Error: Invariant .* is violated|^Error: Deadlock reached' "$tmp/$1.out" \
    || { echo "TLC gave no result for $1.cfg. Full output: $tmp/$1.out" >&2; exit 4; }
}
path_of() {
  grep -E '^State [0-9]+:' "$tmp/$1.out" | sed -E 's/^State ([0-9]+): <([A-Za-z]+).*/\1 \2/; s/^State ([0-9]+): <Initial predicate>/\1 Init/'
}

code=0

echo "== rules (RecoveryStore.cfg)"
tlc RecoveryStore
if grep -q 'Model checking completed. No error has been found.' "$tmp/RecoveryStore.out"; then
  grep -E 'states generated' "$tmp/RecoveryStore.out" | grep -v Progress
else
  grep -E '^Error' "$tmp/RecoveryStore.out" | head -5
  path_of RecoveryStore
  echo "A rule broke. Full output: $tmp/RecoveryStore.out"
  code=1
fi

known_findings=""
for finding in $known_findings; do
  echo "== known finding ($finding.cfg)"
  tlc "$finding"
  if grep -q 'Invariant UnsavedWorkHasCopy is violated' "$tmp/$finding.out"; then
    echo "still breaks UnsavedWorkHasCopy, as recorded"
  else
    grep -E '^Error|No error' "$tmp/$finding.out" | head -5
    echo "It no longer breaks UnsavedWorkHasCopy. Full output: $tmp/$finding.out"
    [ "$code" -ne 0 ] || code=2
  fi
done

echo "== coverage"
sync=$(sed -n 's/^sync-commit: *//p' "$here/SKILL.md")
covered=$(sed -n '/^covered-files:/,/^$/s/^- //p' "$here/SKILL.md")
repo=$(cd "$here" && git rev-parse --show-toplevel)
if ! (cd "$repo" && git rev-parse --verify -q "$sync^{commit}" >/dev/null); then
  echo "sync commit $sync is not in this clone"
  [ "$code" -ne 0 ] || code=3
else
  # shellcheck disable=SC2086 # the covered paths are one per line and hold no spaces
  changed=$(cd "$repo" && git diff --name-only "$sync" -- $covered)
  if [ -n "$changed" ]; then
    echo "changed since $sync:"
    echo "$changed"
    [ "$code" -ne 0 ] || code=3
  else
    echo "in sync with $sync"
  fi
fi

# Exits 1 and 2 print a path under $tmp to read the result from, so they keep the folder.
case "$code" in 1 | 2) ;; *) rm -rf "$tmp" ;; esac
exit "$code"
