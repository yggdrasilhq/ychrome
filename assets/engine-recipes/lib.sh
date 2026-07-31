#!/usr/bin/env bash
# Shared helpers for the engine recipes (docs/agent-engine.md Phase E).
#
# THE RULE these recipes exist to teach: after any input, WAIT for the state you
# expect before you read. WebKitGTK acknowledges key events one at a time while
# `eval` is sent immediately, so a read issued straight after typing can
# overtake the last keystroke and see the field one character short. This is not
# a rare race — it reproduced on two runs in three. `ctl wait` is the fix, and
# it is the fix for every "the page has not caught up yet" case, not just typing.
set -euo pipefail

YCHROME="${YCHROME:-ychrome}"
ctl() { "$YCHROME" ctl "$@"; }

# jq is not assumed: these hosts have python3 everywhere.
jget() { python3 -c 'import json,sys;d=json.load(sys.stdin);print(eval("d"+sys.argv[1]))' "$1"; }

fail() { echo "RECIPE FAIL: $*" >&2; exit 1; }
ok()   { echo "  ok: $*"; }

# Wait for a JS expression to become truthy, and REFUSE to continue if it does
# not. A recipe that carries on after an unmet wait is the thing that teaches
# the wrong habit.
wait_js() {
  local page="$1" expr="$2" timeout="${3:-8000}" met
  met=$(ctl wait page_id="$page" until="{\"js\":$(python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$expr")}" timeout_ms="$timeout" | jget '["met"]')
  [ "$met" = "True" ] || fail "wait timed out for: $expr"
}

new_page() { ctl open url="$1" ${2:+profile="$2"} | jget '["page_id"]'; }
close_page() { ctl close page_id="$1" >/dev/null; }
