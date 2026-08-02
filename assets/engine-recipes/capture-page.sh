#!/usr/bin/env bash
# RECIPE 4 — capture a page, in all four regions.
#
# The composition an agent actually wants: url in, PNG out. It is a RECIPE and
# not a new top-level verb on purpose — `ctl` is a thin client by design
# (docs/agent-engine.md §3, "CLI = thin client"), and a `ychrome shot <url>`
# with its own flag set would be a second door onto `/engine/shot` that can
# drift from it. Copy this file, or copy the four lines you need out of it.
#
# THE RULE it teaches, which is the same rule every other recipe teaches: wait
# for the state you are about to read. A capture issued straight after `goto`
# can beat the paint, and a blank PNG of a page that loaded fine is the most
# expensive kind of wrong answer — it looks like a rendering bug.
source "$(dirname "$0")/lib.sh"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT

# A tall page with a fixed header, a marked element, and content below the fold.
cat > "$work/page.html" <<'HTML'
<!doctype html><meta charset="utf-8"><title>capture recipe</title>
<style>body{margin:0;font-family:sans-serif}
header{position:fixed;top:0;left:0;right:0;height:40px;background:#123;color:#fff}
section{height:500px;padding:20px}
#target{width:300px;height:150px;background:#c00;border:5px solid #000}</style>
<header>fixed</header>
<section style="padding-top:60px;background:#eee"><h1>band 1</h1></section>
<section style="background:#dff"><h1>band 2</h1></section>
<section style="background:#fed"><h1>band 3</h1><div id="target"></div></section>
HTML

echo "recipe: capture-page"
page=$(new_page "file://$work/page.html")
# The load is finished (open waits), but confirm the thing you are about to
# photograph is actually in the document before photographing it.
wait_js "$page" "document.querySelector('#target') !== null"

# 1. VIEWPORT — what is on screen. The default region.
shot=$(ctl shot page_id="$page" --out "$work/viewport.png")
vh=$(printf '%s' "$shot" | jget '["height"]')
[ "$vh" -gt 0 ] || fail "viewport capture reported no height"
ok "viewport capture ${vh}px tall"

# 2. FULL — the whole scrollable document. It must be TALLER than the viewport,
#    which is the assertion that distinguishes a real full-page capture from a
#    viewport capture with a different label on it.
shot=$(ctl shot page_id="$page" region=full --out "$work/full.png")
fh=$(printf '%s' "$shot" | jget '["height"]')
[ "$fh" -gt "$vh" ] || fail "full capture ($fh) is not taller than the viewport ($vh)"
ok "full-page capture ${fh}px tall (viewport is ${vh}px)"

# 3. ELEMENT — one node, cropped out of that same full-document snapshot. The
#    selector resolves through the pool /engine/input clicks through, so the
#    reply also says how many things matched.
shot=$(ctl shot page_id="$page" region=element selector='#target' --out "$work/element.png")
ew=$(printf '%s' "$shot" | jget '["width"]')
matches=$(printf '%s' "$shot" | jget '["selector"]["hittable"]')
[ "$ew" -ge 300 ] && [ "$ew" -le 320 ] || fail "element crop is ${ew}px wide, expected ~310"
ok "element capture ${ew}px wide, $matches hittable match(es)"

# 4. RECT — a document-space area. This is the "selection area" mode: a human
#    drags a rectangle, an agent names one, both land here.
#    ⛔ DOCUMENT coordinates. A getBoundingClientRect top is VIEWPORT-relative;
#    add window.scrollY before sending it, or the crop lands off the top.
shot=$(ctl shot page_id="$page" region=rect rect='{"x":0,"y":600,"w":400,"h":300}' --out "$work/rect.png")
rw=$(printf '%s' "$shot" | jget '["width"]')
[ "$rw" -eq 400 ] || fail "rect crop is ${rw}px wide, expected 400"
ok "rect capture ${rw}px wide at document y=600"

# A crop that misses must REFUSE, not answer with a blank image. This is the
# failure that otherwise gets debugged as a rendering bug.
if ctl shot page_id="$page" region=rect rect='{"x":0,"y":99000,"w":10,"h":10}' \
     --out "$work/miss.png" >/dev/null 2>&1; then
  fail "a rect off the end of the document should have been refused"
fi
[ -e "$work/miss.png" ] && fail "a refused capture must not write an --out file"
ok "a crop that misses the document is refused, and writes no file"

# Every artifact is a real PNG, not an error page with a .png name.
for f in viewport full element rect; do
  head -c 8 "$work/$f.png" | od -An -tx1 | grep -q "89 50 4e 47" || fail "$f.png is not a PNG"
done
ok "all four captures are PNG"

close_page "$page"
echo "recipe capture-page: GREEN"
