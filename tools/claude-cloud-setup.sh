#!/usr/bin/env bash
# Setup script for Claude Code cloud environments (claude.ai/code) working on WordCraft.
#
# Installs what `cargo xtask ci` needs on Ubuntu 24.04, mirroring the shared gate workflow
# (storytold/craft-common-actions gate.yml, inputs in .github/workflows/ci.yml):
#   - the Linux build packages from ci.yml (egui/winit/GTK/fontconfig, plus Mesa's Vulkan
#     driver so wgpu can render headless `ui_shot` screenshots),
#   - Rust stable with clippy, rustfmt and the wasm32-unknown-unknown target,
#   - cargo-nextest at the CI version (xtask uses it when installed),
#   - craft-i18n at craft-i18n.rev and craft-fonts at ci.yml's pin, next to the repository,
#     with CRAFT_I18N_DIR / CRAFT_FONTS_DIR exported for every shell,
#   - the crates.io and git dependencies (`cargo fetch --locked`),
#   - Java and TLC for the recovery-store skill's model check, with TLA2TOOLS exported,
#   - the pstack model sheet CLAUDE.md includes, unless one is already there.
#
# Idempotent: every step checks what is already there, so rerunning it is quick.
# Runs from the repository root or from the directory above it (where the cloud environment
# starts its setup script). Paste it into the environment's Setup script, or call it with
#   cd wordcraft && bash tools/claude-cloud-setup.sh
#
# Optional knobs (edit here, the cloud setup script cannot read environment variables you set
# in the environment settings):
#   WORDCRAFT_PREBUILD=1   also build the test targets once (`cargo build --workspace --tests`).
#   WORDCRAFT_SKIP_FONTS=1 skip the craft-fonts checkout.
set -euo pipefail

PREBUILD="${WORDCRAFT_PREBUILD:-0}"
SKIP_FONTS="${WORDCRAFT_SKIP_FONTS:-0}"
NEXTEST_VERSION="0.9.150"
# Fallback pins when the repository files cannot be read.
DEFAULT_FONTS_REF="b076417b5880bd4154bd22f00695b1d28c6bea83"
DEFAULT_TLA_VERSION="1.7.4"
DEFAULT_TLA_SHA="936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"

log() { printf '\n==> %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
SUDO=""
if [ "$(id -u)" -ne 0 ] && command -v sudo >/dev/null 2>&1; then SUDO="sudo"; fi

# --- Find the repository -------------------------------------------------------------------
REPO=""
for cand in "${WORDCRAFT_DIR:-}" "$PWD" "$PWD/wordcraft" "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." 2>/dev/null && pwd)"; do
  if [ -n "$cand" ] && [ -f "$cand/Cargo.toml" ] && [ -d "$cand/xtask" ] && grep -q 'wordcraft' "$cand/Cargo.toml"; then
    REPO="$(cd "$cand" && pwd)"
    break
  fi
done
if [ -z "$REPO" ]; then
  warn "WordCraft repository not found from $PWD; installing the toolchain only"
fi
DEPS_DIR="${WORDCRAFT_DEPS_DIR:-$( [ -n "$REPO" ] && dirname "$REPO" || echo "$HOME")}"

# --- System packages -------------------------------------------------------------------------
APT_PACKAGES="pkg-config libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev
libgtk-3-dev libfontconfig1-dev libfreetype-dev mesa-vulkan-drivers"
if [ -n "$REPO" ] && [ -f "$REPO/.github/workflows/ci.yml" ]; then
  # Read the list from ci.yml so the two never drift: the lines after `packages: >-`.
  from_ci="$(awk '/packages:[[:space:]]*>-/{on=1;next} on&&/^[[:space:]]*[a-z0-9]/&&!/:/{print;next} on{exit}' "$REPO/.github/workflows/ci.yml" | xargs || true)"
  [ -n "$from_ci" ] && APT_PACKAGES="$from_ci"
fi
# Tools the script itself and the build use. The cloud image ships GNU Unifont, which covers
# Myanmar without reordering it, so font fallback picks it and the layout crate's Myanmar shaping
# tests fail; Padauk (SIL OFL) gives fallback a real Myanmar face, as on the CI runners.
APT_PACKAGES="$APT_PACKAGES build-essential curl ca-certificates git fonts-sil-padauk"
# TLC (see below) needs Java 11 or later; an image that has a Java keeps it.
java -version >/dev/null 2>&1 || APT_PACKAGES="$APT_PACKAGES default-jre-headless"

if command -v apt-get >/dev/null 2>&1; then
  missing=()
  for p in $APT_PACKAGES; do
    dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q 'install ok installed' || missing+=("$p")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "Installing apt packages: ${missing[*]}"
    export DEBIAN_FRONTEND=noninteractive
    $SUDO apt-get update -qq || warn "apt-get update failed; trying the install anyway"
    $SUDO apt-get install -y -qq --no-install-recommends "${missing[@]}"
  else
    log "apt packages already installed"
  fi
