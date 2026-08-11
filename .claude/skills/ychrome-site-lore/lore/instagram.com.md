# instagram.com

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## export-via-accounts-centre · PARTIAL
task: obtain and read Instagram message/profile data; login itself not exercised
model: claude-opus-5
date: 2026-08-11
tags: export, dyi, archive, parsing, encoding

Not driven directly. This records what was established about Instagram's data while working
the shared Meta Accounts Centre and reading two Instagram DYI archives on disk. Scope stated
so the next agent knows what is measured and what is not: the export flow below is verified,
the instagram.com login is NOT.

EXPORTS GO THROUGH THE SHARED ACCOUNTS CENTRE, NOT THROUGH instagram.com.
Full flow in the `facebook.com` lore, slug `accounts-centre-export-dyi`. The profile chooser
there lists every linked profile with its platform, so an Instagram export is requested from
the same place as the Facebook one.
⛔ ONE PROFILE PER EXPORT. Facebook and Instagram cannot be covered by a single request.
⭐ That chooser is also the cheapest authoritative answer to "which Instagram accounts does
this login actually own today", which beats inferring it from archives on disk.

WHAT AN INSTAGRAM HTML EXPORT ACTUALLY CONTAINS, measured on two of them:
- Layout: `your_instagram_activity/messages/inbox/<handle>_<threadid>/message_1.html`.
- ⛔ HTML exports carry essentially NO MEDIA. Both archives held exactly ONE media file, the
  profile photo, in a ~1.1MB tree. No posts, stories, reels or shared photos came down.
  If media matters, request JSON with media quality set to higher, and expect a much larger
  archive.
- Useful identity files, all under `personal_information/` and
  `security_and_login_information/`:
    personal_information/personal_information/personal_information.html   username, name, email, DOB, private flag
    security_and_login_information/login_and_profile_creation/signup_details.html   ACCOUNT CREATION timestamp
    personal_information/personal_information/profile_changes.html        username/email change history
    connections/followers_and_following/{followers_1,following}.html
  ⭐ `signup_details.html` is the file that dates an account. It is ABSENT on older archives,
  and its absence is itself evidence that the account predates the export format.
  `profile_changes.html` can show a username set LATER than the earliest message, which means
  the account predates its own current name. Do not read a handle as an account identity.

PARSING: the HTML is a flat block sequence with a huge inline <style> prelude. A regex strip
returns the CSS as content. Use a real parser that drops <style>/<script>, then read
timestamps of the shape "Mon D, YYYY H:MM:SS am/pm".

ENCODING: these HTML exports are CLEAN UTF-8. A known-Bengali thread held 184 Bengali
codepoints and zero mojibake runs. ⛔ Do NOT apply the latin1/utf8 round-trip repair to them;
it would corrupt correct text. That repair is for the JSON exports.
