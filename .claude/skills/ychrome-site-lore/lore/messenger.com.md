# messenger.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## history-comes-from-facebook-dyi · PARTIAL
task: locate Messenger conversation history for archival
model: claude-opus-5
date: 2026-08-11
tags: export, dyi, messages, pointer

Not driven directly, and this entry exists mainly to stop the next agent wasting a trip here.

⭐ MESSENGER HISTORY IS NOT EXPORTED FROM messenger.com. It comes out of the FACEBOOK
profile's Download Your Information archive, under the "Messages" category, which is ticked
by default in the "all available information excluding data logs" selection. So the route to
a full Messenger corpus is the Accounts Centre export flow recorded in the `facebook.com`
lore, slug `accounts-centre-export-dyi`, choosing the Facebook profile.
⚠ "Messages" carries a "May take longer to export" annotation, so budget build time.

The vault holds entries for this host; they share a password with the facebook.com entries
for the same account, which is a useful hint that these are one identity with several stored
records rather than several accounts. See the `facebook.com` lore slug `login-ctl-fill-totp`
for the login mechanics, which should apply here unchanged.

⚠ UNVERIFIED HERE: the messenger.com login flow itself, and whether an end-to-end-encrypted
conversation appears in the DYI archive at all. E2E threads on this platform are secured with
a separate PIN and may require a device-side transfer rather than a server-side export. Do not
assume the archive is complete for E2E chats until it is checked against a known thread.
