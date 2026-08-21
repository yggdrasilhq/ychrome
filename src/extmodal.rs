//! What each extension lets you configure — the ONE owner, wherever it is drawn.
//!
//! The settings pane used to carry every extension's controls flattened into
//! itself: nineteen buttons in one column, with SponsorBlock's eleven categories
//! and its site list sitting between the ad-blocking toggles and the userscript
//! list. Each extension now gets a row with its own **Options…** button, and the
//! controls live behind it.
//!
//! ## Two placements, one set of widgets
//!
//! yggterm can raise a shell-owned modal carrying widgets an app declared, and
//! it says so on every schema GET (`app_modals`). But a fleet does not upgrade
//! its shell and its apps in one step, so this module answers the same question
//! twice:
//!
//! | the shell says | the pane draws | this module returns |
//! |---|---|---|
//! | `app_modals=true` | a row with **Options…** | the widgets, for the dialog |
//! | anything else | a section, inline, as before | the SAME widgets, inline |
//!
//! ⛔ **The fallback is not a second implementation.** It is the same
//! `Vec<Value>` in a different place, because two implementations of one
//! extension's options is exactly how the two drift until only one of them gets
//! a new setting. The only thing [`Placement`] changes is how an action id is
//! spelled — a click made inside a dialog has to redraw that dialog, and a click
//! made inline must not raise one.
//!
//! ## What is NOT here
//!
//! Installing, deleting and enabling a script are [`crate::webpolicy`]'s, the
//! catalogue is [`crate::extensions`]'s, and SponsorBlock's categories are
//! [`crate::sponsorblock`]'s. This module draws them; it owns none of them, and
//! re-deriving any of it here would put a second answer beside the first.

use serde_json::{Value, json};

/// The reserved stem for AD BLOCKING, which has options but is not a userscript
/// — the ruleset is WebKit's, compiled into the webview.
///
/// ⛔ It shares the namespace with the catalogue's stems, so
/// `the_adblock_stem_cannot_collide_with_a_real_extension` refuses a catalogue
/// entry that ever takes this name: a collision would silently give one of them
/// the other's dialog.
pub const ADBLOCK_STEM: &str = "adblock";

/// Where a set of options is being drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Inside the shell-owned modal. Every action is wrapped so the reply can
    /// redraw the dialog the click was made in.
    Modal,
    /// Inline in the settings pane, on a shell that cannot raise a dialog.
    Inline,
}

/// The envelope a modal's actions travel in: `ext-in:<stem>:<inner action>`.
///
/// The stem is a bare filename stem by construction, so it can carry no `:` and
/// the split is unambiguous. Keeping the INNER action untouched is the point —
/// `sponsorblock:intro:auto` means one thing whether it was clicked in a dialog
/// or in the pane, and there is one dispatch for both.
pub const MODAL_ACTION_PREFIX: &str = "ext-in:";
/// `ext-options:<stem>` — the button that raises a dialog.
pub const OPEN_ACTION_PREFIX: &str = "ext-options:";

impl Placement {
    /// Spell one action id for this placement.
    pub fn action(self, stem: &str, inner: &str) -> String {
        match self {
            Placement::Modal => format!("{MODAL_ACTION_PREFIX}{stem}:{inner}"),
            Placement::Inline => inner.to_string(),
        }
    }
}

/// One extension's options, ready to draw.
pub struct Options {
    /// The dialog's title, or the inline section's heading.
    pub title: String,
    /// One quiet line under it. Inline placements draw it as a muted label.
    pub subtitle: String,
    pub widgets: Vec<Value>,
}

/// Does this stem have an options surface at all?
pub fn has_options(stem: &str) -> bool {
    stem == ADBLOCK_STEM || crate::extensions::find(stem).is_some()
}

/// The friendly name for a stem, or the stem itself for a script the user
/// dropped in themselves.
pub fn display_name(stem: &str) -> String {
    if stem == ADBLOCK_STEM {
        return "Ad blocking".to_string();
    }
    crate::extensions::find(stem)
        .map(|ext| ext.name.to_string())
        .unwrap_or_else(|| stem.to_string())
}

