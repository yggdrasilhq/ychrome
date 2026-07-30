# lalpathlabs.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## custom-panel-cart-no-login · WORKS
task: 
model: claude-opus-5
date: 2026-07-28
tags: 

Booking a custom multi-test panel on Dr Lal PathLabs, agent-driven, no login needed
up to the cart.

TWO FRONTENDS, TWO CARTS. `/checkout` is a LEGACY app with its own (always empty)
cart and will make a successful SPA add look like it failed. The real SPA is
`/book-a-test/<city>` and its cart is `/home/cart`. Do not diagnose an add-to-cart
failure from `/checkout`.

City is sticky in localStorage (`City`, `HeaderCity`) and in redux-persist
`persist:root` -> App.selected_city. Kolkata = city_id 7. Land on
`/book-a-test/kolkata` once and every later API call is city-scoped.

SEARCH: the catalogue search box is react-bootstrap-typeahead (`input.rbt-input-main`,
index 0). Native-setter + `input` event drives it; results render in `.rbt-menu`
after ~2.5-3.5 s as "<CODE> - <TEST NAME>". Clicking an option navigates to
`/pathology-test/<slug_name>`.

THE API: the typeahead calls
  GET https://1xviewapimaster.lalpathlabs.com/v1/test/global-search
      ?page=1&size=100&p=true&search_string=<q>&city_id=<n>
via XMLHttpRequest (NOT fetch - hook XHR, not fetch). A direct XHR/fetch of that URL
from eval context returns null (CORS/headers), so you cannot call it yourself - drive
the typeahead and capture `this.responseText` on the XHR `load` event. The response
carries item_id, item_name, slug_name, instructions, ref_guide_new (specimen, method,
report frequency) but NO PRICE. Price is only on the rendered detail page.

PRICE: navigate `/pathology-test/<slug>`, wait ~5 s, read
`document.body.innerText` from the "Home\n" marker; the block is
"<Title> | <TEST NAME> | <MRP> | <price> | Special Instruction : ... |
Parameters covered : N | Report Frequency : ...". Single-price tests show one number.

ADD TO CART: `el.click()` on the element whose trimmed innerText is exactly
"Add to cart" WORKS (React onClick binds fine; no real input needed, so an UNMAPPED
shadow surface is enough - `web do` is not required). VERIFY by re-reading the page:
the button flips to "Remove". Do NOT trust the click return value.

CART STATE is authoritative in localStorage `persist:root` -> Cart.CART_LIST, each
entry {item_id,item_name,price,components_count}. Read it to total the order without
rendering anything.

Reaching the cart: the header "Cart" is a JS handler with no href - click the element
whose trimmed innerText is exactly "Cart" (filter children.length<3). Lands on
`/home/cart` showing line items + Net Payable. Login ("Sign In", phone+OTP) is NOT
required to build the cart; it gates slot/address/payment only.

Report-frequency restrictions are per-test and REAL scheduling constraints, e.g.
CYSTATIN C (B173) is Mon/Wed/Fri only; SWASTHFIT GLP SCREEN (WM250) is Mon/Thu by
11 am. Read "Report Frequency" on every test before promising the user a day.

## otp-login-needs-mapped-surface · BLOCKED
task: 
model: claude-opus-5
date: 2026-07-28
tags: 

Login is where an unmapped shadow surface stops. Cart building is fully headless
(see slug custom-panel-cart-no-login); LOGIN IS NOT.

"LOGIN / SIGN UP WITH OTP" on /home/cart sends a **4-digit** SMS OTP (sender
TX-LALLAB-S, arrives in <10 s) into FOUR separate `input[type=tel][maxlength=1]`
boxes. Native-setter + input/change/keyup events fill all four visibly
(`filled:"xxxx"`) and the LOGIN button enables — but React state never updates,
so submit posts an empty OTP: no error text appears, the modal simply stays put
and the header still reads "Sign In". A silent failure that looks like a wrong code.

`web do fill --selector-set 'input[type=tel]' --text '1234'` is the correct verb
for this component, and it refuses `surface_not_mapped` on a shadow surface. So:
segmented OTP on this site REQUIRES real input, which requires a mapped surface.
Do not burn attempts re-driving it with eval - the DOM will look right every time.

REMOVING an item from /home/cart also resists eval. The visible "Remove" is a
`<label for="removeTest">` inside `<p class="... cursor-pointer">`; clicking either
the label or the parent `<p>` does nothing to CART_LIST. WORKAROUND THAT WORKS:
rewrite the cart directly -
  const root=JSON.parse(localStorage.getItem('persist:root'));
  const cart=JSON.parse(root.Cart); cart.CART_LIST=[];
  root.Cart=JSON.stringify(cart); localStorage.setItem('persist:root',JSON.stringify(root));
then reload `/book-a-test/kolkata` (redux-persist rehydrates empty) and re-add the
wanted tests from their `/pathology-test/<slug>` pages, which DOES work headless.

HANDOFF PATH: the cart lives in the ychrome PROFILE's localStorage, so an operator
can finish it in a normal window with
  ychrome --profile <same-profile> https://www.lalpathlabs.com/home/cart
and the built cart is still there to log into and pay for. Build the cart with the
agent, hand over the OTP step.
