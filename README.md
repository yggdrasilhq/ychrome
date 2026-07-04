# ychrome

ychrome is a web viewport for the Yggdrasil ecosystem, and the pilot app for
**libyggterm** — the pattern where a program launched in a yggterm terminal
takes over yggterm's GUI surfaces.

## The design

The primary UX is *locality by invocation*:

```
# in a yggterm terminal connected to machine `dev`
$ ychrome http://localhost:8000
```

The yggterm viewport for that session becomes a browser showing **dev's**
`localhost:8000`. No flags, no tunnels to think about, no "which machine am I
on" bookkeeping — where you typed the command *is* where the URL resolves.
Closing ychrome (Ctrl+C, or closing the surface) hands the viewport back to
the terminal.

Under the hood, ychrome is a thin client: it finds the local yggterm host
daemon, asks it to open a web surface for the calling session, and blocks in
the foreground while the surface is alive. The daemon relays the request to
the attached yggterm GUI; the GUI swaps the session viewport to a webview and
routes localhost traffic back to the session's machine over the existing ssh
substrate. The right sidebar belongs to ychrome while it runs (profiles,
navigation, page info).

## Standalone fallback

Outside yggterm (a plain desktop, a bare xterm), ychrome degrades to opening
its own window:

```
ychrome https://chat.example.com --profile work
ychrome https://chat.example.com --profile personal
```

`--profile` gives each name an isolated persistent storage (cookies, logins),
so multiple accounts on the same site coexist — each profile is its own
session jar under `~/.local/share/ychrome/profiles/<name>`.

`--via <ssh-host>` exists only in standalone mode: browse with that machine's
network identity (its localhost, its internal DNS, its routes) when no yggterm
is in the loop:

```
ychrome --via dev http://localhost:8000
```

Inside yggterm this flag is unnecessary by design — running `ychrome` in a
session already connects directly on that session's machine (the egress rule
in [docs/architecture.md](docs/architecture.md)).

## Status

Early pilot. The standalone window mode works today; the yggterm viewport
takeover (the actual point) is being built against the daemon-relay protocol
described in [docs/protocol.md](docs/protocol.md).

## Docs

- [Product direction](docs/product.md)
- [Architecture](docs/architecture.md)
- [Surface protocol](docs/protocol.md)

## License

Apache-2.0.
