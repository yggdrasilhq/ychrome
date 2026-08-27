//! Phase C — identity parity (`docs/agent-engine.md` §8).
//!
//! **This module owns nothing.** It is an ADAPTER: it asks the existing owners
//! what this profile's identity is and turns their answer into WebKit objects.
//! Every concept below already has exactly one owner elsewhere, and AGENTS.md's
//! reuse-never-fork rule means the engine may consume them but never keep a
//! second copy:
//!
//! | concept | owner | consumed as |
//! |---|---|---|
//! | profile jar | `crate::profile_dir` | `WebsiteDataManager` base dirs |
//! | adblock ruleset | `webpolicy::policy().adblock_rules` | a compiled `WebKitUserContentFilter` |
//! | userscripts + placement | `webpolicy::policy().userscripts` | `WebKitUserScript` per script |
//! | UA | `webpolicy::policy().user_agent` (from `useragent`) | `Settings::set_user_agent` |
//! | per-site zoom | `webzoom::zoom_for_host` | `WebViewExt::set_zoom_level` |
//! | egress | the caller's SOCKS endpoint | `NetworkProxySettings` |
//!
//! One thing it does own, because nothing else can: the profile's
//! **content→UI-process message channels** ([`MessageChannel`]). A script
//! message handler has to be registered on the `UserContentManager` before a
//! document is created on it, and this module is where that manager is built —
//! so an owner elsewhere would be an owner that arrives too late.
//!
//! The engine and the visible surface must be the SAME browser to a website.
//! That is why the jar directory is `crate::profile_dir`'s, unmodified: a page
//! logged in under profile X in the visible surface is logged in here with no
//! re-auth, because it is literally the same cookie jar on disk.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Result, bail};
use glib::translate::{IntoGlib, ToGlibPtr, from_glib_full};
use javascriptcore::ValueExt;
use serde_json::{Value, json};
use webkit2gtk::{
    CookieManagerExt, CookiePersistentStorage, NetworkProxyMode, NetworkProxySettings,
    UserContentInjectedFrames, UserContentManager, UserContentManagerExt, UserScript,
    UserScriptInjectionTime, WebContext, WebsiteDataManager, WebsiteDataManagerExt,
};

use crate::userscript::ScriptWorld;

/// The cookie jar's filename inside a profile directory.
///
/// It is `cookies` because that is what wry writes for the visible surface and
/// the standalone window. The engine and the surface must be ONE browser to a
/// website (this module's whole premise), and two files would make them two.
/// If wry ever renames it, this is the line that has to move with it.
pub const COOKIE_JAR_FILE: &str = "cookies";

/// The name of the isolated world engine userscripts run in.
///
/// A NAME is what makes a world isolated in WebKit; there is no "anonymous
/// isolated world". One constant so every isolated script shares one world and
/// can see each other's globals, exactly as they do on the visible surface.
///
/// Public because `Engine::eval_in_world` needs to name the SAME world the
/// engine's own scripts run in — a second spelling would put the frame verb's
/// dispatcher in a world where its own bridge does not exist.
pub const ISOLATED_WORLD: &str = "ychrome";

/// One profile's WebKit identity, built once and reused for every page on that
/// profile.
///
/// Reused rather than rebuilt because a second `WebContext` over the same jar
/// directory is a second writer to it, and because attaching the content filter
/// is the expensive step this whole module has to be careful about.
#[derive(Clone)]
pub struct ProfileIdentity {
    pub context: WebContext,
    pub data: WebsiteDataManager,
    pub content: UserContentManager,
    pub user_agent: Option<String>,
    /// What actually got attached, for the journal. Claims here are checked
    /// against reality by `engine identity`.
    pub applied: serde_json::Value,
}

thread_local! {
    /// Per-profile identities, on the engine thread that owns the GTK objects.
    static IDENTITIES: RefCell<HashMap<String, ProfileIdentity>> =
        RefCell::new(HashMap::new());
}

// ---------------------------------------------------------------------------
// Content → UI-process message channels
// ---------------------------------------------------------------------------

/// A content→UI-process channel: a WebKit **script message handler**,
/// registered on a profile's `UserContentManager` in a named world.
///
/// It is the one direction that does not travel over the page's own wires. A
/// `postMessage` between frames is observable and forgeable by whatever
/// document sits in the frame (measured — `engine worlds`), so a reply that
/// went back that way would hand the top page an answer about another origin.
/// `window.webkit.messageHandlers.<name>.postMessage()` goes to the UI process
/// instead, and a world it was not registered in has no such handler to call.
///
/// ⛔ **The world is load-bearing and getting it backwards is silent**, exactly
/// as it is for a userscript: a handler registered in the main world is one the
/// PAGE can call, and a reply channel the page can call is not a reply channel.
pub struct MessageChannel {
    pub name: String,
    pub world: ScriptWorld,
}