/// Build one extension's options.
///
/// `None` for a stem this build has nothing to say about — which is not an
/// error: a script the user dropped into the directory themselves gets
/// [`generic`] instead, because ychrome knows its file and nothing else.
pub fn options(
    stem: &str,
    profile: &str,
    host: Option<&str>,
    state: &crate::webpolicy::PolicyState,
    placement: Placement,
) -> Option<Options> {
    if stem == ADBLOCK_STEM {
        return Some(adblock_options(profile, state, placement));
    }
    let ext = crate::extensions::find(stem)?;
    let mut widgets = script_header(stem, ext.description, state, placement);
    match stem {
        crate::extensions::SPONSORBLOCK_STEM => {
            widgets.extend(sponsorblock_options(state, placement))
        }
        "youtube-adblock" => widgets.extend(youtube_options()),
        "idcac" => widgets.extend(idcac_options()),
        _ if stem == crate::abp::COSMETIC_SCRIPT_STEM || stem == crate::abp::SCRIPTLET_SCRIPT_STEM => {
            widgets.extend(generated_companion_options(stem))
        }
        _ => {}
    }
    let _ = host;
    widgets.extend(script_footer(stem, state, placement));
    Some(Options {
        title: ext.name.to_string(),
        subtitle: ext.description.to_string(),
        widgets,
    })
}

/// A script ychrome did not ship: its file is all we know about it.
pub fn generic(
    stem: &str,
    state: &crate::webpolicy::PolicyState,
    placement: Placement,
) -> Options {
    let mut widgets = script_header(
        stem,
        "A userscript you added yourself. ychrome runs it and reports what it \
         reads off its metadata block; what it does is its own business.",
        state,
        placement,
    );
    widgets.extend(script_footer(stem, state, placement));
    Options {
        title: stem.to_string(),
        subtitle: format!("~/.yggterm/web-userscripts/{stem}.js on this host"),
        widgets,
    }
}

/// The status of one on-disk script, as the pane sees it.
fn status<'a>(
    stem: &str,
    state: &'a crate::webpolicy::PolicyState,
) -> Option<&'a crate::webpolicy::UserscriptStatus> {
    state.userscripts.iter().find(|script| script.stem == stem)
}

/// Every script's dialog opens the same way: is it installed, is it on, and is
/// anything wrong with this host's copy.
fn script_header(
    stem: &str,
    description: &str,
    state: &crate::webpolicy::PolicyState,
    placement: Placement,
) -> Vec<Value> {
    let mut widgets = vec![json!({"kind": "section", "text": "This extension", "card": true})];
    widgets.push(json!({"kind": "label", "muted": true, "text": description}));
    match status(stem, state) {
        Some(script) => {
            widgets.push(json!({
                "kind": "toggle",
                "id": format!("ext-on-{stem}"),
                "action": placement.action(stem, &format!("userscript:{stem}")),
                "label": "Run this extension",
                "value": script.enabled,
            }));
            // ⛔ A REFUSAL OUTRANKS A NOTE, and both outrank the toggle's own
            // word: a refused script is injected NOWHERE whatever the filename
            // says, so a dialog that showed "Enabled" and stopped would be
            // describing a script that is not running.
            if let Some(refusal) = script.refusal.as_deref() {
                widgets.push(json!({
                    "kind": "label",
                    "text": format!("⛔ Not running: {refusal}"),
                }));
            } else if let Some(note) = script.note.as_deref() {
                widgets.push(json!({"kind": "label", "muted": true, "text": format!("⚠ {note}")}));
            }
        }
        None => {
            widgets.push(json!({
                "kind": "label",
                "muted": true,
                "text": "Not installed on this host.",
            }));
            widgets.push(json!({
                "kind": "button",
                "id": format!("ext-install-{stem}"),
                "action": placement.action(stem, &format!("install:{stem}")),
                "label": "Install",
                "primary": true,
            }));
        }
    }
    widgets
}

