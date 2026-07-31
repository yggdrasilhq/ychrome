#!/usr/bin/env bash
# Run all three recipes. Non-zero if any fails — this is the Phase E AC.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
for r in crawl-and-extract form-fill watch-page-until; do bash "$here/$r.sh"; done
echo "ALL RECIPES GREEN on $(hostname)"