/// The channels every identity built from here on will carry.
///
/// ⛔ **Registration has to happen while the identity is being BUILT.** An
/// identity is constructed once, cached per profile, and every page on that
/// profile loads against it — `window.webkit.messageHandlers` is populated from
/// the manager's registrations as a document is created, so a channel armed
/// after the first page opened exists on nothing that page can see.
static CHANNELS: Mutex<Vec<MessageChannel>> = Mutex::new(Vec::new());

/// What the REGISTRATION CALL answered, per channel, per profile built. The
/// readback, not the request — `register_script_message_handler*` returns false
/// for a name already taken, and a channel nobody registered is silent in
/// exactly the way a channel that cannot reach us is.
static REGISTERED: Mutex<Vec<Value>> = Mutex::new(Vec::new());

/// Everything the UI process has received on an armed channel, oldest first.
static DELIVERED: Mutex<Vec<Value>> = Mutex::new(Vec::new());

/// How many deliveries are kept before the oldest is dropped.
///
/// A cap, because the frame verb made this a SHIPPED path rather than a probe's
/// scratch list: every cross-origin read a long-lived daemon ever performs would
/// otherwise sit here forever. [`take_delivered`] removes what it consumes, so
/// in normal operation the list holds only replies nobody claimed — a forged
/// read from a page, or an answer that arrived after its caller timed out.
const DELIVERED_CAP: usize = 512;

/// Arm a channel for every profile identity built after this call.
///
/// Process-wide rather than thread-local on purpose: the caller arming a
/// channel is not the engine thread that builds the identity, and the reader
/// draining the deliveries is neither of them.
pub fn arm_message_channel(name: &str, world: ScriptWorld) {
    if let Ok(mut channels) = CHANNELS.lock() {
        channels.push(MessageChannel {
            name: name.to_string(),
            world,
        });
    }
}

/// What each armed channel's registration call actually returned.
pub fn channel_registrations() -> Vec<Value> {
    REGISTERED
        .lock()
        .map(|held| held.clone())
        .unwrap_or_default()
}

/// Everything the UI process has received on an armed channel, oldest first.
///
/// ⚠ **A record carries what the SENDING FRAME SAID ABOUT ITSELF, and nothing
/// more.** `script-message-received` hands back the `UserContentManager` — not
/// the WebView, and not the frame — so "I am the child at origin X" is a claim
/// this process cannot check. Reading such a claim is harmless; ROUTING on it
/// would be trusting a page's word about which document it is.
pub fn delivered_messages() -> Vec<Value> {
    DELIVERED
        .lock()
        .map(|held| held.clone())
        .unwrap_or_default()
}

/// CONSUME the oldest delivery on `channel` whose payload the caller accepts.
///
/// Taking rather than reading is what makes a reply a reply: a caller correlates
/// on its own request's identity, and once it has that answer nobody else may
/// see it again. The predicate is the CALLER's, because the correlation rule
/// belongs to whoever owns the protocol on that channel — this module owns the
/// channel, not what travels on it.
///
/// ⚠ The predicate reads a payload, which is the SENDING FRAME'S OWN CLAIM
/// (see [`delivered_messages`]). Correlating on a token the caller itself
/// generated is sound; routing on a `frame` or `origin` the payload asserts is
/// not, and no caller here does it.
pub fn take_delivered(channel: &str, accepts: impl Fn(&Value) -> bool) -> Option<Value> {
    let mut held = DELIVERED.lock().ok()?;
    let index = held
        .iter()
        .position(|row| row["channel"] == json!(channel) && accepts(&row["payload"]))?;
    let mut row = held.remove(index);
    Some(row["payload"].take())
}

/// Register one channel on a manager and wire its receiver. Engine thread only.
fn register_channel(content: &UserContentManager, channel: &MessageChannel) -> Value {
    let registered = match channel.world {
        // The page's OWN world: `window.webkit.messageHandlers.<name>` exists
        // for the page's scripts, which is right for a channel a page is meant
        // to use and wrong for a reply path.
        ScriptWorld::Main => content.register_script_message_handler(&channel.name),
        // A NAMED world — the engine's own, the same one `attach_script` puts
        // isolated userscripts in, or our scripts would find no handler.
        ScriptWorld::Isolated => {
            content.register_script_message_handler_in_world(&channel.name, ISOLATED_WORLD)
        }
    };
    if registered {
        let name = channel.name.clone();
        let world = channel.world.as_str();
        content.connect_script_message_received(Some(&channel.name), move |_manager, result| {
            // `to_json` is the decoder `/engine/eval` already uses, so a payload
            // reads here exactly as the same value would read there.
            let payload = result
                .js_value()
                .and_then(|value| value.to_json(0))
                .and_then(|text: glib::GString| serde_json::from_str::<Value>(text.as_str()).ok())
                .unwrap_or(Value::Null);
            if let Ok(mut held) = DELIVERED.lock() {
                // Oldest out first. A flood from a page posting forged reads
                // must not be able to grow this without bound, and the thing a
                // caller is waiting for is always the NEWEST arrival.
                while held.len() >= DELIVERED_CAP {
                    held.remove(0);
                }
                held.push(json!({ "channel": name, "world": world, "payload": payload }));
            }
        });
    }
    let detail = json!({
        "name": channel.name,
        "world": channel.world.as_str(),
        "registered": registered,
    });
    if let Ok(mut held) = REGISTERED.lock() {
        held.push(detail.clone());
    }
    detail
}

