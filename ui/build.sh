#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WWWROOT="${SCRIPT_DIR}/../wwwroot"
DX_OUT="${SCRIPT_DIR}/target/dx/postgres-mcp-ui/release/web/public"

cd "${SCRIPT_DIR}"

# dx content-hashes the js/wasm bundle names and never removes the previous ones
# from its output directory, so after two builds that directory holds two of each.
# `cp -R` below would copy all of them into wwwroot and they would be committed —
# hundreds of kilobytes of wasm that index.html does not reference. Wiping the
# output first is what makes "one build, one bundle" true.
echo ">> cleaning ${DX_OUT}"
rm -rf "${DX_OUT}"

echo ">> dx build --release --web"
dx build --release --web

if [ ! -d "${DX_OUT}" ]; then
    echo "ERROR: build output not found at ${DX_OUT}"
    exit 1
fi

echo ">> cache-busting index.html"
python3 build.py "${DX_OUT}/index.html"

echo ">> cleaning ${WWWROOT}"
rm -rf "${WWWROOT}"
mkdir -p "${WWWROOT}"

echo ">> copying ${DX_OUT}/. -> ${WWWROOT}/"
cp -R "${DX_OUT}/." "${WWWROOT}/"

echo ">> done."