/// …and ends the same way: the file, and the two verbs that act on the file.
fn script_footer(
    stem: &str,
    state: &crate::webpolicy::PolicyState,
    placement: Placement,
) -> Vec<Value> {
    if status(stem, state).is_none() {
        return Vec::new();
    }
    let mut widgets = vec![json!({"kind": "section", "text": "This host's copy", "card": true})];
    widgets.push(json!({
        "kind": "label",
        "muted": true,
        "text": format!(
            "~/.yggterm/web-userscripts/{stem}.js, on the host ychrome runs on — which over ssh \
             is not the machine showing this window."
        ),
    }));
    let mut actions = vec![json!({
        "action": placement.action(stem, &format!("userscript-delete:{stem}")),
        "label": "Delete",
        "title": format!("Remove {stem}.js from this host"),
    })];
    // ⛔ Reinstall is offered only for a script ychrome SHIPS. Offering it for
    // one the user wrote would be a button that destroys their file and puts
    // nothing back.
    if crate::extensions::find(stem).is_some() {
        actions.insert(
            0,
            json!({
                "action": placement.action(stem, &format!("ext-reinstall:{stem}")),
                "label": "Reinstall",
                "title": "Replace this host's copy with the one ychrome ships",
            }),
        );
    }
    widgets.push(json!({
        "kind": "list-row",
        "id": format!("ext-file-{stem}"),
        "title": format!("{stem}.js"),
        "subtitle": "Changes apply when the surface reloads.",
        "actions": actions,
    }));
    widgets
}

// ───────────────────────────────────────────────────────────── ad blocking