/// Build (or reuse) a profile's identity. Engine thread only.
pub fn for_profile(profile: &str) -> Result<ProfileIdentity> {
    if let Some(existing) = IDENTITIES.with(|map| map.borrow().get(profile).cloned()) {
        return Ok(existing);
    }
    let built = build(profile)?;
    IDENTITIES.with(|map| {
        map.borrow_mut().insert(profile.to_string(), built.clone());
    });
    Ok(built)
}

fn build(profile: &str) -> Result<ProfileIdentity> {
    let started = Instant::now();
    // THE profile jar — the same directory the visible surface uses. Not a
    // copy, not an engine-specific sibling: the point of Phase C is that these
    // are one identity.
    let dir = crate::profile_dir(profile)?;
    // ⭐ BOTH base directories are the profile jar, because wry passes the SAME
    // path for both (`webkitgtk/web_context.rs`: `base_cache_directory` and
    // `base_data_directory` are each `data_directory`). A cache directory of our
    // own would be exactly the "engine-specific sibling" the paragraph above
    // says this is not.
    //
    // It was `dir.join("cache")` until 2026-08-01, and the consequence is the
    // cookie bug's twin: WebKit's HTTP disk cache lives under
    // `<base_cache_directory>/WebKitCache`, so the engine filled
    // `<profile>/cache/WebKitCache` while the visible browser filled
    // `<profile>/WebKitCache` and NEITHER could read the other's records.
    // Measured on dev and the GUI host before the fix: three profiles on each host
    // carried BOTH trees, one written by the engine and one by the surface.
    // Every engine page therefore refetched assets the user's own browser had
    // on disk a second earlier, and vice versa — "one browser" was true for
    // cookies and false for every byte of cacheable content.
    //
    // `hsts-storage.sqlite` and `CacheStorage` (the service-worker Cache API)
    // live under the same root and were forked the same way, so an HSTS upgrade
    // the surface learned was unknown to the engine.
    //
    // Concurrency is not a new question here: `base_data_directory` was ALREADY
    // shared, so the daemon's network process and the GUI's have always
    // co-written this profile's cookies, localstorage and IndexedDB. WebKit's
    // NetworkCache is written as per-record files published by rename and
    // checksum-verified on read, so the worst a racing writer costs is a miss.
    let manager = WebsiteDataManager::builder()
        .base_data_directory(dir.to_string_lossy().as_ref())
        .base_cache_directory(dir.to_string_lossy().as_ref())
        .build();
    // ⭐ COOKIES DO NOT PERSIST JUST BECAUSE THE JAR HAS A DIRECTORY.
    //
    // A `WebsiteDataManager` with base directories still gets a MEMORY-ONLY
    // cookie store: WebKitGTK persists cookies only when someone calls
    // `set_persistent_storage`. wry does that for the visible surface and the
    // standalone window (`webkitgtk/web_context.rs`), so those wrote
    // `<profile>/cookies`; the engine built its manager by hand and never did,
    // so EVERY engine page started with an empty jar and wrote nothing back.
    //
    // Measured on dev, 2026-07-31: after a full page load under a fresh
    // profile, that profile's directory held `cache/`, `storage/` and
    // `mediakeys/` and NO `cookies` file, while a profile the visible browser
    // had used held one. So the module's claim above — "a page logged in under
    // profile X in the visible surface is logged in here" — was false, and
    // `parity.rs`'s cookie test passed only because both its pages live in one
    // process sharing one in-memory store.
    //
    // The failure this causes is worse than "logged out": a bot-check clearance
    // cookie (`cf_clearance`) can never be kept, so a site that issues one
    // re-challenges on every single navigation and the challenge LOOPS with
    // nothing on screen to explain why.
    //
    // Same path and same format as wry, deliberately — `<profile>/cookies`,
    // Netscape text — because the whole point is that this is not a second jar.
    if let Some(cookies) = manager.cookie_manager() {
        cookies.set_persistent_storage(
            dir.join(COOKIE_JAR_FILE).to_string_lossy().as_ref(),
            CookiePersistentStorage::Text,
        );
    }
    let context = WebContext::with_website_data_manager(&manager);

    // ⛔ THE PASSKEY TRANSPORT FOR ENGINE PAGES. Session surfaces reach the
    // signer through yggterm's `yggterm-appctl://` bridge; the engine's own
    // context (headless ctl pages) has no such bridge, so a ceremony's fetch
    // died at the scheme and every passkey login silently failed. The engine
    // registers the scheme ITSELF, backed by the ctl signer (set at daemon
    // start): the handler reads the POST body, parks on the signer exactly
    // like the sidebar HTTP route, and finishes with the JSON. Blocking the
    // handler is the same shape the session bridge has, bounded by the
    // ceremony timeout; the agent grants in seconds.
    {
        use webkit2gtk::{SecurityManagerExt, URISchemeRequestExt, WebContextExt};
        let security = context.security_manager();
        if let Some(security) = security {
            security.register_uri_scheme_as_secure("yggterm-appctl");
        }
        let context_for_scheme = context.clone();
        context.register_uri_scheme("yggterm-appctl", move |request| {
            crate::daemon::journal(
                "passkey_scheme_fetch",
                json!({ "uri": request.uri().map(|u| u.to_string()).unwrap_or_default() }),
            );
            let Some(signer) = crate::engine::api::ctl_signer() else {
                crate::daemon::journal(
                    "passkey_scheme_fetch",
                    json!({ "error": "no ctl signer" }),
                );
                return;
            };
            let uri = request
                .uri()
                .map(|u| u.to_string())
                .unwrap_or_default();
            let path = uri.trim_start_matches("yggterm-appctl://").to_string();
            let method = request.http_method().map(|m| m.to_string()).unwrap_or_else(|| "GET".into());
            let body_json: serde_json::Value = request.http_body().and_then(|stream| {
                use gio::prelude::*;
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0u8; 16384];
                    let read = stream.read(&mut chunk, gio::Cancellable::NONE);
                    match read {
                        Ok(0) => break,
                        Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                        Err(_) => break,
                    }
                }
                serde_json::from_slice(&bytes).ok()
            }).unwrap_or(serde_json::Value::Null);
            let (status, payload) = match path.as_str() {
                p if p.starts_with("/fido2/get") => signer.handle_get(&body_json),
                p if p.starts_with("/fido2/create") => signer.handle_create(&body_json),
                _ => (404, serde_json::json!({ "error": "unknown fido2 path" })),
            };
            let _ = method;
            let payload_bytes = serde_json::to_vec(&payload).unwrap_or_default();
            let stream = gio::MemoryInputStream::from_bytes(
                &glib::Bytes::from(&payload_bytes),
            );
            request.finish(
                &stream,
                payload_bytes.len() as i64,
                Some("application/json"),
            );
            let _ = status;
        });
    }

    // THE policy — one call, one owner. The engine does not decide whether ad
    // blocking is on, which scripts are enabled, or what the UA is; it asks.
    let policy = crate::webpolicy::policy(profile);
    let content = UserContentManager::new();

    // The message channels FIRST, because "before the page loads" is the whole
    // requirement and nothing below opens a page.
    //
    // The FRAME verb's reply channel leads, and it is not armed by a caller:
    // `/engine/frame` is a shipped route, so its channel has to exist on every
    // identity this engine ever builds rather than on the ones some earlier call
    // happened to arm. It is registered in the engine's own world — a reply path
    // the page can call is not a reply path (`super::frame`).
    let mut channel_detail: Vec<Value> = vec![register_channel(
        &content,
        &MessageChannel {
            name: super::frame::CHANNEL.to_string(),
            world: ScriptWorld::Isolated,
        },
    )];
    if let Ok(channels) = CHANNELS.lock() {
        for channel in channels.iter() {
            channel_detail.push(register_channel(&content, channel));
        }
    }

    let mut scripts_attached = 0;
    let mut script_detail = Vec::new();
    // The frame bridge is the engine's OWN script, so it goes on before the
    // profile's — and it goes on through the same `attach_script` every other
    // script uses, reading its placement out of its own metadata block. A second
    // placement decision here is how a script ends up in a world its channel was
    // not registered in, silently.
    // The passkey shim rides the IDENTITY, not the session /policy path: an
    // engine page (ctl-opened, headless) never fetches /policy, so without this
    // `navigator.credentials` is undefined there and every site's WebAuthn dies
    // silently at the first call — the "passkeys are broken" report, 2026-08-27.
    // The shim is GM-pattern-scoped to the rp_ids the vault holds, so it stays
    // invisible everywhere else (the fingerprinting fix below is untouched).
    // ⚠ The identity is cached per profile: shim scoping is read once per
    // daemon life — a passkey newly added to the vault is picked up at the
    // next daemon restart. Acceptable: enrolment is rarer than login.
    let ctl_shim_scripts = crate::engine::api::ctl_passkey_shim_scripts();
    for script in std::iter::once(super::frame::bridge())
        .chain(policy.userscripts.iter().cloned())
        .chain(ctl_shim_scripts)
    {
        attach_script(&content, &script)?;
        scripts_attached += 1;
        script_detail.push(json!({
            "matches": script.matches.len(),
            "world": script.world.as_str(),
            "all_frames": script.all_frames,
            "bytes": script.body.len(),
        }));
    }

    let filter = match &policy.adblock_rules {
        None => {
            json!({ "attached": false, "reason": "policy says no ad blocking for this profile" })
        }
        Some(rules) => attach_filter(&content, rules)?,
    };

    let identity = ProfileIdentity {
        context,
        data: manager,
        content,
        user_agent: policy.user_agent.clone(),
        applied: json!({
            "profile": profile,
            "jar": dir.display().to_string(),
            // The cookie FILE, not just the directory. Reported because "the jar
            // directory exists" was never evidence that cookies survive, and a
            // reader of this journal line deserves the path they can stat.
            "cookie_jar": dir.join(COOKIE_JAR_FILE).display().to_string(),
            "user_agent": policy.user_agent,
            "userscripts": scripts_attached,
            "userscript_detail": script_detail,
            "message_channels": channel_detail,
            "adblock": filter,
            "build_ms": started.elapsed().as_millis(),
        }),
    };
    crate::daemon::journal("engine.identity.built", identity.applied.clone());
    Ok(identity)
}

