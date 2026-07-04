# Product Direction

## Why ychrome exists

Two concrete pains, one design:

1. **Remote localhost is invisible.** A dev server running on another machine
   (`python -m http.server` on `dev`) cannot be seen from the desk you sit at
   without manual tunnels, `0.0.0.0` binds, or RDP-ing into the machine to run
   a browser there. In yggterm you are *already* in a terminal on that
   machine; typing `ychrome http://localhost:8000` there should simply show
   the page in the viewport. The invocation location carries the network
   locality — that is the whole product.

2. **Multi-account web apps are painful.** Two accounts on one self-hosted
   service (e.g. an Open WebUI instance) fight over a single browser session.
   ychrome profiles are cheap, named, persistent storage jars; one command per
   account, no account switching.

## Why it is the libyggterm pilot

ychrome is deliberately the smallest possible libyggterm app: one surface
(a webview in the viewport), a small sidebar contribution, a clean
open/close lifecycle. It forces the platform questions — how an app claims
the viewport, how it feeds sidebar panels, how it releases on exit, what
happens in a plain terminal — without dragging in documents (Paper), grids
(Cellulose), or graphs (yggtopo). What ychrome's integration teaches becomes
libyggterm's first API.

## Non-goals (v0)

- Not a general browser: no tabs, no bookmarks, no extensions. One URL, one
  surface. Navigation within the page works; chrome around it stays minimal.
- No embedded rendering tricks: the webview is the platform WebKit, unmodified.
- No auth of its own: profiles are storage isolation, not identity.