fn adblock_options(
    profile: &str,
    state: &crate::webpolicy::PolicyState,
    placement: Placement,
) -> Options {
    let mut widgets = vec![json!({"kind": "section", "text": "Blocking", "card": true})];
    if state.adblock_rules_present {
        widgets.push(json!({
            "kind": "toggle",
            "id": "adblock-enabled",
            "action": placement.action(ADBLOCK_STEM, "adblock-enabled"),
            "label": format!("Block ads & trackers ({} rules)", state.adblock_rule_count),
            "value": state.adblock_enabled,
        }));
        widgets.push(json!({
            "kind": "toggle",
            "id": "adblock-profile",
            "action": placement.action(ADBLOCK_STEM, "adblock-profile"),
            "label": format!("Enabled for “{profile}”"),
            "value": !state.adblock_profile_disabled,
        }));
    } else {
        widgets.push(json!({
            "kind": "label",
            "muted": true,
            "text": "No ruleset installed. ychrome installs the bundled one at launch, so this \
                     means the write failed — check that ~/.yggterm/web-adblock/ is writable, or \
                     run `ychrome adblock update` on this host.",
        }));
    }
    // ⭐ THE THREE LAYERS, SAID OUT LOUD. They fail independently and a user
    // reading one number cannot tell which one is down — the ruleset can be
    // perfectly healthy while both companion scripts are missing, which is
    // exactly the state that starved every host of scriptlets for weeks.
    widgets.push(json!({
        "kind": "label",
        "muted": true,
        "text": "Three layers do this. WebKit's content blocker refuses the network requests; \
                 “Cosmetic filters” hides the shapes a network rule cannot reach; “Scriptlets” \
                 runs the per-site fixes the filter lists ask for. Each is switched separately \
                 below, and each can be down while the others work.",
    }));

    let provenance = crate::adblock::provenance();
    widgets.push(json!({"kind": "section", "text": "This ruleset", "card": true}));
    if provenance.installed {
        let installed = provenance
            .installed_version
            .clone()
            .unwrap_or_else(|| "unversioned".to_string());
        widgets.push(json!({
            "kind": "label",
            "text": match provenance.rule_count {
                Some(count) => format!("Version {installed} — {count} rules compiled into the webview."),
                // ⛔ Worth saying loudly: ONE bad rule fails the whole compile,
                // so an unparseable ruleset is not one missing filter, it is no
                // ad blocking at all.
                None => format!("Version {installed} — ⛔ this file does not parse, so NOTHING is blocked."),
            },
        }));
        if provenance.installed_version.as_deref() != Some(provenance.bundled_version.as_str()) {
            widgets.push(json!({
                "kind": "label",
                "muted": true,
                "text": format!(
                    "ychrome ships {}. This host is on its own copy — “Update now” refreshes it \
                     from the lists.",
                    provenance.bundled_version
                ),
            }));
        }
        let layers = [
            ("Network rules", provenance.network_rules),
            ("Network exceptions", provenance.network_exceptions),
            ("Cosmetic selectors", provenance.cosmetic_selectors),
            ("Scriptlet invocations", provenance.scriptlet_rules),
            ("Filters WebKit cannot express", provenance.untranslated),
        ];
        for (label, count) in layers {
            if let Some(count) = count {
                widgets.push(json!({
                    "kind": "list-row",
                    "id": format!("adblock-layer-{}", label.to_ascii_lowercase().replace(' ', "-")),
                    "title": label,
                    "subtitle": count.to_string(),
                }));
            }
        }
        if provenance.network_rules.is_none() {
            widgets.push(json!({
                "kind": "label",
                "muted": true,
                "text": "No conversion report beside this ruleset, so it was copied here by hand \
                         rather than generated. “Update now” rebuilds it with its provenance.",
            }));
        }
    } else {
        widgets.push(json!({"kind": "label", "muted": true, "text": "Nothing installed."}));
    }
    widgets.push(json!({
        "kind": "button",
        "id": "adblock-update",
        "action": placement.action(ADBLOCK_STEM, "adblock-update"),
        "label": "Update now",
        "title": "Fetch every list below and rebuild the ruleset on this host",
    }));
    widgets.push(json!({
        "kind": "label",
        "muted": true,
        "text": "A new RULESET needs a yggterm restart — WebKit compiles the filter once per GUI \
                 process. Everything else here applies when the surface reloads.",
    }));

    widgets.push(json!({"kind": "section", "text": "Where the rules come from"}));
    // The roster is `crate::adblock::LISTS`, the ONE place lists are named:
    // `update` fetches exactly these and the sidecar records exactly these.
    let counted: std::collections::HashMap<&str, u64> = provenance
        .sources
        .iter()
        .map(|(name, lines)| (name.as_str(), *lines))
        .collect();
    for list in crate::adblock::LISTS.iter() {
        let subtitle = match counted.get(list.name) {
            Some(lines) => format!("{} — {lines} filters in this build", list.why),
            None => format!("{} — not in the installed ruleset", list.why),
        };
        widgets.push(json!({
            "kind": "list-row",
            "id": format!("adblock-list-{}", list.name),
            "title": list.name,
            "subtitle": subtitle,
        }));
    }

    // The two GENERATED companions, reachable from the dialog that explains what
    // they are for rather than from a separate list further down the pane.
    widgets.push(json!({"kind": "section", "text": "The two companion scripts", "card": true}));
    for stem in [crate::abp::COSMETIC_SCRIPT_STEM, crate::abp::SCRIPTLET_SCRIPT_STEM] {
        let name = display_name(stem);
        match status(stem, state) {
            Some(script) => {
                let mut subtitle = if script.enabled { "Running" } else { "Off" }.to_string();
                if let Some(refusal) = script.refusal.as_deref() {
                    subtitle = format!("⛔ Not running: {refusal}");
                } else if let Some(note) = script.note.as_deref() {
                    subtitle = format!("⚠ {note}");
                }
                widgets.push(json!({
                    "kind": "list-row",
                    "id": format!("adblock-companion-{stem}"),
                    "title": name,
                    "subtitle": subtitle,
                    "actions": [{
                        "action": placement.action(ADBLOCK_STEM, &format!("userscript:{stem}")),
                        "label": if script.enabled { "Disable" } else { "Enable" },
                    }],
                }));
            }
            None => {
                widgets.push(json!({
                    "kind": "list-row",
                    "id": format!("adblock-companion-{stem}"),
                    "title": name,
                    // ⛔ Absent is a real state and it is not the same as off:
                    // provisioning only refreshes what is already installed, so
                    // a deleted companion stays deleted until it is asked for.
                    "subtitle": "Not installed — this layer is doing nothing on this host.",
                    "actions": [{
                        "action": placement.action(ADBLOCK_STEM, &format!("install:{stem}")),
                        "label": "Install",
                    }],
                }));
            }
        }
    }

    Options {
        title: "Ad blocking".to_string(),
        subtitle: "The ruleset, the two scripts that carry what it cannot express, and where \
                   the filters come from."
            .to_string(),
        widgets,
    }
}

// ───────────────────────────────────────────────────────────── sponsorblock