/// Attach one userscript, honouring the placement its own metadata block
/// declared.
///
/// The WORLD is load-bearing, and getting it backwards is silent.
/// `docs/adblock.md` §5 records the cost: `youtube-adblock` ran in the isolated
/// world, so its `window.fetch` patch was invisible to the page, so the user
/// saw every ad and nothing said so. A script that asked for the main world and
/// quietly got an isolated one is a lie the engine must not tell.
///
/// The two constructors are the opposite way round from what the names suggest,
/// and WebKit says so loudly rather than silently — see the comment on the
/// match below.
fn attach_script(
    content: &UserContentManager,
    script: &crate::userscript::Userscript,
) -> Result<()> {
    let frames = if script.all_frames {
        UserContentInjectedFrames::AllFrames
    } else {
        UserContentInjectedFrames::TopFrame
    };
    // Every surface injects at document-start today; `run_at` travels but is
    // not yet honoured anywhere (see `userscript::Userscript::run_at`), and the
    // engine does not get to be the exception that disagrees with the GUI.
    let when = UserScriptInjectionTime::Start;

    let allow: Vec<&str> = script.matches.iter().map(String::as_str).collect();
    let block: Vec<&str> = script.exclude_matches.iter().map(String::as_str).collect();

    let user_script = match script.world {
        // `webkit_user_script_new` injects into the page's OWN world. It is the
        // plain constructor precisely because that is WebKit's default.
        //
        // I had this backwards and WebKit said so immediately: passing NULL to
        // `..._new_for_world` to mean "the main one" trips
        // `assertion 'worldName' failed`, returns NULL, and every script is
        // refused. `..._for_world` does not take an optional name — a NAME is
        // what makes a world isolated.
        ScriptWorld::Main => UserScript::new(&script.body, frames, when, &allow, &block),
        // A named world: the page shares the DOM but not the globals.
        ScriptWorld::Isolated => unsafe {
            let raw = webkit2gtk::ffi::webkit_user_script_new_for_world(
                script.body.to_glib_none().0,
                frames.into_glib(),
                when.into_glib(),
                ISOLATED_WORLD.to_glib_none().0,
                allow.to_glib_none().0,
                block.to_glib_none().0,
            );
            if raw.is_null() {
                bail!("WebKit refused an isolated-world userscript");
            }
            from_glib_full::<_, UserScript>(raw)
        },
    };
    content.add_script(&user_script);
    Ok(())
}

