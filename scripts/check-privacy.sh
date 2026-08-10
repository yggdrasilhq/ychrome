#!/usr/bin/env bash
# Refuse to let private material into this PUBLIC repo.
#
# Why this exists: on 2026-08-07 an audit found the owner's private decision
# structure sitting in tracked test fixtures and war-story comments — his
# sidebar row taxonomy (numbered campaign lanes), the names of private data
# stores, live legal-filing portals, and personal home paths. None of it was a
# credential. All of it was a map of a private life, published.
#
# ⛔ THE RULE THIS FILE ENFORCES, and the thing that makes it different from a
# secret scanner: the leak vector here is NEVER a secret. It is an agent
# writing a REAL example into a test fixture or a comment because a real
# example was in front of it. Use invented examples. Always.
#
# ⛔ This checker must not itself become the leak. It matches SHAPES where it
# can (a numbered row title looks the same whatever it is called), and where it
# must name something it holds the term base64-encoded, so the word is not
# greppable in a public tree. Never add a plaintext private term below.
#
# Exit non-zero with the offending lines; silence means clean.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2
fail=0
note() { echo "privacy: $*" >&2; fail=1; }

# Tracked files AND untracked-but-not-ignored ones, minus vendored/third-party
# trees and binary-ish assets.
#
# ⛔ WHY UNTRACKED FILES ARE IN SCOPE, learned 2026-08-09: this checker is meant
# to run BEFORE a commit, and before a commit a newly written doc is exactly
# `??` — untracked. Scanning `git ls-files` alone therefore reported "ok" on a
# file containing a real home path, a private store name and a host name,
# because the file had not been added yet. The one moment the lock exists to
# cover was the one moment it could not see. Verified by writing a deliberately
# leaky file and watching the checker pass.
#
# `--exclude-standard` keeps .gitignore'd build output out, so this stays fast
# and does not flood on target/ or node_modules.
files=$(git ls-files --cached --others --exclude-standard \
  | grep -vE '^(vendor|third_party|node_modules)/' \
  | grep -vE '^assets/' \
  | grep -vE '(Cargo\.lock|\.b64|\.woff2?|\.png|\.jpg|\.ico|\.gz|\.zip)$' \
  | grep -vE '^docs/archive/' \
  | grep -vE '^scripts/check-privacy\.sh$')
[ -n "$files" ] || exit 0

hits() { echo "$files" | xargs grep -nIE "$1" 2>/dev/null; }

# 1. Personal home paths. A public repo must not know whose machine it was on.
#    ⚠ Invented placeholders are the DESIRED form, so they are allowlisted here.
#    A checker that flags the correct answer gets switched off, and then it
#    protects nothing — so keep this list generous and the failure rare.
PLACEHOLDER='/home/(user|u|x|y|z|operator|gui-host|example|someone|test|alice|bob|dev|dev-host|build)(/|\b)'
h=$(hits '/home/[a-z][a-z0-9_-]*/' | grep -vE "$PLACEHOLDER")
[ -n "$h" ] && { note "absolute personal home paths — use /home/user or an invented placeholder:"; echo "$h" | head -12 >&2; }

# 2. RFC1918 addresses. Real topology is a signpost to live attack surface;
#    RFC 5737 (192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24) exists for docs.
h=$(hits '\b(192\.168\.[0-9]+\.[0-9]+|10\.[0-9]+\.[0-9]+\.[0-9]+|172\.(1[6-9]|2[0-9]|3[01])\.[0-9]+\.[0-9]+)\b')
[ -n "$h" ] && { note "private LAN addresses — use RFC 5737 ranges in examples:"; echo "$h" | head -12 >&2; }

# 3. The owner's sidebar row taxonomy. A fixture shaped `"3. word: phrase"` or
#    `"5.1 word: phrase"` is how his campaign lanes are named, and publishing the
#    set publishes what he is working on.
#    ⚠ The SHAPE is also a real feature (outline numbering) and its tests cannot
#    exist without it — so this flags the shape only when the LABEL is not an
#    obviously invented one. Testing outline parsing is fine; naming his actual
#    lanes is not. Add new synthetic labels here rather than weakening the rule.
SYNTHETIC='"[0-9]+(\.[0-9]+)? (widgets|gadgets|sprockets|cogs|levers|spindles|yggterm|demo|sample|project|alpha|beta|gamma|thing|probe|foo|bar)(:|\b)'
h=$(hits '"[0-9]+(\.[0-9]+)? [a-z][a-z0-9_-]{2,}: ' | grep -vE "$SYNTHETIC")
[ -n "$h" ] && { note "numbered row-taxonomy fixture names a real lane — use an invented label:"; echo "$h" | head -12 >&2; }

# 4. Named private stores / portals / personal projects, held encoded so this
#    file does not republish them. Add new terms with:
#      printf '%s' 'theterm' | base64
for enc in \
  ZG9zc2llcmdyYXBo Y2FsbGdyYXBo ZmluZ3JhcGg= bWVkZ3JhcGg= dGF4Z3JhcGg= \
  aGluZ2U= Z21hdA== amFncml0aQ== dHJ1ZWNhbGxlcg== L3J1bi9zbWI0aw== c21iZnM= \
  YXZpa2FscGFfb3Bj
do
  term=$(printf '%s' "$enc" | base64 -d 2>/dev/null) || continue
  [ -n "$term" ] || continue
  h=$(echo "$files" | xargs grep -nIiF -- "$term" 2>/dev/null)
  [ -n "$h" ] && { note "a private store/portal/project name is present (term withheld) — use an invented name:"; echo "$h" | head -6 >&2; }
done

if [ "$fail" -eq 0 ]; then
  echo "privacy: ok — no personal paths, LAN addresses, row taxonomy, or private names"
fi
exit $fail