fn sponsorblock_options(
    state: &crate::webpolicy::PolicyState,
    placement: Placement,
) -> Vec<Value> {
    let stem = crate::extensions::SPONSORBLOCK_STEM;
    // Nothing below has anywhere to act until the script is on this host.
    if status(stem, state).is_none() {
        return Vec::new();
    }
    let prefs = crate::sponsorblock::preferences();
    let mut widgets = vec![json!({"kind": "section", "text": "Categories", "card": true})];
    widgets.push(json!({
        "kind": "label",
        "muted": true,
        "text": "What ychrome does when the playhead reaches each kind of segment. The buttons \
                 on a row are the states it is NOT in.",
    }));
    for (category, behaviour) in crate::sponsorblock::effective() {
        widgets.push(category_row(category, behaviour, placement));
    }

    widgets.push(json!({"kind": "section", "text": "Behaviour", "card": true}));
    widgets.push(json!({
        "kind": "toggle",
        "id": "sb-skip-notice",
        "action": placement.action(stem, &format!("sponsorblock-pref:{}", crate::sponsorblock::PREF_SKIP_NOTICE)),
        "label": "Say when a segment is skipped",
        "value": prefs.skip_notice,
    }));
    // ⛔ The cost of switching it off, where it is switched off: the notice
    // carries the only Undo there is.
    widgets.push(json!({
        "kind": "label",
        "muted": true,
        "text": "The notice carries the Undo. Turn it off and a skip is silent and final — \
                 seeking back into the segment just skips it again.",
    }));
    widgets.push(json!({
        "kind": "toggle",
        "id": "sb-markers",
        "action": placement.action(stem, &format!("sponsorblock-pref:{}", crate::sponsorblock::PREF_SEEK_BAR_MARKERS)),
        "label": "Draw segments on the seek bar",
        "value": prefs.seek_bar_markers,
    }));
    widgets.push(json!({
        "kind": "number-input",
        "id": crate::sponsorblock::PREF_MIN_DURATION,
        "label": "Ignore segments shorter than (seconds)",
        "value": prefs.min_duration_secs,
        "min": 0,
        "max": crate::sponsorblock::MAX_MIN_DURATION_SECS,
    }));
    widgets.push(json!({
        "kind": "button",
        "id": "sb-min-duration-save",
        "action": placement.action(stem, "sponsorblock-min-duration"),
        "label": "Apply the minimum",
    }));

    widgets.push(json!({"kind": "section", "text": "Where it runs", "card": true}));
    widgets.extend(site_widgets(placement));

    // ⛔⛔ THE CONTRIBUTING PLANE. Its own section, at the bottom, with the
    // consequence stated beside the switches rather than in a document.
    widgets.push(json!({"kind": "section", "text": "Contributing", "card": true}));
    widgets.push(json!({
        "kind": "label",
        "text": crate::sponsorblock::Contributing::WARNING,
    }));
    widgets.push(json!({
        "kind": "toggle",
        "id": "sb-voting",
        "action": placement.action(stem, &format!("sponsorblock-pref:{}", crate::sponsorblock::PREF_VOTING)),
        "label": "Let me vote on segments",
        "value": prefs.voting,
    }));
    widgets.push(json!({
        "kind": "toggle",
        "id": "sb-submission",
        "action": placement.action(stem, &format!("sponsorblock-pref:{}", crate::sponsorblock::PREF_SUBMISSION)),
        "label": "Let me submit new segments",
        "value": prefs.submission,
    }));
    if prefs.submission {
        widgets.push(json!({
            "kind": "label",
            "muted": true,
            "text": "Submitting sends the video's id in the clear — it has to, since the point is \
                     to say which video has the segment. Reading segments never does.",
        }));
    }
    // ⛔ The FINGERPRINT, never the id. The id is a write credential — anyone
    // holding it can vote and submit as this user — and a schema is re-fetched
    // on every open and re-sent on every action.
    match crate::sponsorblock::public_user_fingerprint() {
        Some(fingerprint) => widgets.push(json!({
            "kind": "list-row",
            "id": "sb-identity",
            "title": format!("You contribute as {fingerprint}…"),
            "subtitle": "The public fingerprint of this browser's pseudonym. Forgetting it starts \
                         a fresh one and cuts this browser off from what it has already sent.",
            "actions": [{
                "action": placement.action(stem, "sponsorblock-forget-id"),
                "label": "Forget",
                "title": "Start a new pseudonym",
            }],
        })),
        None => widgets.push(json!({
            "kind": "label",
            "muted": true,
            "text": "No pseudonym yet. One is minted the first time you switch either of these on.",
        })),
    }
    widgets
}