else
  warn "apt-get not found; install these yourself: $APT_PACKAGES"
fi

# --- Rust ------------------------------------------------------------------------------------
export PATH="$HOME/.cargo/bin:$PATH"
if ! command -v rustup >/dev/null 2>&1; then
  log "Installing rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
fi
# Same rule as the gate: rust-toolchain.toml's channel wins, else stable.
TOOLCHAIN="stable"
if [ -n "$REPO" ] && [ -f "$REPO/rust-toolchain.toml" ]; then
  ch="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$REPO/rust-toolchain.toml")"
  TOOLCHAIN="${ch:-stable}"
fi
log "Rust toolchain $TOOLCHAIN with clippy, rustfmt, wasm32-unknown-unknown"
rustup toolchain install "$TOOLCHAIN" --profile minimal --no-self-update \
  --component clippy --component rustfmt --target wasm32-unknown-unknown
rustup default "$TOOLCHAIN" >/dev/null
rustc --version

# --- cargo-nextest ---------------------------------------------------------------------------
if cargo nextest --version 2>/dev/null | grep -q "cargo-nextest $NEXTEST_VERSION"; then
  log "cargo-nextest $NEXTEST_VERSION already installed"
else
  log "Installing cargo-nextest $NEXTEST_VERSION"
  arch="$(uname -m)"
  case "$arch" in
    x86_64) triple="x86_64-unknown-linux-gnu" ;;
    aarch64 | arm64) triple="aarch64-unknown-linux-gnu" ;;
    *) triple="" ;;
  esac
  url="https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$NEXTEST_VERSION/cargo-nextest-$NEXTEST_VERSION-$triple.tar.gz"
  if [ -n "$triple" ] && curl -fsSL --retry 3 "$url" | tar -xz -C "$HOME/.cargo/bin"; then
    cargo nextest --version
  else
    warn "could not install cargo-nextest; xtask falls back to cargo test"
  fi
fi

# --- craft-i18n and craft-fonts --------------------------------------------------------------
# Shallow checkout of one commit; does nothing when the directory is already at that commit.
checkout_pin() { # repo-url dir sha
  local url="$1" dir="$2" sha="$3"
  if [ -d "$dir/.git" ] && [ "$(git -C "$dir" rev-parse HEAD 2>/dev/null)" = "$sha" ]; then
    echo "$dir already at $sha"
    return 0
  fi
  mkdir -p "$dir"
  git -C "$dir" init -q
  git -C "$dir" remote remove origin 2>/dev/null || true
  git -C "$dir" remote add origin "$url"
  git -C "$dir" fetch -q --depth 1 origin "$sha"
  git -C "$dir" -c advice.detachedHead=false checkout -q --force FETCH_HEAD
  echo "$dir checked out at $sha"
}

ENV_LINES=()
if [ -n "$REPO" ] && [ -f "$REPO/craft-i18n.rev" ]; then
  i18n_rev="$(tr -d '[:space:]' <"$REPO/craft-i18n.rev")"
  if [[ "$i18n_rev" =~ ^[0-9a-f]{40}$ ]]; then
    log "craft-i18n at $i18n_rev"
    if checkout_pin https://github.com/storytold/craft-i18n "$DEPS_DIR/craft-i18n" "$i18n_rev"; then
      ENV_LINES+=("export CRAFT_I18N_DIR=\"$DEPS_DIR/craft-i18n\"" "export CRAFT_I18N_REQUIRED=1")
    else
      warn "craft-i18n checkout failed; proofing tests that need it will fail"
    fi
  else
    warn "craft-i18n.rev is not a commit SHA: '$i18n_rev'"
  fi
fi

if [ "$SKIP_FONTS" != 1 ]; then
  fonts_ref="$DEFAULT_FONTS_REF"
  if [ -n "$REPO" ] && [ -f "$REPO/.github/workflows/ci.yml" ]; then
    r="$(sed -n 's/^[[:space:]]*craft-fonts-ref:[[:space:]]*\([0-9a-f]\{40\}\).*/\1/p' "$REPO/.github/workflows/ci.yml" | head -1)"
    [ -n "$r" ] && fonts_ref="$r"
  fi
  log "craft-fonts at $fonts_ref"
  if checkout_pin https://github.com/storytold/craft-fonts "$DEPS_DIR/craft-fonts" "$fonts_ref"; then
    ENV_LINES+=("export CRAFT_FONTS_DIR=\"$DEPS_DIR/craft-fonts\"" "export CRAFT_FONTS_REQUIRED=1")
  else
    warn "craft-fonts checkout failed; building without it"
  fi
fi

