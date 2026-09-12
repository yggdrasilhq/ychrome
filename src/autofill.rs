//! THE LEARNED AUTOFILL PLANE: what a profile typed and which vault item it
//! used, remembered per site, so a notorious re-login (the owner's example:
//! an institute portal that demands its two-page login every start) costs one
//! click instead of a typing drill.
//!
//! Three facts live here, all per profile, none secret:
//!
//! 1. **Manual values.** What the human TYPED into non-password fields — the
//!    email the vault could not reach. Captured by the page shim, POSTed by
//!    the GUI to [`AUTOFILL_LEARN_ROUTE`].
//! 2. **Vault references.** Which vault item was click-filled into this
//!    site's secret field — a REFERENCE (item name + user), written by the
//!    same arm that builds the click-fill script ([`crate::sidebar`]'s
//!    `fill`/`card-fill` actions). The secret itself never leaves the vault;
//!    the plan endpoint resolves it fresh on every ask.
//! 3. **The plan.** On navigation the GUI asks [`AUTOFILL_PLAN_ROUTE`] for
//!    what this profile knows about the origin: the manual values inline and,
//!    when a vault item is remembered, a READY fill script built by the SAME
//!    machinery as click-fill ([`armed_fill_script`]) — the owner's law that
//!    the app computes the value and the GUI injects it via surface-eval.
//!
//! ⛔ THE CAPTURE NEVER SEES A SECRET. The shim classifies password-type,
//!    one-time-code and new-password fields and reports only a `secret: true`
//!    marker — the value is dropped page-side. [`learn`] refuses such entries
//!    a second time, because the cost of a captured secret is unbounded while
//!    the cost of a missed email is one retyping. The consult
//!    (lores/chain-of-thought 2026-09-12, gemini-3.8-flash-high) additionally
//!    barred page-load injection of secrets: the armed script fills on the
//!    user's FIRST interaction with the form, never on load — a tracker's
//!    script must not be able to read a filled password by watching the DOM.
//!
//! Growth is bounded per store (sites and identities), because a browser
//! feature that grows without bound is a disk leak with a UI.

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(crate) const AUTOFILL_LEARN_ROUTE: &str = "/autofill/learn";
pub(crate) const AUTOFILL_PLAN_ROUTE: &str = "/autofill/plan";

/// Bound the store: at most this many sites per profile. The oldest-updated
/// site falls out first — forgetting the site you visited once in March is
/// the correct behavior for a cache with no UI.
const MAX_SITES: usize = 256;
/// At most this many remembered identities per site. A login form has a
/// handful of fields; hundreds would mean the matcher is learning noise.
const MAX_IDENTITIES_PER_SITE: usize = 16;
/// A captured value longer than this is prose, not a form field.
const MAX_CAPTURED_LEN: usize = 512;

/// One remembered non-secret value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct LearnedValue {
    pub value: String,
    /// Unix millis, last write. The display never shows it; it orders decay.
    pub at: u64,
    /// How many times re-learned. A value typed once is a guess; typed five
    /// times is the identity.
    #[serde(default)]
    pub hits: u32,
}

/// A remembered vault-item REFERENCE for a site's secret field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct VaultRef {
    /// The vault item's name, exactly as the pane's fill action named it.
    pub item: String,
    /// The item's login (which credential inside the item) — empty if the
    /// fill action carried none.
    #[serde(default)]
    pub user: String,
    pub at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SiteMemory {
    /// Identity string (computed page-side by the shim's matcher) → value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    values: BTreeMap<String, LearnedValue>,
    /// Identity of a SECRET field → which vault item fills it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    vault: BTreeMap<String, VaultRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct Store {
    version: u32,
    /// Lowercased host (with port when non-default) → memory. A host, not a
    /// full URL: a login form on /signin and /login are the same site to a
    /// human, and the memory should say so.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    sites: BTreeMap<String, SiteMemory>,
}

impl Store {
    pub(crate) fn path(profile: &str) -> Result<PathBuf, String> {
        let dir = crate::profile_dir(profile).map_err(|e| e.to_string())?;
        Ok(dir.join("autofill.json"))
    }

