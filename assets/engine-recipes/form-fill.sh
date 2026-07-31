#!/usr/bin/env bash
# RECIPE 2 — fill and submit a form with TRUSTED input.
#
# Every event here is a real GdkEvent, so isTrusted is true and default actions
# fire. The load-bearing lesson is the wait after typing: read the field
# straight after `ctl input` and you will eventually see it one character short.
source "$(dirname "$0")/lib.sh"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT

cat > "$work/form.html" <<'HTML'
<!doctype html><title>form fixture</title><body>
<form onsubmit="document.getElementById('out').textContent='sent:'+document.getElementById('name').value;return false">
<input id="name" style="width:300px;height:40px"><button id="go" type="submit">Go</button></form>
<div id="out">nothing</div></body>
HTML

echo "recipe: form-fill"
page=$(new_page "file://$work/form.html")
wait_js "$page" "document.getElementById('name') !== null"
ok "opened $page"

ctl input page_id="$page" events='[{"type":"click","selector":"#name"}]' >/dev/null
wait_js "$page" "document.activeElement && document.activeElement.id === 'name'"
ok "focused the field with a selector-addressed click"

ctl input page_id="$page" events='[{"type":"type","text":"ada lovelace"}]' >/dev/null
# THE RULE. Not a nicety: without this the next read races the last keystroke.
wait_js "$page" "document.getElementById('name').value === 'ada lovelace'"
ok "typed the full value (waited for it, did not assume it)"

ctl input page_id="$page" events='[{"type":"key","key":"Return"}]' >/dev/null
wait_js "$page" "document.getElementById('out').textContent === 'sent:ada lovelace'"
ok "submitted with Enter and the handler ran"

close_page "$page"
echo "recipe form-fill: GREEN"
