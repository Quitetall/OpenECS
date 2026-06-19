#!/bin/sh
# Fetch the ECS-Bench-v1 corpus (PhysioNet CHB-MIT v1.0.0, ODC-BY 1.0).
#
# The corpus data is NOT redistributed in this repo — only the record list and
# the pinned hashes (ECS-Bench-v1.toml) are. This script downloads the exact
# canonical files; their SHA-256 then matches the committed manifest, so any
# lab grades against byte-identical data.
#
# Usage:   sh fetch.sh [DEST_DIR]      (default DEST_DIR: ./data)
set -eu

# Default the download to ./data NEXT TO this script (and the manifest), so the
# committed ECS-Bench-v1.toml paths resolve no matter the caller's cwd.
HERE="$(cd "$(dirname "$0")" && pwd)"
DEST="${1:-$HERE/data}"
BASE="https://physionet.org/files/chbmit/1.0.0"

while IFS= read -r rec; do
    case "$rec" in '' | \#*) continue ;; esac
    mkdir -p "$DEST/$(dirname "$rec")"
    echo "fetching $rec"
    curl -fSL -o "$DEST/$rec" "$BASE/$rec"
done < "$HERE/records.txt"

cat <<EOF

Done — downloaded into: $DEST

Verify against the committed pins (SHA-256 + shape):
  openecs verify-corpus --corpus-manifest "$HERE/ECS-Bench-v1.toml"

Benchmark your codec:
  openecs bench --codec-manifest YOUR_CODEC.toml \\
      --corpus-manifest "$HERE/ECS-Bench-v1.toml" --report report.html --charts
EOF