    pub(crate) fn load(profile: &str) -> Store {
        match Self::path(profile) {
            Ok(path) => match std::fs::read(&path) {
                Ok(bytes) => serde_json::from_slice::<Store>(&bytes).unwrap_or_default(),
                Err(_) => Store::default(),
            },
            Err(_) => Store::default(),
        }
    }

    /// Atomic replace, 0600: the store holds identities and emails — not
    /// secrets, but not advertising material either. Same shape as a cookie
    /// jar's permissions.
    pub(crate) fn save(&self, profile: &str) -> Result<(), String> {
        let path = Self::path(profile)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let body = serde_json::to_vec(&self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &body).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Is this captured field one the plane must never store? The shim already
/// drops values page-side; this is the second line of defense, because the
/// learn route is reachable by anything holding the GUI's control token and
/// the cost of a stored secret is unbounded (see the module header).
fn forbidden_field(identity: &str, kind: &str, value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_CAPTURED_LEN {
        return true;
    }
    let kind = kind.to_ascii_lowercase();
    if kind == "password" || kind == "hidden" {
        return true;
    }
    let id = identity.to_ascii_lowercase();
    // One-time codes change every login; remembering them is noise at best
    // and a replayable token at worst. Every spelling the wild uses — otp,
    // one-time, onetime, sms-code's "code" alone is too greedy to bar. Consult Q6.
    let squashed = id.replace(['-', '_', ' '], "");
    id.contains("otp")
        || squashed.contains("onetime")
        || kind.contains("one-time")
        || kind == "otp"
}

/// Record what the human typed. `fields` is [{identity, kind, value}] from
/// the page shim, already filtered for secrets page-side and filtered AGAIN
/// here. Re-learning the same identity refreshes `at` and bumps `hits`; a
/// NEW identity on a full site evicts the coldest identity of that site.
pub(crate) fn learn(profile: &str, origin: &str, fields: &serde_json::Value) -> Result<serde_json::Value, String> {
    let site = normalize_origin(origin)?;
    let fields = fields
        .as_array()
        .ok_or_else(|| "fields must be an array".to_string())?;
    let mut store = Store::load(profile);
    let memory = store.sites.entry(site.clone()).or_default();
    let mut learned: u32 = 0;
    for field in fields {
        let (Some(identity), Some(value)) = (
            field.get("identity").and_then(serde_json::Value::as_str),
            field.get("value").and_then(serde_json::Value::as_str),
        ) else {
            continue;
        };
        let kind = field
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("text");
        if forbidden_field(identity, kind, value) {
            continue;
        }
        let entry = memory.values.entry(identity.to_string()).or_insert_with(|| LearnedValue {
            value: value.to_string(),
            at: now_millis(),
            hits: 0,
        });
        entry.value = value.to_string();
        entry.at = now_millis();
        entry.hits = entry.hits.saturating_add(1);
        learned += 1;
    }
    if learned == 0 {
        return Ok(json!({ "ok": true, "learned": 0 }));
    }
    trim_site(memory);
    trim_store(&mut store);
    store.save(profile)?;
    Ok(json!({ "ok": true, "learned": learned, "site": site }))
}

/// Record which vault item filled this site's secret field. Called by the
/// SAME arm that builds the click-fill script, so the memory can never name
/// an item the browser did not actually fill.
pub(crate) fn remember_vault(profile: &str, origin: &str, item: &str, user: &str) -> Result<(), String> {
    if item.trim().is_empty() {
        return Ok(());
    }
    let site = normalize_origin(origin)?;
    let mut store = Store::load(profile);
    let memory = store.sites.entry(site).or_default();
    memory.vault.insert(
        "secret".to_string(),
        VaultRef {
            item: item.to_string(),
            user: user.to_string(),
            at: now_millis(),
        },
    );
    trim_store(&mut store);
    store.save(profile)
}

/// What this profile knows about one origin, as the GUI's plan: manual values
/// inline and, when a vault item is remembered and the vault answers, the
/// READY armed fill script for the secret field. A locked or missing vault
/// degrades to the manual values plus a named hint — never an error, because
/// the email half of the owner's two-page login must keep working while the
/// vault is locked.
pub(crate) fn plan(profile: &str, origin: &str) -> Result<serde_json::Value, String> {
    let site = normalize_origin(origin)?;
    let store = Store::load(profile);
    let Some(memory) = store.sites.get(&site) else {
        return Ok(json!({ "origin": site, "values": [], "secret": null }));
    };
    let values: Vec<serde_json::Value> = memory
        .values
        .iter()
        .map(|(identity, learned)| {
            json!({
                "identity": identity,
                "value": learned.value,
                "strength": if learned.hits >= 2 { "settled" } else { "tentative" },
            })
        })
        .collect();
    // The remembered vault item's username is ALSO a candidate for the site's
    // identity field (consult Q6): the owner logs into the same site
    // differently per profile, and the item remembered on THIS profile is by
    // construction the identity THIS profile uses. The shim offers it only to
    // email/username-shaped fields and only when no learned value claims the
    // field first.
    let vault_username = memory
        .vault
        .get("secret")
        .map(|reference| reference.user.clone())
        .filter(|user| !user.is_empty());
    let mut values = values;
    if let Some(user) = &vault_username {
        let taken = values.iter().any(|entry| {
            entry["identity"].as_str().is_some_and(|id| {
                id.starts_with("ac:email")
                    || id.starts_with("ac:username")
                    || id.starts_with("aria:")
                    || id.starts_with("name:")
                    || id.starts_with("id:")
            })
        });
        if !taken {
            values.push(json!({
                "identity": "weak:vault-username",
                "value": user,
                "strength": "weak",
            }));
        }
    }
    let secret = match memory.vault.get("secret") {
        None => json!(null),
        Some(reference) => {
            match crate::sidebar::vault_op(json!({
                "op": "get",
                "name": reference.item,
                "user": (!reference.user.is_empty()).then_some(reference.user.as_str()),
            })) {
                Ok(reply) => match reply["entry"]["password"].as_str() {
                    Some(password) if !password.is_empty() => json!({
                        "item": reference.item,
                        "script": armed_fill_script(
                            (!reference.user.is_empty()).then_some(reference.user.as_str()),
                            password,
                        ),
                    }),
                    _ => json!({ "item": reference.item, "error": "item has no password" }),
                },
                Err(error) => json!({ "item": reference.item, "error": error.to_string() }),
            }
        }
    };
    Ok(json!({ "origin": site, "values": values, "secret": secret }))
}

/// `learn` + `remember_vault` collapse to nothing without the site name; one
/// normalizer, so `/autofill/learn` and `/autofill/plan` cannot disagree
/// about what "the same site" means (the AGENTS.md single-owner law).
pub(crate) fn normalize_origin(origin: &str) -> Result<String, String> {
    let trimmed = origin.trim();
    if trimmed.is_empty() {
        return Err("origin is required".to_string());
    }
    let host = if let Ok(parsed) = url::Url::parse(trimmed) {
        parsed.host_str().map(str::to_string)
    } else {
        // The shim may send a bare host when the surface has no URL yet.
        trimmed.split('/').next().map(str::to_string)
    };
    let mut host = host.ok_or_else(|| "origin has no host".to_string())?;
    host.make_ascii_lowercase();
    // Keep a non-default port: two services on one host are two sites to the
    // memory, the same distinction a cookie jar makes.
    if let Ok(parsed) = url::Url::parse(trimmed) {
        if let Some(port) = parsed.port() {
            host = format!("{host}:{port}");
        }
    }
    Ok(host)
}

/// Evict the coldest identities when a site outgrows its budget.
fn trim_site(memory: &mut SiteMemory) {
    while memory.values.len() > MAX_IDENTITIES_PER_SITE {
        let coldest = memory
            .values
            .iter()
            .min_by_key(|(_, learned)| (learned.at, std::cmp::Reverse(learned.hits)))
            .map(|(identity, _)| identity.clone());
        match coldest {
            Some(identity) => {
                memory.values.remove(&identity);
            }
            None => break,
        }
    }
}

/// Evict the least-recently-updated sites when the store outgrows its budget.
fn trim_store(store: &mut Store) {
    while store.sites.len() > MAX_SITES {
        let coldest = store
            .sites
            .iter()
            .filter(|(_, memory)| memory.values.is_empty() && memory.vault.is_empty())
            .map(|(site, _)| site.clone())
            .next()
            .or_else(|| {
                store
                    .sites
                    .iter()
                    .min_by_key(|(_, memory)| {
                        memory
                            .values
                            .values()
                            .map(|learned| learned.at)
                            .chain(memory.vault.values().map(|reference| reference.at))
                            .max()
                            .unwrap_or(0)
                    })
                    .map(|(site, _)| site.clone())
            });
        match coldest {
            Some(site) => {
                store.sites.remove(&site);
            }
            None => break,
        }
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

/// The click-fill body, ARMED: the script installs one listener set and fills
/// on the user's FIRST interaction (focus or press) inside the form that owns
/// a password field — never on page load (consult Q1: a page-load injection
/// is readable by any third-party script watching the DOM; a user-gesture-
/// gated one is not, because it has not happened yet). Returns a verdict
/// object on a `ychrome-autofill` detail event so the shim can toast honestly.
pub(crate) fn armed_fill_script(username: Option<&str>, password: &str) -> String {
    let body = fill_script_body(username, password);
    format!(
        r#"(function() {{
{SET_FIELD}
  let armed = false;
  const run = () => {{
    if (armed) return;
    armed = true;
    document.removeEventListener('focusin', onGesture, true);
    document.removeEventListener('pointerdown', onGesture, true);
    const verdict = (function() {{
{body}
    }})();
    document.dispatchEvent(new CustomEvent('ychrome-autofill', {{ detail: verdict }}));
  }};
  const onGesture = (event) => {{
    const form = event.target && event.target.closest ? event.target.closest('form') : null;
    if (!armed && (form || document.querySelector('input[type=password]'))) run();
  }};
  document.addEventListener('focusin', onGesture, true);
  document.addEventListener('pointerdown', onGesture, true);
  // SPA logins mount the form late (consult Q5): watch until it exists, then
  // the first gesture on it fires. The watcher retires with the arming.
  const watch = new MutationObserver(() => {{
    if (document.querySelector('input[type=password]:not([disabled])')) run();
  }});
  watch.observe(document.documentElement, {{ childList: true, subtree: true }});
}})()"#,
        SET_FIELD = crate::sidebar::SET_FIELD,
        body = body,
    )
}

/// The field-picking and readback half of [`crate::sidebar::fill_script`],
/// word for word — ONE owner of "which fields get the credential". The
/// sidebar's immediate script and this plane's armed script differ only in
/// WHEN it runs.
pub(crate) fn fill_script_body(username: Option<&str>, password: &str) -> String {
    let username_literal = crate::sidebar::js_string(username.unwrap_or(""));
    format!(
        r#"  const secrets = Array.from(document.querySelectorAll('input[type=password]:not([disabled])'));
  const pw = secrets[0] || null;
  let user = null;
  if (pw) {{
    const form = pw.form || document;
    const candidates = Array.from(form.querySelectorAll('input'));
    const pwIndex = candidates.indexOf(pw);
    user = candidates.slice(0, pwIndex < 0 ? candidates.length : pwIndex).reverse().find((el) =>
      ['text', 'email', 'tel', ''].includes((el.type || '').toLowerCase()) && !el.disabled);
  }}
  if (!user) {{
    user = document.querySelector('input[autocomplete=username], input[name*=user i], input[type=email]');
  }}
  const CONFIRM = /confirm|retype|re-type|repeat|verify|again/i;
  const describes = (el) => [el.getAttribute('name'), el.getAttribute('id'),
    el.getAttribute('placeholder'), el.getAttribute('aria-label'),
    el.getAttribute('autocomplete')].filter(Boolean).join(' ');
  const twin = secrets.slice(1).find((el) => CONFIRM.test(describes(el))) || null;
  const fields = [];
  const record = (label, el, verdict) => {{
    fields.push(Object.assign({{ field: label, target: ychromeField(el) }}, verdict));
    return verdict;
  }};
  const userVerdict = {username} ? record('username', user, ychromeSet(user, {username})) : null;
  const pwVerdict = record('secret', pw, ychromeSet(pw, {password}));
  const twinVerdict = twin ? record('secret-confirm', twin, ychromeSet(twin, {password})) : null;
  if (pw) {{ pw.focus(); }}
  let filled = pwVerdict.present
    ? 'filled'
    : ((userVerdict && userVerdict.present) ? 'user-only' : 'no-fields');
  if (fields.some((f) => f.present && !f.ok)) {{ filled = 'unverified'; }}
  return {{
    filled: filled,
    fields: fields,
    secret_field_count: secrets.length,
    confirm: twin ? (twinVerdict.ok ? 'filled' : 'unverified')
                  : (secrets.length > 1 ? 'present-but-unnamed' : 'absent'),
  }}"#,
        username = username_literal,
        password = crate::sidebar::js_string(password),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_profile(tag: &str) -> String {
        let name = format!("autofill-test-{tag}-{}", std::process::id());
        std::fs::create_dir_all(crate::profile_dir(&name).unwrap()).unwrap();
        name
    }

    #[test]
    fn learn_stores_a_value_and_plan_answers_it() {
        let profile = temp_profile("roundtrip");
        let reply = learn(
            &profile,
            "https://Learn.CFAInstitute.org/signin",
            &json!([{ "identity": "ac:email", "kind": "email", "value": "someone@example.net" }]),
        )
        .unwrap();
        assert_eq!(reply["learned"], 1);
        // The origin normalized to a lowercase host without the path.
        assert_eq!(reply["site"], "learn.cfainstitute.org");
        let plan = plan(&profile, "https://learn.cfainstitute.org/other-page").unwrap();
        assert_eq!(plan["values"][0]["identity"], "ac:email");
        assert_eq!(plan["values"][0]["value"], "someone@example.net");
        assert_eq!(plan["values"][0]["strength"], "tentative");
        std::fs::remove_dir_all(crate::profile_dir(&profile).unwrap()).ok();
    }

    #[test]
    fn learn_never_stores_a_secret_shaped_field() {
        let profile = temp_profile("secrecy");
        let reply = learn(
            &profile,
            "https://site.example",
            &json!([
                { "identity": "name:password", "kind": "password", "value": "real-secret" },
                { "identity": "ac:one-time-code", "kind": "text", "value": "123456" },
                { "identity": "ac:email", "kind": "email", "value": "" }
            ]),
        )
        .unwrap();
        assert_eq!(reply["learned"], 0, "password, OTP and empty are all refused");
        let plan = plan(&profile, "https://site.example").unwrap();
        assert_eq!(plan["values"].as_array().unwrap().len(), 0);
        std::fs::remove_dir_all(crate::profile_dir(&profile).unwrap()).ok();
    }

    #[test]
    fn remember_vault_then_plan_names_the_item_and_degrades_without_an_agent() {
        let profile = temp_profile("vaultref");
        remember_vault(&profile, "https://site.example", "CFA Login", "someone@example.net").unwrap();
        let plan = plan(&profile, "site.example").unwrap();
        // The reference survives (this test box has no vault agent, so the
        // plan names the failure instead of failing the ask — the email half
        // of a two-page login must keep working while the vault is locked).
        assert_eq!(plan["secret"]["item"], "CFA Login");
        assert!(plan["secret"]["error"].is_string());
        // The remembered item's username is offered as the weak identity
        // candidate (consult Q6).
        assert_eq!(plan["values"][0]["identity"], "weak:vault-username");
        assert_eq!(plan["values"][0]["strength"], "weak");
        std::fs::remove_dir_all(crate::profile_dir(&profile).unwrap()).ok();
    }

    #[test]
    fn the_armed_script_defers_every_secret_and_names_its_verdict() {
        let script = armed_fill_script(Some("someone@example.net"), "hunter2hunter2");
        // The secret is embedded (the owner's law: the app computes, the GUI
        // injects), but the fill runs ONLY behind the gesture gate.
        assert!(script.contains("hunter2hunter2"));
        assert!(script.contains("addEventListener('focusin', onGesture, true)"));
        assert!(script.contains("addEventListener('pointerdown', onGesture, true)"));
        assert!(script.contains("MutationObserver"), "SPA logins mount the form late");
        assert!(!script.contains("document.addEventListener('DOMContentLoaded'"));
    }

    #[test]
    fn normalization_keeps_nondefault_ports_and_folds_case() {
        assert_eq!(normalize_origin("HTTPS://Example.COM/a").unwrap(), "example.com");
        assert_eq!(normalize_origin("http://localhost:8443/x").unwrap(), "localhost:8443");
        assert!(normalize_origin("").is_err());
    }

    #[test]
    fn the_armed_script_and_the_click_fill_script_pick_fields_identically() {
        let armed = armed_fill_script(Some("u@example.net"), "pw");
        let armed_body = armed
            .split("const run = () => {").nth(1).unwrap()
            .split("}})();").next().unwrap();
        let immediate = crate::sidebar::fill_script("u@example.net", "pw");
        for anchor in [
            "const secrets = Array.from(document.querySelectorAll('input[type=password]:not([disabled])'))",
            "input[autocomplete=username], input[name*=user i], input[type=email]",
        ] {
            assert!(armed_body.contains(anchor), "armed body missing {anchor}");
            assert!(immediate.contains(anchor), "click-fill missing {anchor}");
        }
    }
}

/// The page-side half of the learned-autofill plane, delivered as a synthetic
/// userscript (the SponsorBlock config precedent: compiled-in, one placement).
/// Two halves, one document-start body:
///
/// **Capture** — `change` events plus a submit-button `pointerdown` (consult
/// Q2: SPA logins intercept submit and unmount before blur fires). Only
/// non-secret fields are stashed, and a secret is classified THREE ways
/// before a value may leave the page: input type, autocomplete token, and the
/// identity string. The stashed fields ride the EXISTING `yggtermSurface`
/// message channel; the route they reach is gated by the signer's token (the
/// page routes' existing credential), so a page can forge captures for its
/// OWN origin and nothing else — forgeable learn is noise, forgeable PLAN is
/// a credential, which is why the plan stays GUI-only.
///
/// **Apply** — `__ychromeApplyPlan(plan)` is called BY THE HOST (surface eval)
/// with the origin's learned values: exact identities fill directly, the weak
/// vault-username candidate fills only email/username-shaped fields, and a
/// field the human already typed into is never overwritten. The plan's SECRET
/// script does NOT ride through here — the host evaluates it separately, so
/// a strict-CSP page cannot break the arming with `Function`.
pub(crate) fn capture_userscript(token: &str) -> crate::userscript::Userscript {
    let body = r#"// ==UserScript==
// @match      *://*/*
// @run-at     document-start
// ==/UserScript==
(function () {
  if (window.__ychromeAutofillShim) return;
  window.__ychromeAutofillShim = true;
  var SECRET = function (el) {
    var t = (el.getAttribute('type') || '').toLowerCase();
    if (t === 'password' || t === 'hidden') return true;
    var ac = (el.getAttribute('autocomplete') || '').toLowerCase();
    if (ac.indexOf('one-time-code') >= 0 || ac.indexOf('new-password') >= 0 || ac.indexOf('current-password') >= 0) return true;
    return false;
  };
  var identityFor = function (el) {
    var ac = el.getAttribute('autocomplete');
    if (ac) return 'ac:' + ac.toLowerCase();
    var lbl = (el.labels && el.labels[0] && el.labels[0].innerText || '').trim().toLowerCase().slice(0, 48);
    if (lbl) return 'label:' + lbl;
    var aria = el.getAttribute('aria-label');
    if (aria) return 'aria:' + aria.trim().toLowerCase().slice(0, 48);
    var nm = el.getAttribute('name');
    var id = nm ? null : el.getAttribute('id');
    if (nm || id) return (nm ? 'name:' : 'id:') + (nm || id).toLowerCase();
    if (el.form) {
      var inputs = Array.prototype.slice.call(el.form.querySelectorAll('input'));
      return 'idx:' + inputs.indexOf(el);
    }
    return null;
  };
  var stash = {};
  var stashCount = 0;
  var capture = function (el) {
    if (!el || !el.matches || !el.matches('input,textarea') || el.disabled || el.readOnly) return;
    if (SECRET(el)) return;
    var v = (el.value || '').trim();
    if (!v || v.length > 512) return;
    var id = identityFor(el);
    if (!id) return;
    if (!stash[id]) stashCount += 1;
    stash[id] = { identity: id, kind: (el.getAttribute('type') || 'text').toLowerCase(), value: v };
  };
  var TOKEN = '{token}';
  var flush = function () {
    if (!stashCount) return;
    var fields = Object.keys(stash).map(function (k) { return stash[k]; });
    stash = {}; stashCount = 0;
    // The page credential (the signer token, the same one /fido2 uses) rides
    // the yggterm-appctl bridge, which forwards it as the signer header. A
    // page may learn for its own origin; the PLAN half of this plane stays
    // GUI-only, because the plan resolves vault references into ready fill
    // scripts — the learn gate buys quietness, not secrecy.
    fetch('yggterm-appctl://signer/autofill/learn', {{
      method: 'POST',
      headers: {{ 'Content-Type': 'application/json', 'X-Ychrome-Fido2': TOKEN }},
      body: JSON.stringify({{ origin: location.host || '', fields: fields }})
    }}).catch(function () {{ /* standalone window: no bridge, nothing to learn into */ }});
  };
  document.addEventListener('change', function (e) { capture(e.target); }, true);
  document.addEventListener('pointerdown', function (e) {
    var b = e.target && e.target.closest ? e.target.closest('button, [role=button], input[type=submit]') : null;
    if (!b) return;
    var text = ((b.innerText || b.value || '') + '').toLowerCase();
    if ((b.closest && b.closest('form')) || /sign|log|next|continue|submit/.test(text)) {
      Array.prototype.slice.call(document.querySelectorAll('input,textarea')).forEach(capture);
      flush();
    }
  }, true);
  window.addEventListener('pagehide', flush);
  window.__ychromeApplyPlan = function (plan) {
    try {
      var used = {};
      var byIdentity = {};
      (plan.values || []).forEach(function (v) { byIdentity[v.identity] = v; });
      var weak = byIdentity['weak:vault-username'] || null;
      var matchFor = function (el) {
        var id = identityFor(el);
        if (id && byIdentity[id] && !used[id]) return byIdentity[id];
        if (weak && !used[weak.identity]) {
          var t = (el.getAttribute('type') || '').toLowerCase();
          var ac = (el.getAttribute('autocomplete') || '').toLowerCase();
          if (t === 'email' || ac.indexOf('email') >= 0 || ac.indexOf('username') >= 0) return weak;
        }
        return null;
      };
      var apply = function () {
        Array.prototype.slice.call(document.querySelectorAll('input,textarea')).forEach(function (el) {
          if (SECRET(el) || el.disabled || el.readOnly) return;
          if (el.value && el.value.length) return;
          var hit = matchFor(el);
          if (!hit) return;
          var proto = Object.getPrototypeOf(el);
          var setter = Object.getOwnPropertyDescriptor(proto, 'value');
          if (setter && setter.set) setter.set.call(el, hit.value); else el.value = hit.value;
          el.dispatchEvent(new Event('input', { bubbles: true }));
          el.dispatchEvent(new Event('change', { bubbles: true }));
          used[hit.identity] = true;
        });
      };
      apply();
      if (window.MutationObserver) {
        new MutationObserver(apply).observe(document.documentElement, { childList: true, subtree: true });
      }
    } catch (e) { /* a plan that cannot apply must never break the page */ }
  };
})();
"#;
    // Authored above with its own metadata block; `parse` is infallible and
    // the placement gate is for FILE scripts, not compiled-in ones (the
    // SponsorBlock config precedent).
    let body = body.replace("{token}", token);
    crate::userscript::parse(&body)
}
