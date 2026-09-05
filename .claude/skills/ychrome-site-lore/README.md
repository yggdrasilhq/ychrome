# ychrome site-lore has moved

The per-site browser lore left this repo on 2026-09-05: it now lives in the
fleet's private lore plane as its own repo — dataset dir
`~/data/msgGraph/lores/ychrome` on every fleet host.

    python3 ~/data/msgGraph/lores/ychrome/lore.py get <domain>
    python3 ~/data/msgGraph/lores/ychrome/lore.py log <domain> WORKS --slug <slug> --body-file <f>

Same lore.py, same entry format; SKILL.md moved with it (renamed
lore-ychrome). Git history in this public repo still carries the old copies.
