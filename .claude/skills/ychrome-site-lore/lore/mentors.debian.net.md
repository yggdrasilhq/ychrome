# mentors.debian.net

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## login-via-vault · WORKS
task: log in as an uploader
model: claude-opus-4-8
date: 2026-07-22
tags: login, vault, django

Debexpo login at /accounts/login/ (Django form). Fields:
- #id_username (email), #id_password, hidden csrfmiddlewaretoken + next.

Credential: `ychrome-vault get mentors.debian.net --field username|password`. The
vault must be unlocked ON THE GUI HOST you drive (per-host agents), not the host
you ssh from. Inject the password without it ever hitting argv/logs: base64 it on
the GUI host and embed via atob() in JS piped to `yggterm server app web eval
--stdin` (never a literal in a command line or in lore).

Fill both fields, dispatch an `input` event on each, then `p.form.submit()`.
Success redirects to /accounts/profile/; your uploads are at /packages/my/.

Gotcha: vault is per-host. Proof (2026-07-22): filled + submitted, landed on
/accounts/profile/ with a Logout link; /packages/my/ listed the uploads.

## delete-package · WORKS
task: delete an uploaded source package (all versions)
model: claude-opus-4-8
date: 2026-07-22
tags: delete, debexpo, csrf

Remove an uploaded source package (all versions) from /package/<name>/.

The "Delete this package" control is
<input name="commit_delete" value="Delete this package"> — but Debexpo renders it
OUTSIDE its <form> (the <form action="/package/<name>/delete/"> parses EMPTY), so a
naive .click() silently no-ops. Build + submit the POST yourself:
  POST /package/<name>/delete/
  body: csrfmiddlewaretoken (read live from any input[name=csrfmiddlewaretoken] on
        the page) + commit_delete=Delete this package
Delete a single upload instead: POST /package/<name>/delete/<uploadId>/.
Verify: /packages/my/ shows "No packages" (or the package is gone).

Gotcha: each delete redirects to /packages/my/, so `location.href=/package/<next>/`
then an immediate `web eval` RACES the load — URL-guard, or `server app web wait
--until load_finished`. Proof (2026-07-22): deleted 3 packages (35+28+4 uploads);
/packages/my/ then read "No packages".
