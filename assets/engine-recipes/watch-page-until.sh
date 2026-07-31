#!/usr/bin/env bash
# RECIPE 3 — watch a page until something arrives, then act.
#
# Shows all the /engine/wait forms an agent actually needs, including the one
# that matters most: an unmet wait is a REPORTED outcome (met:false), not an
# exception. A watcher that cannot tell "not yet" from "broken" is useless.
source "$(dirname "$0")/lib.sh"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT

cat > "$work/late.html" <<'HTML'
<!doctype html><title>late fixture</title><body><div id="slot"></div>
<script>setTimeout(function(){var d=document.createElement('div');d.id='late';d.textContent='arrived';document.getElementById('slot').appendChild(d);},800);</script>
</body>
HTML

echo "recipe: watch-page-until"
page=$(new_page "file://$work/late.html")

absent=$(ctl eval page_id="$page" js="document.getElementById('late') === null" | jget '["value"]')
[ "$absent" = "True" ] || fail "the element was already there; the watch would prove nothing"
ok "target genuinely absent at the start"

met=$(ctl wait page_id="$page" until='{"selector":"#late","state":"visible"}' timeout_ms=6000 | jget '["met"]')
[ "$met" = "True" ] || fail "the element never became visible"
ok "waited for #late by selector"

met=$(ctl wait page_id="$page" until='{"idle_ms":400}' timeout_ms=8000 | jget '["met"]')
[ "$met" = "True" ] || fail "the page never went idle"
ok "waited for layout+network quiet"

# An unmeetable wait must REPORT, not raise.
unmet=$(ctl wait page_id="$page" until='{"js":"false"}' timeout_ms=400 | jget '["met"]')
[ "$unmet" = "False" ] || fail "an unmeetable wait should report met:false"
ok "an unmeetable wait reported met:false instead of failing"

close_page "$page"
echo "recipe watch-page-until: GREEN"