/// Attach the ad-blocking content filter, **loading before compiling**.
///
/// This is the expensive one and the reason the whole module caches. Measured
/// by the adblock lane on WebKitGTK 2.52.5 with the shipped 146,748-rule set:
/// `..._save` recompiles unconditionally at **15.7 s and 476 MB**, while
/// `..._load` against a populated store returns the same filter in **0.011 s**.
/// So: load first, keyed by a stamp over the ruleset's own bytes, and save only
/// on a miss. A changed ruleset gets a new identifier and therefore a new
/// compile; an unchanged one never recompiles at all.
///
/// `WebKitUserContentFilter` and its store are absent from the gir bindings —
/// `add_filter` is literally commented out as `/*Ignored*/` — so this is FFI,
/// the same route the surface path's adblock already takes.
fn attach_filter(content: &UserContentManager, rules: &str) -> Result<serde_json::Value> {
    let dir = crate::adblock::adblock_dir()?.join("store");
    std::fs::create_dir_all(&dir)?;
    // The identifier IS the content stamp, from the ruleset's one owner. A
    // hand-edited rules.json therefore gets a different identifier and is
    // recompiled, instead of silently serving the previous bytecode.
    let identifier = format!("ychrome-{}", crate::adblock::rules_stamp(rules));

    let started = Instant::now();
    let store = unsafe {
        let raw = webkit2gtk::ffi::webkit_user_content_filter_store_new(
            dir.to_string_lossy().to_glib_none().0,
        );
        if raw.is_null() {
            bail!(
                "could not open the content-filter store at {}",
                dir.display()
            );
        }
        raw
    };

    let (filter, path) = match load_filter(store, &identifier) {
        Some(filter) => (filter, "load"),
        None => {
            let compiled = save_filter(store, &identifier, rules)?;
            (compiled, "compile")
        }
    };
    let elapsed_ms = started.elapsed().as_millis();

    // SAFETY: `filter` is a full reference from the load/save finish call, and
    // `add_filter` takes its own. We release ours below.
    unsafe {
        webkit2gtk::ffi::webkit_user_content_manager_add_filter(content.to_glib_none().0, filter);
        webkit2gtk::ffi::webkit_user_content_filter_unref(filter);
        glib::gobject_ffi::g_object_unref(store as *mut _);
    }

    let detail = json!({
        "attached": true,
        "identifier": identifier,
        "path": path,
        "elapsed_ms": elapsed_ms,
        "rule_bytes": rules.len(),
        "store": dir.display().to_string(),
    });
    crate::daemon::journal("engine.identity.adblock", detail.clone());
    Ok(detail)
}

