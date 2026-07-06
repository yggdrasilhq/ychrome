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
host-owned session jar under `~/.yggterm/web-profiles/<name>` (the same jar the
yggterm viewport uses, so a profile means one identity whether ychrome renders
it standalone or hands it to yggterm).

`--via <ssh-host>` exists only in standalone mode: browse with that machine's
network identity (its localhost, its internal DNS, its routes) when no yggterm
is in the loop:

```
ychrome --via dev http://localhost:8000
```

Inside yggterm this flag is unnecessary by design — running `ychrome` in a
session already connects directly on that session's machine (the egress rule
in [docs/architecture.md](docs/architecture.md)).

## Profile picker (no arguments)

Run `ychrome` with no URL and it shows a **profile picker** instead of a blank
page:

```
# in a yggterm session
ychrome
```

It serves a small picker page on a loopback port and points the yggterm
viewport at it — a card per existing profile plus a URL/search field. Pick a
profile and type a URL (or leave it blank to start on search); ychrome then
opens that URL under that profile in the viewport. This is the natural entry
point for the multi-account use case: one command, choose which identity to
browse as. (The picker is thin-client only; standalone `ychrome` with no URL
still opens a blank window.)

## Status

**Working end-to-end as of yggterm 2.9.53.** Live-verified: `ychrome
http://localhost:8377` in a yggterm session on a remote machine rendered that
machine's loopback-only dev server in the viewport, with egress on that
machine, and Ctrl+C handed the terminal back. Standalone window mode also
works. Contract: [docs/protocol.md](docs/protocol.md) (authoritative copy in
yggterm's `docs/web-surfaces.md`).

## Docs

- [Product direction](docs/product.md)
- [Architecture](docs/architecture.md)
- [Surface protocol](docs/protocol.md)

## License

Apache-2.0.
