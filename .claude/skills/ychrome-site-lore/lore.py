#!/usr/bin/env python3
"""ychrome site-lore: known-working methods for agent-driven browsing, per site.

Source of truth is one Markdown file per domain under ./lore/<domain>.md. Those
files are committed to the ychrome repo, so every agent on every fleet host
learns from the ones that came before via a normal git pull. A derived SQLite
index (rebuildable, gitignored) is offered for fast cross-site search, but it is
NEVER the source of truth: the fleet syncs files newest-wins, and a shared binary
DB would silently clobber concurrent writes from two hosts and cannot be diffed
or reviewed. Markdown per-site keeps writes local, mergeable, and human-readable.

Commands:
    lore.py get <domain>                 print a site's lore (or a template hint)
    lore.py log <domain> ...             append a method entry (see --help)
    lore.py search <term>                grep entries across every site
    lore.py list                         sites with entry counts + last touched
    lore.py reindex                      (re)build the SQLite query cache
    lore.py query "<sql>"                run SQL against the cache (read-only)

Entry format inside lore/<domain>.md (parsed deterministically):

    ## <slug> · <STATUS>
    task: <one line: what the agent was trying to do>
    model: <model id, e.g. claude-fable-5>
    date: <YYYY-MM-DD>
    tags: <comma,separated>

    <freeform markdown: the method, selectors, exact steps, gotchas, proof>

STATUS is one of WORKS | PARTIAL | BROKEN | BLOCKED.
"""
from __future__ import annotations

import argparse
import datetime as _dt
import os
import re
import sqlite3
import sys
from dataclasses import dataclass, field
from pathlib import Path

SKILL_DIR = Path(__file__).resolve().parent
LORE_DIR = SKILL_DIR / "lore"
STATUSES = ("WORKS", "PARTIAL", "BROKEN", "BLOCKED")
CACHE_DB = Path(
    os.environ.get("YCHROME_SITE_LORE_DB", str(Path.home() / ".yggterm/ychrome/site-lore.db"))
)

HEADER_KEYS = ("task", "model", "date", "tags")
_ENTRY_RE = re.compile(r"^##\s+(?P<slug>\S+)\s+·\s+(?P<status>\w+)\s*$")


@dataclass
class Entry:
    domain: str
    slug: str
    status: str
    task: str = ""
    model: str = ""
    date: str = ""
    tags: list[str] = field(default_factory=list)
    body: str = ""

    def to_markdown(self) -> str:
        lines = [f"## {self.slug} · {self.status}"]
        lines.append(f"task: {self.task}")
        lines.append(f"model: {self.model}")
        lines.append(f"date: {self.date}")
        lines.append(f"tags: {', '.join(self.tags)}")
        lines.append("")
        lines.append(self.body.strip("\n"))
        return "\n".join(lines).rstrip() + "\n"


def _normalize_domain(raw: str) -> str:
    """A bare registrable-ish host: strip scheme, path, port, and a leading www."""
    d = raw.strip().lower()
    d = re.sub(r"^[a-z]+://", "", d)
    d = d.split("/", 1)[0]
    d = d.split(":", 1)[0]
    d = re.sub(r"^www\.", "", d)
    return d


def _site_path(domain: str) -> Path:
    return LORE_DIR / f"{_normalize_domain(domain)}.md"


def _parse_site(path: Path) -> list[Entry]:
    if not path.exists():
        return []
    domain = path.stem
    entries: list[Entry] = []
    cur: Entry | None = None
    body_lines: list[str] = []
    header_phase = False
    for line in path.read_text(encoding="utf-8").splitlines():
        m = _ENTRY_RE.match(line)
        if m:
            if cur is not None:
                cur.body = "\n".join(body_lines).strip("\n")
                entries.append(cur)
            cur = Entry(domain=domain, slug=m.group("slug"), status=m.group("status").upper())
            body_lines = []
            header_phase = True
            continue
        if cur is None:
            continue
        if header_phase:
            hm = re.match(r"^(task|model|date|tags):\s?(.*)$", line)
            if hm:
                key, val = hm.group(1), hm.group(2).strip()
                if key == "tags":
                    cur.tags = [t.strip() for t in val.split(",") if t.strip()]
                else:
                    setattr(cur, key, val)
                continue
            # first non-header, non-blank line ends the header block
            if line.strip() == "" and not body_lines:
                continue
            header_phase = False
        body_lines.append(line)
    if cur is not None:
        cur.body = "\n".join(body_lines).strip("\n")
        entries.append(cur)
    return entries


def _all_sites() -> list[Path]:
    if not LORE_DIR.exists():
        return []
    return sorted(p for p in LORE_DIR.glob("*.md") if not p.name.startswith("_"))


def _today() -> str:
    return _dt.date.today().isoformat()


# ---- commands ---------------------------------------------------------------

def cmd_get(args: argparse.Namespace) -> int:
    path = _site_path(args.domain)
    if not path.exists():
        dom = _normalize_domain(args.domain)
        print(f"No lore yet for {dom}.")
        print(f"Start one:  lore.py log {dom} --slug <slug> --status WORKS \\")
        print('              --task "..." --model <model> --body "..."')
        print(f"(file will be created at lore/{dom}.md)")
        return 1
    sys.stdout.write(path.read_text(encoding="utf-8"))
    return 0