# --- TLC -------------------------------------------------------------------------------------
# The model checker .claude/skills/recovery-store/check.sh and the /statechart skill run. The pin
# is read from check.sh so the two never drift, and the jar goes where check.sh looks for it.
# TLA2TOOLS is what lets /statechart skip its mise install, which an image without mise can't do.
tla_version="$DEFAULT_TLA_VERSION"
tla_sha="$DEFAULT_TLA_SHA"
tla_check="$REPO/.claude/skills/recovery-store/check.sh"
if [ -n "$REPO" ] && [ -f "$tla_check" ]; then
  v="$(sed -n 's/^version=//p' "$tla_check")"
  s="$(sed -n 's/^sha=//p' "$tla_check")"
  if [ -n "$v" ] && [ -n "$s" ]; then
    tla_version="$v"
    tla_sha="$s"
  fi
fi
tla_jar="${XDG_CACHE_HOME:-$HOME/.cache}/wordcraft-tla/tla2tools-$tla_version.jar"
tla_verified() { [ -f "$tla_jar" ] && [ "$(sha256sum "$tla_jar" | cut -d' ' -f1)" = "$tla_sha" ]; }
if tla_verified; then
  log "TLC $tla_version already downloaded"
else
  log "Downloading TLC $tla_version"
  mkdir -p "$(dirname "$tla_jar")"
  curl -fsSL --retry 3 -o "$tla_jar.part" "https://github.com/tlaplus/tlaplus/releases/download/v$tla_version/tla2tools.jar" \
    && mv -f "$tla_jar.part" "$tla_jar" || true
fi
if tla_verified; then
  ENV_LINES+=("export TLA2TOOLS=\"$tla_jar\"")
else
  rm -f "$tla_jar" "$tla_jar.part"
  warn "could not get TLC $tla_version; the recovery-store check will try again when it runs"
fi

# --- pstack model sheet ----------------------------------------------------------------------
# CLAUDE.md includes this file, which lives outside the repository. /setup-pstack rewrites it, so
# one that is already there is left alone. A session without the pstack plugin ignores it.
PSTACK_SHEET="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/pstack-models.md"
if [ -f "$PSTACK_SHEET" ]; then
  log "pstack model sheet already at $PSTACK_SHEET"
else
  log "Writing the pstack model sheet to $PSTACK_SHEET"
  mkdir -p "$(dirname "$PSTACK_SHEET")"
  cat >"$PSTACK_SHEET" <<'EOF'
# pstack model configuration

Per-role model overrides for pstack skills, from tools/claude-cloud-setup.sh; /setup-pstack rewrites this file. Each pstack SKILL.md names its defaults in a Models section; the values here override those defaults. Delete a line to fall back to the skill default.

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
session hook: on
EOF
fi

# --- Shell environment -----------------------------------------------------------------------
# Same variables the gate sets, for login shells (/etc/profile.d) and interactive or
# non-interactive bash (~/.bashrc, sourced before its interactive-only early return).
ENV_FILE="/etc/profile.d/wordcraft-dev.sh"
[ -w /etc/profile.d ] || ENV_FILE="$HOME/.wordcraft-dev.sh"
{
  echo "# Written by wordcraft tools/claude-cloud-setup.sh"
  echo 'case ":$PATH:" in *":$HOME/.cargo/bin:"*) ;; *) export PATH="$HOME/.cargo/bin:$PATH" ;; esac'
  echo "export CARGO_NET_RETRY=10"
  echo "export CARGO_PROFILE_DEV_DEBUG=line-tables-only"
  for l in "${ENV_LINES[@]+"${ENV_LINES[@]}"}"; do echo "$l"; done
} >"$ENV_FILE.tmp"
mv -f "$ENV_FILE.tmp" "$ENV_FILE"
marker="# wordcraft-dev-env"
if ! grep -qF "$marker" "$HOME/.bashrc" 2>/dev/null; then
  { echo "[ -r \"$ENV_FILE\" ] && . \"$ENV_FILE\" $marker"; cat "$HOME/.bashrc" 2>/dev/null || true; } >"$HOME/.bashrc.tmp"
  mv -f "$HOME/.bashrc.tmp" "$HOME/.bashrc"
fi
log "Environment written to $ENV_FILE"
cat "$ENV_FILE"
# shellcheck disable=SC1090
. "$ENV_FILE"

# --- Dependencies ----------------------------------------------------------------------------
if [ -n "$REPO" ]; then
  cd "$REPO"
  log "cargo fetch --locked"
  for attempt in 1 2 3; do
    if cargo fetch --locked; then break; fi
    [ "$attempt" = 3 ] && { warn "cargo fetch failed; the first build will fetch instead"; break; }
    sleep $((attempt * 5))
  done
  if [ "$PREBUILD" = 1 ]; then
    log "Prebuilding test targets"
    cargo build --workspace --tests || warn "prebuild failed"
  fi
fi

log "WordCraft environment ready. Run: cargo xtask ci"