/// One category's row: the label, what it does now, and a button per state it is
/// NOT in. Drawn from [`crate::sponsorblock`]'s own catalogue, so a category
/// added there appears here with no change and offers exactly its own options.
fn category_row(
    category: &'static crate::sponsorblock::Category,
    behaviour: &str,
    placement: Placement,
) -> Value {
    let stem = crate::extensions::SPONSORBLOCK_STEM;
    let actions: Vec<Value> = category
        .options
        .iter()
        .filter(|option| **option != behaviour)
        .map(|option| {
            json!({
                "action": placement.action(stem, &format!("sponsorblock:{}:{option}", category.id)),
                "label": behaviour_label(option),
                "title": format!("{}: {}", category.label, behaviour_title(option)),
            })
        })
        .collect();
    json!({
        "kind": "list-row",
        "id": format!("sponsorblock-{}", category.id),
        "title": category.label,
        "subtitle": format!("{} — now: {}", category.description, behaviour_label(behaviour)),
        "actions": actions,
    })
}

fn behaviour_label(behaviour: &str) -> &'static str {
    match behaviour {
        crate::sponsorblock::AUTO => "Auto-skip",
        crate::sponsorblock::MANUAL => "Skip button",
        crate::sponsorblock::MUTE => "Mute",
        crate::sponsorblock::SHOW => "Show",
        _ => "Off",
    }
}

fn behaviour_title(behaviour: &str) -> &'static str {
    match behaviour {
        crate::sponsorblock::AUTO => "seek past it without asking",
        crate::sponsorblock::MANUAL => "offer a skip button while it plays",
        crate::sponsorblock::MUTE => "mute it rather than seek past it",
        crate::sponsorblock::SHOW => "mark it on the seek bar",
        _ => "ignore it entirely",
    }
}

/// The extra hosts SponsorBlock may run on.
///
/// ⚠ Every example in this repository is INVENTED. An instance address is the
/// user's own infrastructure; a shipped one both leaks whoever is on it and rots.
fn site_widgets(placement: Placement) -> Vec<Value> {
    let stem = crate::extensions::SPONSORBLOCK_STEM;
    let mut widgets = vec![json!({
        "kind": "label",
        "muted": true,
        "text": "YouTube always. A front-end that serves YouTube's catalogue serves the same \
                 video ids, so the community database answers for it too — add its host and \
                 SponsorBlock works there.",
    })];
    for host in crate::sponsorblock::sites() {
        widgets.push(json!({
            "kind": "list-row",
            "id": format!("sponsorblock-site-{host}"),
            "title": host,
            "subtitle": "and its sub-domains",
            "actions": [{
                "action": placement.action(stem, &format!("sponsorblock-site-remove:{host}")),
                "label": "Remove",
            }],
        }));
    }
    widgets.push(json!({
        "kind": "text-input",
        "id": "sponsorblock_site",
        "label": "Add a host",
        "placeholder": "videos.example.net",
    }));
    widgets.push(json!({
        "kind": "button",
        "id": "sponsorblock-site-add",
        "action": placement.action(stem, "sponsorblock-site-add"),
        "label": "Add",
    }));
    widgets
}

// ─────────────────────────────────────────────── the rest of the catalogue

fn youtube_options() -> Vec<Value> {
    vec![
        json!({"kind": "section", "text": "What it can and cannot reach", "card": true}),
        json!({
            "kind": "label",
            "muted": true,
            "text": "YouTube's ads are FIRST-PARTY, so no URL filter can reach them. This strips \
                     the ad schedule out of the player's own answer before the player reads it, \
                     by hooking the two funnels every route passes through.",
        }),
        json!({
            "kind": "label",
            "muted": true,
            "text": "⚠ It rots on YouTube's schedule, and that is expected rather than a defect: \
                     the fields it removes are YouTube's to rename. Ads coming back means the \
                     shape moved, not that the extension is off.",
        }),
        json!({
            "kind": "label",
            "muted": true,
            "text": "It cannot touch an ad spliced into the video stream itself — there is no \
                     response to prune in that case, and nothing here pretends otherwise.",
        }),
    ]
}