def cmd_log(args: argparse.Namespace) -> int:
    status = args.status.upper()
    if status not in STATUSES:
        print(f"status must be one of {', '.join(STATUSES)}", file=sys.stderr)
        return 2
    body = args.body
    if args.body_file:
        body = Path(args.body_file).read_text(encoding="utf-8")
    if body is None:
        body = sys.stdin.read() if not sys.stdin.isatty() else ""
    dom = _normalize_domain(args.domain)
    path = _site_path(dom)
    entry = Entry(
        domain=dom,
        slug=args.slug,
        status=status,
        task=args.task or "",
        model=args.model or os.environ.get("YCHROME_LORE_MODEL", ""),
        date=args.date or _today(),
        tags=[t.strip() for t in (args.tags or "").split(",") if t.strip()],
        body=body or "",
    )
    LORE_DIR.mkdir(parents=True, exist_ok=True)
    if not path.exists():
        path.write_text(
            f"# {dom}\n\n"
            "Known working methods for agent-driven browsing. Newest entries at the\n"
            "bottom (append-only). Read before co-browsing this site; log what you\n"
            "learn after. See ../SKILL.md for the contract.\n\n",
            encoding="utf-8",
        )
    with path.open("a", encoding="utf-8") as fh:
        fh.write("\n" + entry.to_markdown())
    print(f"logged {dom} · {entry.slug} ({status}) -> {path}")
    return 0


def cmd_search(args: argparse.Namespace) -> int:
    term = args.term.lower()
    hits = 0
    for path in _all_sites():
        for e in _parse_site(path):
            hay = " ".join([e.slug, e.status, e.task, " ".join(e.tags), e.body]).lower()
            if term in hay:
                hits += 1
                print(f"{e.domain} · {e.slug} · {e.status}  ({e.model} {e.date})")
                if e.task:
                    print(f"    task: {e.task}")
    if not hits:
        print(f"no entries match {args.term!r}")
        return 1
    return 0


def cmd_list(args: argparse.Namespace) -> int:
    sites = _all_sites()
    if not sites:
        print("no site lore yet")
        return 0
    for path in sites:
        entries = _parse_site(path)
        n_work = sum(1 for e in entries if e.status == "WORKS")
        mtime = _dt.date.fromtimestamp(path.stat().st_mtime).isoformat()
        print(f"{path.stem:<28} {len(entries):>3} entries ({n_work} WORKS)  updated {mtime}")
    return 0


def _build_cache() -> sqlite3.Connection:
    CACHE_DB.parent.mkdir(parents=True, exist_ok=True)
    if CACHE_DB.exists():
        CACHE_DB.unlink()
    con = sqlite3.connect(str(CACHE_DB))
    con.execute(
        "CREATE TABLE lore (domain TEXT, slug TEXT, status TEXT, task TEXT, "
        "model TEXT, date TEXT, tags TEXT, body TEXT)"
    )
    rows = []
    for path in _all_sites():
        for e in _parse_site(path):
            rows.append((e.domain, e.slug, e.status, e.task, e.model, e.date, ",".join(e.tags), e.body))
    con.executemany("INSERT INTO lore VALUES (?,?,?,?,?,?,?,?)", rows)
    con.commit()
    return con


def cmd_reindex(args: argparse.Namespace) -> int:
    con = _build_cache()
    n = con.execute("SELECT COUNT(*) FROM lore").fetchone()[0]
    con.close()
    print(f"reindexed {n} entries -> {CACHE_DB}")
    return 0


def cmd_query(args: argparse.Namespace) -> int:
    if not CACHE_DB.exists():
        _build_cache().close()
    con = sqlite3.connect(f"file:{CACHE_DB}?mode=ro", uri=True)
    try:
        cur = con.execute(args.sql)
        cols = [d[0] for d in cur.description] if cur.description else []
        if cols:
            print("\t".join(cols))
        for row in cur.fetchall():
            print("\t".join("" if v is None else str(v) for v in row))
    except sqlite3.Error as exc:
        print(f"sql error: {exc}", file=sys.stderr)
        return 2
    finally:
        con.close()
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="lore.py", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    g = sub.add_parser("get", help="print a site's lore")
    g.add_argument("domain")
    g.set_defaults(fn=cmd_get)

    lg = sub.add_parser("log", help="append a method entry")
    lg.add_argument("domain")
    lg.add_argument("--slug", required=True, help="short kebab id for the method")
    lg.add_argument("--status", required=True, help="|".join(STATUSES))
    lg.add_argument("--task", default="", help="what you were trying to do")
    lg.add_argument("--model", default="", help="model id (defaults $YCHROME_LORE_MODEL)")
    lg.add_argument("--date", default="", help="YYYY-MM-DD (defaults today)")
    lg.add_argument("--tags", default="", help="comma,separated")
    lg.add_argument("--body", default=None, help="method body; omit to read stdin")
    lg.add_argument("--body-file", default=None, help="read body from a file")
    lg.set_defaults(fn=cmd_log)

    s = sub.add_parser("search", help="grep entries across every site")
    s.add_argument("term")
    s.set_defaults(fn=cmd_search)

    ls = sub.add_parser("list", help="sites with entry counts")
    ls.set_defaults(fn=cmd_list)

    ri = sub.add_parser("reindex", help="rebuild the SQLite query cache")
    ri.set_defaults(fn=cmd_reindex)

    q = sub.add_parser("query", help="read-only SQL against the cache")
    q.add_argument("sql")
    q.set_defaults(fn=cmd_query)

    args = p.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    raise SystemExit(main())