/// Drive one async filter-store call to completion on this thread's main
/// context.
///
/// The store API is async-only, and the engine thread is already inside the
/// main loop when it builds an identity — so this spins a nested
/// `MainContext::iteration` until the callback lands, rather than blocking the
/// loop it depends on. Without the nesting the callback could never run and the
/// call would deadlock.
fn await_async<T>(mut poll: impl FnMut() -> Option<T>) -> Option<T> {
    let context = glib::MainContext::default();
    let deadline = Instant::now() + std::time::Duration::from_secs(180);
    loop {
        if let Some(value) = poll() {
            return Some(value);
        }
        if Instant::now() > deadline {
            return None;
        }
        context.iteration(true);
    }
}

type FilterPtr = *mut webkit2gtk::ffi::WebKitUserContentFilter;

fn load_filter(
    store: *mut webkit2gtk::ffi::WebKitUserContentFilterStore,
    id: &str,
) -> Option<FilterPtr> {
    let slot: std::rc::Rc<RefCell<Option<Option<FilterPtr>>>> =
        std::rc::Rc::new(RefCell::new(None));
    unsafe extern "C" fn done(
        source: *mut glib::gobject_ffi::GObject,
        result: *mut gio::ffi::GAsyncResult,
        data: glib::ffi::gpointer,
    ) {
        let slot =
            unsafe { std::rc::Rc::from_raw(data as *const RefCell<Option<Option<FilterPtr>>>) };
        let mut error = std::ptr::null_mut();
        let filter = unsafe {
            webkit2gtk::ffi::webkit_user_content_filter_store_load_finish(
                source as *mut _,
                result,
                &mut error,
            )
        };
        if !error.is_null() {
            unsafe { glib::ffi::g_error_free(error) };
            *slot.borrow_mut() = Some(None);
        } else {
            *slot.borrow_mut() = Some(Some(filter));
        }
    }
    unsafe {
        webkit2gtk::ffi::webkit_user_content_filter_store_load(
            store,
            id.to_glib_none().0,
            std::ptr::null_mut(),
            Some(done),
            std::rc::Rc::into_raw(std::rc::Rc::clone(&slot)) as glib::ffi::gpointer,
        );
    }
    await_async(|| slot.borrow_mut().take()).flatten()
}