fn idcac_options() -> Vec<Value> {
    vec![
        json!({"kind": "section", "text": "What it does", "card": true}),
        json!({
            "kind": "label",
            "muted": true,
            "text": "It presses “reject all”, in six languages, and gives the page its scrolling \
                     back. It NEVER accepts anything — hiding a banner leaves it unanswered, and \
                     an unanswered banner is asked again on the next site.",
        }),
        json!({
            "kind": "label",
            "muted": true,
            "text": "Hiding the banners is the RULESET's job, not this script's: the consent \
                     lists in the ad-blocking ruleset name tens of thousands of domains, \
                     maintained by people who do it full time. Turn ad blocking off and most \
                     banners come back even with this on.",
        }),
    ]
}

fn generated_companion_options(stem: &str) -> Vec<Value> {
    let what = if stem == crate::abp::SCRIPTLET_SCRIPT_STEM {
        "the per-site fixes the filter lists ask for with ##+js(...)"
    } else {
        "the ad shapes WebKit's content blocker cannot express (:has-text, :style)"
    };
    vec![
        json!({"kind": "section", "text": "Generated, not written", "card": true}),
        json!({
            "kind": "label",
            "muted": true,
            "text": format!(
                "This script carries {what}. It is GENERATED from the same conversion that builds \
                 the ad-blocking ruleset, so it is refreshed by “Update now” in Ad blocking — \
                 there is nothing to configure here, and editing the file makes it undeployable."
            ),
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_state() -> crate::webpolicy::PolicyState {
        crate::webpolicy::PolicyState {
            adblock_rules_present: true,
            adblock_rule_count: 42,
            adblock_enabled: true,
            adblock_profile_disabled: false,
            userscripts: Vec::new(),
        }
    }

    fn installed(stems: &[&str]) -> crate::webpolicy::PolicyState {
        let mut state = empty_state();
        state.userscripts = stems
            .iter()
            .map(|stem| crate::webpolicy::UserscriptStatus {
                stem: stem.to_string(),
                enabled: true,
                refusal: None,
                note: None,
            })
            .collect();
        state
    }

    fn actions_of(widgets: &[Value]) -> Vec<String> {
        let mut out = Vec::new();
        for widget in widgets {
            if let Some(action) = widget["action"].as_str() {
                out.push(action.to_string());
            }
            for row_action in widget["actions"].as_array().into_iter().flatten() {
                if let Some(action) = row_action["action"].as_str() {
                    out.push(action.to_string());
                }
            }
        }
        out
    }

    /// ⛔ The reserved stem shares a namespace with the catalogue's, and a
    /// collision would silently hand one of them the other's dialog.
    #[test]
    fn the_adblock_stem_cannot_collide_with_a_real_extension() {
        assert!(
            crate::extensions::find(ADBLOCK_STEM).is_none(),
            "a catalogue entry took the reserved {ADBLOCK_STEM:?} stem"
        );
        assert!(has_options(ADBLOCK_STEM));
    }

    /// EVERY catalogue entry opens a dialog. This is the owner's ask stated as a
    /// test: an extension without one is an extension whose button does nothing.
    #[test]
    fn every_catalogue_entry_has_options() {
        let state = installed(
            &crate::extensions::catalog()
                .iter()
                .map(|ext| ext.stem)
                .collect::<Vec<_>>(),
        );
        for ext in crate::extensions::catalog() {
            let options = options(ext.stem, "default", None, &state, Placement::Modal)
                .unwrap_or_else(|| panic!("{} has no options surface", ext.stem));
            assert!(!options.title.is_empty(), "{} has no title", ext.stem);
            assert!(!options.widgets.is_empty(), "{} draws nothing", ext.stem);
            // A dialog with no control at all is a dialog that cannot be used —
            // every one must at least be able to turn its extension off.
            assert!(
                actions_of(&options.widgets)
                    .iter()
                    .any(|action| action.contains(&format!("userscript:{}", ext.stem))),
                "{}'s dialog cannot switch it off",
                ext.stem
            );
        }
    }

    /// ⛔ THE TWO PLACEMENTS MUST DRAW THE SAME CONTROLS. The inline fallback
    /// exists for a shell that cannot raise a dialog; the moment it is a
    /// different set of widgets it is a second implementation, and the two
    /// diverge the first time somebody adds a setting to one of them.
    #[test]
    fn the_inline_fallback_draws_the_same_controls_as_the_dialog() {
        let stems: Vec<&str> = crate::extensions::catalog()
            .iter()
            .map(|ext| ext.stem)
            .collect();
        let state = installed(&stems);
        for stem in stems.iter().copied().chain([ADBLOCK_STEM]) {
            let modal = options(stem, "default", None, &state, Placement::Modal).expect("options");
            let inline = options(stem, "default", None, &state, Placement::Inline).expect("options");
            assert_eq!(
                modal.widgets.len(),
                inline.widgets.len(),
                "{stem} draws a different number of widgets inline"
            );
            let wrapped = actions_of(&modal.widgets);
            let bare = actions_of(&inline.widgets);
            assert_eq!(wrapped.len(), bare.len(), "{stem} offers different actions");
            for (wrapped, bare) in wrapped.iter().zip(bare.iter()) {
                assert_eq!(
                    *wrapped,
                    format!("{MODAL_ACTION_PREFIX}{stem}:{bare}"),
                    "{stem}: only the envelope may differ between placements"
                );
            }
        }
    }

    /// ⛔⛔ The consequence has to be beside the switches. A privacy cost stated
    /// in a document is a privacy cost nobody reads, and these two are the only
    /// controls in ychrome that write to somebody else's database.
    #[test]
    fn the_contributing_switches_state_their_cost_where_they_are_offered() {
        let state = installed(&[crate::extensions::SPONSORBLOCK_STEM]);
        let options = options(
            crate::extensions::SPONSORBLOCK_STEM,
            "default",
            None,
            &state,
            Placement::Modal,
        )
        .expect("sponsorblock options");
        let text: Vec<String> = options
            .widgets
            .iter()
            .filter_map(|widget| widget["text"].as_str().map(ToOwned::to_owned))
            .collect();
        assert!(
            text.iter()
                .any(|line| line == crate::sponsorblock::Contributing::WARNING),
            "the contributing warning is not drawn beside its switches"
        );
        // …and it must sit ABOVE them, not somewhere below where a scrolled
        // dialog would put it out of sight of the thing it warns about.
        let warning = options
            .widgets
            .iter()
            .position(|widget| widget["text"].as_str() == Some(crate::sponsorblock::Contributing::WARNING))
            .expect("the warning is present");
        let switch = options
            .widgets
            .iter()
            .position(|widget| widget["id"].as_str() == Some("sb-voting"))
            .expect("the voting switch is present");
        assert!(warning < switch, "the warning must precede the switches");
    }

    /// ⛔ A write credential must never reach a schema. A schema is re-fetched
    /// on every open and re-sent on every action, and anyone holding this id can
    /// vote and submit as its owner.
    #[test]
    fn the_dialog_shows_a_fingerprint_and_never_the_private_id() {
        let state = installed(&[crate::extensions::SPONSORBLOCK_STEM]);
        let options = options(
            crate::extensions::SPONSORBLOCK_STEM,
            "default",
            None,
            &state,
            Placement::Modal,
        )
        .expect("sponsorblock options");
        let drawn = json!(options.widgets).to_string();
        if let Some(id) = crate::sponsorblock::private_user_id() {
            assert!(
                !drawn.contains(&id),
                "the private SponsorBlock id reached a pane schema"
            );
        }
    }

    /// A script ychrome did not ship gets a dialog too — and NOT a Reinstall
    /// button, which would destroy the user's own file and put nothing back.
    #[test]
    fn a_users_own_script_gets_a_dialog_without_a_reinstall() {
        let state = installed(&["something-of-my-own"]);
        let options = generic("something-of-my-own", &state, Placement::Modal);
        let actions = actions_of(&options.widgets);
        assert!(actions.iter().any(|action| action.contains("userscript-delete:")));
        assert!(
            !actions.iter().any(|action| action.contains("ext-reinstall:")),
            "offered to reinstall a script ychrome does not ship"
        );
    }
}
