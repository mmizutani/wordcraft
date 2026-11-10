#!/usr/bin/env bash
# Build the Sites archive input in a dedicated checkout.
set -euo pipefail

ROOT="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
unset CDPATH

mode=${1:-}
case "$#:$mode" in
  1:--probe) set -- --locked --features probe ;;
  1:--editor) set -- --locked ;;
  *) echo 'Usage: packaging/sites/build.sh --probe|--editor' >&2; exit 2 ;;
esac

dist="$ROOT/dist"
if [[ -d "$dist" ]] && [[ -n "$(find "$dist" -mindepth 1 -maxdepth 1 ! -name server ! -name client -print -quit)" ]]; then
  echo 'Use a dedicated Sites checkout. The Sites packager archives everything under dist/.' >&2
  exit 1
fi

mkdir -p "$ROOT/target"
stage=$(mktemp -d "$ROOT/target/sites-build.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT
mkdir -p "$stage/server" "$stage/client"

(
  cd "$ROOT/packaging/sites/worker"
  worker-build --release --out-dir "$stage/worker" -- "$@"
)
cp "$stage/worker/index.js" "$stage/worker/index_bg.wasm" "$stage/server/"
cat > "$stage/server/wrangler.json" <<'JSON'
{
  "name": "wordcraft",
  "main": "index.js",
  "compatibility_date": "2026-04-01",
  "assets": { "directory": "../client", "binding": "ASSETS" }
}
JSON

if [[ "$mode" == --probe ]]; then
  printf '%s\n' '<!doctype html><meta charset="utf-8"><title>WordCraft Sites probe</title><p>WordCraft Sites probe</p>' > "$stage/client/index.html"
  printf '\000asm\001\000\000\000' > "$stage/client/probe.wasm"
else
  : "${CRAFT_FONTS_DIR:?Set CRAFT_FONTS_DIR to the Pages build input}"
  : "${CRAFT_I18N_DIR:?Set CRAFT_I18N_DIR to the repository-pinned language input}"
  export CRAFT_FONTS_REQUIRED=1 CRAFT_I18N_REQUIRED=1
  export DIST="$stage"
  # shellcheck source=../env.sh
  . "$ROOT/packaging/env.sh"
  (cd "$ROOT/apps/wordcraft-web" && trunk build --release --dist "$stage/client")
  # Enable the same-origin library only in the authenticated Sites editor.
  editor_html="$stage/client/index.html"
  sed 's|</head>|<meta name="wordcraft-cloud-documents" content="enabled"></head>|' "$editor_html" > "$stage/editor.html"
  grep -Fq '<meta name="wordcraft-cloud-documents" content="enabled">' "$stage/editor.html"
  mv "$stage/editor.html" "$editor_html"
  copy_docs "$stage/client"

  # The Worker serves oversized WASM at its original Trunk URL from a gzip asset.
  asset_limit=$((25 * 1024 * 1024))
  for wasm in "$stage/client"/wordcraft-web-*_bg.wasm; do
    test -s "$wasm"
    if (( $(wc -c < "$wasm") > asset_limit )); then
      gzip -n -9 -c "$wasm" > "$wasm.gz"
      if (( $(wc -c < "$wasm.gz") > asset_limit )); then
        echo 'Compressed editor WASM exceeds the Workers asset limit. Use an R2 delivery path.' >&2
        exit 1
      fi
      rm -- "$wasm"
    fi
  done
fi

test -s "$stage/client/index.html"
mkdir -p "$dist"
rm -rf -- "$dist/server" "$dist/client"
mv "$stage/server" "$stage/client" "$dist/"
printf 'Built Sites Worker and browser assets in %s\n' "$dist"
