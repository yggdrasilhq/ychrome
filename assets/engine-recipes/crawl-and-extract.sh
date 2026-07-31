#!/usr/bin/env bash
# RECIPE 1 — crawl a list of URLs and extract structure from each.
#
# Uses /engine/batch, which STREAMS NDJSON: each page prints as it finishes, so
# a 300-page crawl reports progress instead of going quiet for minutes. The
# governor is in charge throughout — the batch never bypasses max_live.
source "$(dirname "$0")/lib.sh"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT

for n in 1 2 3; do
  printf '<!doctype html><title>crawl %s</title><h1>page %s</h1><a href="https://example.com/">link %s</a>' "$n" "$n" "$n" > "$work/p$n.html"
done
urls=$(python3 -c 'import json,sys;print(json.dumps([{"url":"file://"+u} for u in sys.argv[1:]]))' "$work"/p*.html)

echo "recipe: crawl-and-extract"
lines=$(ctl batch open="$urls" concurrency=3 | tee "$work/stream.ndjson" | wc -l)
[ "$lines" -eq 4 ] || fail "expected 3 pages + 1 summary line, got $lines"
ok "batch streamed $lines ndjson lines"

opened=$(grep '"summary":true' "$work/stream.ndjson" | jget '["opened"]')
[ "$opened" = "3" ] || fail "batch opened $opened of 3"
ok "batch opened $opened pages"

# Extract from each page through /engine/dom, the structured read.
grep -v '"summary":true' "$work/stream.ndjson" | while read -r line; do
  page=$(printf '%s' "$line" | jget '["page_id"]')
  # The page is loaded (batch waits), but ALWAYS confirm the state you are
  # about to read rather than assuming the load implies it.
  wait_js "$page" "document.querySelector('h1') !== null"
  n=$(ctl dom page_id="$page" mode=snapshot | jget '["dom"]["nodes"][0]["text"]')
  [ -n "$n" ] || fail "no interactable node extracted from $page"
  ok "extracted link text $n from $page"
  close_page "$page"
done
echo "recipe crawl-and-extract: GREEN"