fn save_filter(
    store: *mut webkit2gtk::ffi::WebKitUserContentFilterStore,
    id: &str,
    rules: &str,
) -> Result<FilterPtr> {
    let slot: std::rc::Rc<RefCell<Option<std::result::Result<FilterPtr, String>>>> =
        std::rc::Rc::new(RefCell::new(None));
    unsafe extern "C" fn done(
        source: *mut glib::gobject_ffi::GObject,
        result: *mut gio::ffi::GAsyncResult,
        data: glib::ffi::gpointer,
    ) {
        type Slot = RefCell<Option<std::result::Result<FilterPtr, String>>>;
        let slot = unsafe { std::rc::Rc::from_raw(data as *const Slot) };
        let mut error = std::ptr::null_mut();
        let filter = unsafe {
            webkit2gtk::ffi::webkit_user_content_filter_store_save_finish(
                source as *mut _,
                result,
                &mut error,
            )
        };
        if !error.is_null() {
            let message: String = unsafe {
                std::ffi::CStr::from_ptr((*error).message)
                    .to_string_lossy()
                    .into_owned()
            };
            unsafe { glib::ffi::g_error_free(error) };
            *slot.borrow_mut() = Some(Err(message));
        } else {
            *slot.borrow_mut() = Some(Ok(filter));
        }
    }
    let bytes = glib::Bytes::from(rules.as_bytes());
    unsafe {
        webkit2gtk::ffi::webkit_user_content_filter_store_save(
            store,
            id.to_glib_none().0,
            bytes.to_glib_none().0,
            std::ptr::null_mut(),
            Some(done),
            std::rc::Rc::into_raw(std::rc::Rc::clone(&slot)) as glib::ffi::gpointer,
        );
    }
    match await_async(|| slot.borrow_mut().take()) {
        Some(Ok(filter)) => Ok(filter),
        // A compile failure is TOTAL — one bad rule means no ad blocking at
        // all, not one missing filter (docs/adblock.md §1). Say so loudly
        // rather than carrying on with an unfiltered view that looks fine.
        Some(Err(message)) => bail!("the content filter did not compile: {message}"),
        None => bail!("the content filter compile did not finish within 180s"),
    }
}

/// Point a profile's network through an ssh SOCKS endpoint.
///
/// Egress is the caller's decision (`--via` on the browser side); the engine
/// only applies it. Same tunnel-reuse rule as the surface path: a context keeps
/// its proxy for its whole life, because churning it mid-session is what breaks
/// a login loop.
pub fn set_egress(identity: &ProfileIdentity, socks: Option<&str>) {
    match socks {
        // The WebsiteDataManager is the owner at 2.52 — the WebContext form is
        // deprecated, and the manager is the object that actually holds the
        // network session for this jar.
        None => identity
            .data
            .set_network_proxy_settings(NetworkProxyMode::Default, None),
        Some(endpoint) => {
            let mut settings = NetworkProxySettings::new(Some(endpoint), &[]);
            identity
                .data
                .set_network_proxy_settings(NetworkProxyMode::Custom, Some(&mut settings));
        }
    }
}

#[cfg(test)]
mod tests {
    // The engine must never grow its own copy of an identity concept. This is
    // a source-level lock because the failure is architectural, not behavioural:
    // a forked jar path or a second UA decision would still WORK, and would
    // silently mean the engine and the visible surface are different browsers —
    // exactly what Phase C exists to prevent.
    #[test]
    fn identity_is_consumed_from_its_owners_never_reimplemented() {
        // The MODULE, not this test module. `include_str!` hands back the whole
        // file, so a scan that forgot to cut its own tests off would match the
        // very strings the assertions below are looking for — which is exactly
        // how this test first failed.
        let source = include_str!("identity.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module body precedes its tests");
        for (owner, what) in [
            ("crate::profile_dir(", "the profile jar"),
            ("crate::webpolicy::policy(", "adblock + userscripts + UA"),
            ("crate::adblock::rules_stamp(", "the ruleset content stamp"),
        ] {
            assert!(
                source.contains(owner),
                "{what} must come from {owner} — the engine may not re-derive it"
            );
        }
        // The jar directory in particular: a literal web-profiles path here
        // would mean the engine and the surface could drift apart.
        assert!(
            !source.contains("web-profiles"),
            "the jar path belongs to crate::profile_dir, not to this module"
        );
    }

    /// ⭐ THE COOKIE JAR MUST BE ASKED TO PERSIST.
    ///
    /// A `WebsiteDataManager` with base directories still stores cookies in
    /// MEMORY ONLY. For three months the engine built one that way and every
    /// page it opened started logged out and threw its cookies away on exit —
    /// invisibly, because the profile directory filled up with cache and
    /// storage and looked alive. A clearance cookie from a bot check
    /// (`cf_clearance`) could never be kept, so a challenged site re-challenged
    /// forever.
    ///
    /// Source-level, like the test above, and for the same reason: the failure
    /// is architectural and SILENT. There is no assertion about a live page
    /// that would have caught it — `parity.rs` set a cookie and read it back in
    /// the same process, and passed.
    #[test]
    fn the_profiles_cookie_jar_is_made_persistent_at_the_wrys_own_path() {
        let source = include_str!("identity.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module body precedes its tests");
        assert!(
            source.contains("set_persistent_storage("),
            "without this call the engine's cookies are memory-only and every \
             page starts logged out"
        );
        assert!(
            source.contains("CookiePersistentStorage::Text"),
            "the format has to match wry's, or the engine and the visible \
             surface keep two jars that cannot read each other"
        );
        // wry writes `<profile>/cookies`. One browser, one file.
        assert_eq!(super::COOKIE_JAR_FILE, "cookies");
    }

