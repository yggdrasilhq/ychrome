# Architecture

## Two modes, one binary

```
ychrome <url>
   │
   ├── yggterm session detected (YGGTERM_SESSION_ID + daemon socket env)
   │      → thin-client mode: declare a web surface over OSC 7717,
   │        block until it closes. Rendering happens in the yggterm GUI.
   │
   └── no yggterm
          → standalone mode: open an own tao/wry (WebKitGTK) window.
```

## Thin-client mode (the yggterm path)

Participants:

- **ychrome (CLI)** — runs on whatever machine the user's terminal session is
  on. Resolves the session identity from environment variables exported by the
  yggterm daemon into every PTY it owns (the `TMUX`-variable pattern). Emits
  `web-surface open` over the terminal's OSC 7717 stream; stays in the foreground;
  Ctrl+C or daemon notification of surface close terminates it.
- **yggterm host daemon** — relays the request to attached GUI clients,
  tagged with the session id. Owns lifecycle truth: if the CLI dies, the
  surface closes; if the surface is closed in the GUI, the CLI is told.
- **yggterm GUI** — swaps that session's viewport from the terminal to a
  webview surface. The right sidebar shows ychrome's panels while the surface
  is foreground.

## The egress rule

**A surface's network egress is the invoking host's network — for all URLs,
always.** The connection to the target service is made *on the session's
machine, by the session's side* (CLI or daemon); the GUI only renders.

Mechanism: a per-surface SOCKS proxy relayed over the existing substrate. The
session-side component is the proxy egress; the GUI configures the surface's
web context (each surface has its own context already, for profile isolation)
to route all requests — including DNS resolution — through it. Loopback
services, internal DNS names, docker networks, VPN-only routes, and
source-IP-checked services all behave exactly as they would in a browser
running on that machine.

A GUI-side `ssh -L` port forward was considered and rejected: it originates
the connection on the GUI host and only special-cases loopback, silently
breaking every other only-reachable-from-there case.

Key property: **the network plumbing is yggterm's job, not the user's.** The
user never names a machine; where they typed the command is where the URL
resolves.

## Standalone mode

`src/main.rs`: tao event loop + wry WebViewBuilder on a WebContext whose
data directory comes from `profile_dir`, currently
`~/.yggterm/web-profiles/<profile>`. `--via <host>`
is the same egress rule with plain ssh as the carrier: borrow that host's
entire network identity for the window via a dynamic SOCKS forward
(`ssh -N -D`), with the web context's proxy pointed at the local end. This
dynamic SOCKS route is implemented in `main.rs`; it is not the earlier
single-port URL rewrite. The forward dies with the window.

Standalone mode is also the degradation story required of every libyggterm
app: in a bare xterm the command still does something sensible.

## State ownership and actual placement

Ychrome is a libyggterm app, not Chromium. Logical profile identity, physical
storage, rendering, invoking-host egress and control authority are separate.
The present implementation does not provide automatic fleet profile sync.

| Concern | Current owner / placement |
| --- | --- |
| Standalone and headless-engine profile jar | `main.rs::profile_dir` on the app host; the temporary profile is ephemeral |
| Thin-client native web context | Created by the rendering GUI; its jar can be GUI-host-local even while policy and egress are remote |
| Profile metadata and normalization | The shared yggterm-core profile contract; unknown metadata keys must survive updates |
| Policy, user-agent settings, zoom and bundled assets | Ychrome app owners on the invoking host |
| Headless engine data/cache | `engine/identity.rs`, with both WebKit base directories in the profile jar |
| Vault | Separate native vault peer; local unlocked state and presence grants are not profile files |
| Daemon socket, lock and live registry | Host-local runtime state, not transferable profile authority |

The surface context key includes jar, egress/SOCKS endpoint and
control/signer endpoint. Profile-name equality alone cannot merge contexts
without changing network or control authority. Engine cookie-import exists
because editing a persistent jar does not update every live network-process
store. Import acknowledgement alone does not prove full website-state sync.

Future shared logical profiles need a common catalog, typed app-owned data
exports and explicit consistency/activation rules. Preserve invoking-host
`localhost` resolution; classify WebKitCache, HSTS, CacheStorage, IndexedDB,
cookies and private settings separately. Do not recursively mirror a mixed
profile/runtime directory or imply that every byte named cache is disposable.

## What lands where (extraction discipline)

The daemon RPC, the GUI surface swap, and the automatic forward live in
yggterm first. Only once a second app (Paper or Cellulose embedding) needs the
same machinery does the shared part get extracted into libyggterm. ychrome
must not grow a private protocol that the extraction then has to undo — the
protocol doc is written against yggterm's daemon from day one.