    /// ⭐ A MESSAGE CHANNEL MUST BE REGISTERED IN THE WORLD IT ASKED FOR.
    ///
    /// The same failure `attach_script` documents, one layer along and with a
    /// worse consequence. A channel meant to be the engine's private reply path
    /// but registered without a world is one the PAGE can call — the handler
    /// appears on `window.webkit.messageHandlers` in the page's own world, and
    /// every reply the engine sends becomes forgeable by the document it was
    /// asking about. Nothing at runtime says so: the channel still works, the
    /// replies still arrive, and only the security property is gone.
    ///
    /// Source-level for exactly that reason, like the two tests below it.
    #[test]
    fn a_message_channel_is_registered_in_the_world_it_asked_for() {
        let source = include_str!("identity.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module body precedes its tests");
        let arm = source
            .split("fn register_channel(")
            .nth(1)
            .expect("register_channel must exist");
        let body = &arm[..arm.find("\n}").unwrap_or(arm.len())];
        let isolated = body
            .split("ScriptWorld::Isolated =>")
            .nth(1)
            .expect("the isolated arm must exist");
        assert!(
            isolated.contains("register_script_message_handler_in_world")
                && isolated.contains("ISOLATED_WORLD"),
            "an isolated channel must name the engine's own world, or our own \
             isolated userscripts find no handler and the page finds one"
        );
        let main = body
            .split("ScriptWorld::Main =>")
            .nth(1)
            .expect("the main arm must exist");
        let main = &main[..main.find("ScriptWorld::Isolated").unwrap_or(main.len())];
        assert!(
            !main.contains("_in_world"),
            "the main-world arm must use the plain registration — a world NAME \
             is what makes a world isolated"
        );
    }

    /// ⭐ THE REGISTRATION READBACK, NOT THE REQUEST.
    ///
    /// `register_script_message_handler*` returns false for a name already
    /// taken. A channel reported as armed because somebody ASKED for it, while
    /// WebKit refused the name, is silent in precisely the way a channel that
    /// cannot reach the UI process is — and a probe would then report a
    /// substrate limit that was really a duplicate name.
    #[test]
    fn a_channel_reports_what_the_registration_call_answered() {
        let source = include_str!("identity.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module body precedes its tests");
        let arm = source
            .split("fn register_channel(")
            .nth(1)
            .expect("register_channel must exist");
        assert!(
            arm.contains("let registered = match channel.world"),
            "the reported flag must come from the register call itself"
        );
        assert!(
            arm.contains("\"registered\": registered,"),
            "the reported flag must be the call's own answer, not a constant"
        );
        // And a receiver is only wired for a registration that was accepted:
        // connecting `script-message-received::<name>` for a name WebKit
        // refused would be a listener on a signal detail nothing can emit.
        assert!(
            arm.contains("if registered {"),
            "the receiver must hang off the accepted registration"
        );
    }

    /// ⭐ THE HTTP DISK CACHE MUST LAND WHERE THE VISIBLE SURFACE'S DOES.
    ///
    /// The cookie test above has a twin, and it went unnoticed for as long.
    /// `WebsiteDataManager` takes TWO base directories and wry passes the
    /// profile jar for both; the engine passed the jar for data and
    /// `<jar>/cache` for cache. WebKit puts its HTTP disk cache at
    /// `<base_cache_directory>/WebKitCache`, so the two halves of "one browser"
    /// kept two caches and neither could read the other's records — an engine
    /// page refetched what the user's browser had just stored, silently, while
    /// both directories looked healthy.
    ///
    /// Source-level for the cookie test's exact reason: a forked cache is
    /// invisible at runtime. Both managers work, both fill a directory, and the
    /// only symptom is bytes on the wire that did not need to be there.
    #[test]
    fn the_profiles_http_cache_lands_in_the_same_jar_the_surface_uses() {
        // CODE only. The module documents the bug it fixes, naming the old path
        // in prose, and a scan that read the commentary would fail on the very
        // war story that stops the next person reintroducing it. This test
        // caught exactly that on the commit that added it.
        let source: String = include_str!("identity.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module body precedes its tests")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let source = source.as_str();
        let arg_of = |call: &str| -> String {
            let start = source
                .find(call)
                .unwrap_or_else(|| panic!("{call} must be called on the builder"))
                + call.len();
            let rest = &source[start..];
            let end = rest.find(')').expect("the call has to close");
            rest[..end].trim().to_string()
        };
        assert_eq!(
            arg_of(".base_cache_directory("),
            arg_of(".base_data_directory("),
            "both base directories must be the profile jar itself — a cache \
             directory of the engine's own splits the HTTP cache, the HSTS \
             store and CacheStorage away from the visible surface's"
        );
        // The specific regression: a `cache` subdirectory under the jar.
        assert!(
            !source.contains("join(\"cache\")"),
            "`<jar>/cache` is the forked cache directory this test exists to \
             keep out"
        );
    }
}
