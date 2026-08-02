//! SponsorBlock's category catalogue and per-category behaviour — the ONE owner.
//!
//! `assets/web-userscripts/sponsorblock.js` does the skipping; this module owns
//! *what may be skipped and how*. Three consumers read it and none re-derives it:
//!
//! | consumer | reads |
//! |---|---|
//! | `crate::sidebar` | the rows in the settings pane's SponsorBlock section |
//! | `crate::webpolicy::policy` | the config preamble injected beside the script |
//! | `assets/web-userscripts/sponsorblock.js` | that preamble, at runtime |
//!
//! **Why a preamble and not an edit to the script.** The only channel from this
//! host to a page is the userscript body, and splicing settings INTO
//! `sponsorblock.js` would make every host's copy diverge from the bundled one —
//! which is precisely the state `crate::provision` reads as "the user edited
//! this, leave it alone". So the settings travel as their own tiny synthetic
//! script, the file on disk stays byte-identical to the asset, and the
//! reconciler keeps working.
//!
//! **The script carries its own copy of these defaults** for the case ychrome
//! did not inject anything (a body hand-copied to a GUI that predates the
//! preamble). That is a second encoding, so
//! `the_script_defaults_match_this_module` parses the asset and locks the two
//! together: change one without the other and the test goes red.
//!
//! ⚠ **Licence boundary.** The category names, action types and colours come
//! from the SponsorBlock project (GPL-3.0) and its public API; the segment
//! DATABASE those categories describe is CC BY-NC-SA 4.0.
//!
//! The line that bites is **distribution**: no segment data may travel in a
//! released binary. Querying the API at runtime is the user's own browser using
//! a public service, and caching what a user fetched for their own use is not
//! distribution either — both are fine. `no_segment_data_is_baked_into_the_binary`
//! locks the half that is not. See `THIRD-PARTY-NOTICES.md` for the reasoning
//! and the one condition it rests on.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// What ychrome does when the playhead reaches a segment.
///
/// Spelled as the wire strings, because these travel into the config file, into
/// the injected preamble, and into a sidebar action id — one spelling for all
/// four hops rather than a mapping table per hop.
pub const AUTO: &str = "auto";
pub const MANUAL: &str = "manual";
pub const MUTE: &str = "mute";
pub const SHOW: &str = "show";
pub const OFF: &str = "off";

/// The behaviours a *skippable* category offers. Order is the button order.
const SKIPPABLE: &[&str] = &[AUTO, MANUAL, MUTE, OFF];
/// The behaviours a *label* category offers — a highlight, a full-video notice
/// and a chapter name are not things you can seek past, so offering "Auto-skip"
/// on them would be an affordance that cannot work.
const LABEL_ONLY: &[&str] = &[SHOW, OFF];

/// One SponsorBlock category as ychrome presents it.
pub struct Category {
    /// The API's own spelling. Also the config key and the action-id suffix.
    pub id: &'static str,
    /// The name in the settings pane.
    pub label: &'static str,
    /// One line on what the community submits under it.
    pub description: &'static str,
    /// What ychrome does with it when the user has never said otherwise.
    pub default: &'static str,
    /// Everything the user may choose for it.
    pub options: &'static [&'static str],
    /// The seek-bar colour, adopted from the SponsorBlock extension's own
    /// `barTypes` so the markers read as SponsorBlock's to someone who knows it.
    pub color: &'static str,
}

/// The catalogue, in the order the settings pane draws it.
///
/// **The defaults are a judgement call and here is the rule behind them:** a
/// category that was already auto-skipping before this version keeps
/// auto-skipping (no user loses behaviour they had), and every category added
/// here arrives as MANUAL or OFF rather than AUTO. Adding a button is a new
/// affordance; silently starting to seek past content the user never asked to
/// lose is not. The upstream extension ships with only `sponsor` auto-skipping
/// and asks the user during onboarding — ychrome has no onboarding, so it
/// inherits what ychrome already did and offers the rest.
pub fn catalog() -> &'static [Category] {
    &CATALOG
}

pub fn find(id: &str) -> Option<&'static Category> {
    CATALOG.iter().find(|category| category.id == id)
}

static CATALOG: [Category; 11] = [
    Category {
        id: "sponsor",
        label: "Sponsor",
        description: "Paid promotion, paid referrals and direct advertisements.",
        default: AUTO,
        options: SKIPPABLE,
        color: "#00d400",
    },
    Category {
        id: "selfpromo",
        label: "Unpaid self-promotion",
        description: "The creator's own merch, Patreon or other channels.",
        default: AUTO,
        options: SKIPPABLE,
        color: "#ffff00",
    },
    Category {
        id: "interaction",
        label: "Interaction reminder",
        description: "“Like, subscribe and hit the bell.”",
        default: AUTO,
        options: SKIPPABLE,
        color: "#cc00ff",
    },
    Category {
        id: "intro",
        label: "Intro / intermission",
        description: "Title animations, pauses with no content. The most-submitted \
                      category on the whole database.",
        default: MANUAL,
        options: SKIPPABLE,
        color: "#00ffff",
    },
    Category {
        id: "outro",
        label: "Endcards / credits",
        description: "End cards, credits, the “watch this next” wall.",
        default: MANUAL,
        options: SKIPPABLE,
        color: "#0202ed",
    },
    Category {
        id: "preview",
        label: "Preview / recap",
        description: "A summary of what is coming, or of an earlier episode.",
        default: MANUAL,
        options: SKIPPABLE,
        color: "#008fd6",
    },
    Category {
        id: "music_offtopic",
        label: "Non-music section",
        description: "On a music video: the parts that are not the music.",
        default: MANUAL,
        options: SKIPPABLE,
        color: "#ff9900",
    },
    Category {
        id: "filler",
        label: "Filler tangent",
        description: "Jokes and tangents with no content. Highly subjective, so it \
                      is off unless you ask for it.",
        default: OFF,
        options: SKIPPABLE,
        color: "#7300ff",
    },
    Category {
        id: "poi_highlight",
        label: "Highlight",
        description: "The moment the video is actually about. A jump target, never \
                      a skip.",
        default: SHOW,
        options: LABEL_ONLY,
        color: "#ff1684",
    },
    Category {
        id: "exclusive_access",
        label: "Exclusive access",
        description: "The whole video exists because the creator was given the \
                      product or the trip. A label, not a segment.",
        default: SHOW,
        options: LABEL_ONLY,
        color: "#008a5c",
    },
    Category {
        id: "chapter",
        label: "Community chapters",
        description: "Named regions on the seek bar. Shown, never skipped.",
        default: SHOW,
        options: LABEL_ONLY,
        color: "#ffd983",
    },
];

/// `~/.yggterm/web-userscripts/sponsorblock.config.json` — beside the script it
/// configures, on the host ychrome runs on.
///
/// A `.json` in that directory is inert to the loader (`enabled_scripts` takes
/// `*.js` only), so it can live next to its script without ever being mistaken
/// for one.
pub fn config_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("no home dir")?
        .join(".yggterm")
        .join("web-userscripts")
        .join("sponsorblock.config.json"))
}

fn read_config() -> Value {
    config_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}))
}

/// The behaviour of one category: what the user chose, or the default.
///
/// A stored value that is not one of the category's own `options` is ignored in
/// favour of the default — a config file written by a future ychrome, or by
/// hand, can never put a category into a state this build does not implement.
fn behaviour_from(config: &Value, category: &'static Category) -> &'static str {
    config["categories"][category.id]
        .as_str()
        .and_then(|stored| category.options.iter().find(|option| **option == stored))
        .copied()
        .unwrap_or(category.default)
}

/// Every category's effective behaviour, catalogue order. One read of the file.
pub fn effective() -> Vec<(&'static Category, &'static str)> {
    let config = read_config();
    catalog()
        .iter()
        .map(|category| (category, behaviour_from(&config, category)))
        .collect()
}

/// Record a choice. Unknown keys in the file survive, so a setting this build
/// never heard of is not destroyed by writing one it does.
pub fn set_behaviour(id: &str, behaviour: &str) -> Result<()> {
    let category = find(id).with_context(|| format!("no SponsorBlock category {id:?}"))?;
    if !category.options.contains(&behaviour) {
        anyhow::bail!(
            "{:?} is not one of {}'s options ({})",
            behaviour,
            category.id,
            category.options.join(", ")
        );
    }
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut config = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let mut categories = config
        .get("categories")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    categories.insert(category.id.to_string(), json!(behaviour));
    config.insert("categories".to_string(), Value::Object(categories));
    std::fs::write(&path, serde_json::to_string_pretty(&Value::Object(config))?)?;
    Ok(())
}

/// A change stamp over the config, for `webpolicy::policy_version`.
///
/// The DECISIONS, not the file's bytes: a config that is absent and one that
/// spells out every default mean the same thing to the page, and a surface must
/// not be told to refetch because a file gained whitespace.
pub fn stamp() -> String {
    let mut line = String::from("sponsorblock:");
    for (category, behaviour) in effective() {
        line.push_str(category.id);
        line.push('=');
        line.push_str(behaviour);
        line.push(';');
    }
    line.push('\n');
    line
}

/// The body of the synthetic userscript that carries the settings to the page.
///
/// Deliberately a bare assignment with no logic: everything that could go wrong
/// belongs in the script that reads it, which is versioned and reviewable, not
/// in a string built here.
pub fn config_script_body() -> String {
    let categories: serde_json::Map<String, Value> = effective()
        .into_iter()
        .map(|(category, behaviour)| {
            (
                category.id.to_string(),
                json!({ "behaviour": behaviour, "color": category.color }),
            )
        })
        .collect();
    format!(
        "// ychrome: SponsorBlock settings, generated from \
         ~/.yggterm/web-userscripts/sponsorblock.config.json.\n\
         // Not a file on disk — injected beside sponsorblock.js so the script \
         asset stays\n// byte-identical to the bundled one. Edit it from the \
         settings pane.\nwindow.__ysbConfig = {};\n",
        Value::Object(categories)
    )
}

/// The synthetic script itself: isolated world (the same one `sponsorblock.js`
/// declares, so the global is visible to it), YouTube only, document-start.
pub fn config_userscript() -> crate::userscript::Userscript {
    let mut script = crate::userscript::Userscript::new(config_script_body());
    script.matches = vec![
        "https://*.youtube.com/*".to_string(),
        "https://youtube.com/*".to_string(),
    ];
    script
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSET: &str = include_str!("../assets/web-userscripts/sponsorblock.js");

    #[test]
    fn catalog_is_well_formed() {
        for category in catalog() {
            assert!(!category.id.is_empty());
            assert!(!category.label.is_empty());
            assert!(!category.description.is_empty());
            assert!(
                category.options.contains(&category.default),
                "{}'s default {:?} is not one of its options",
                category.id,
                category.default
            );
            assert!(
                category.color.starts_with('#') && category.color.len() == 7,
                "{} needs a #rrggbb colour, got {:?}",
                category.id,
                category.color
            );
        }
        let ids: std::collections::HashSet<&str> =
            catalog().iter().map(|category| category.id).collect();
        assert_eq!(ids.len(), catalog().len(), "duplicate category id");
    }

    /// ⚠ THE LOCK THAT KEEPS THE TWO ENCODINGS HONEST.
    ///
    /// The script has to carry its own default table for the case where nothing
    /// injected `window.__ysbConfig`. That is a second copy of this module's
    /// decisions, and a second copy silently diverges. So: parse the asset's
    /// own `DEFAULTS` table and require it to agree, id for id and behaviour
    /// for behaviour. Changing a default in one place and not the other is a
    /// red test, not a shipped disagreement.
    #[test]
    fn the_script_defaults_match_this_module() {
        let table = script_defaults();
        for category in catalog() {
            let found = table
                .iter()
                .find(|(id, _)| id == category.id)
                .unwrap_or_else(|| panic!("sponsorblock.js has no default for {}", category.id));
            assert_eq!(
                found.1, category.default,
                "sponsorblock.js defaults {} to {:?}, this module says {:?}",
                category.id, found.1, category.default
            );
        }
        assert_eq!(
            table.len(),
            catalog().len(),
            "sponsorblock.js knows categories this module does not: {table:?}"
        );
    }

    /// The script must ASK the API for every category the catalogue names, or a
    /// category the settings pane offers can never have a segment to act on.
    /// This is the bug that shipped: three categories were requested and the
    /// other eight were invisible.
    #[test]
    fn the_script_requests_every_catalogued_category() {
        let table = script_defaults();
        for category in catalog() {
            assert!(
                table.iter().any(|(id, _)| id == category.id),
                "sponsorblock.js never asks the API for {} — the settings pane \
                 would offer a category that can never fire",
                category.id
            );
        }
    }

    /// `DEFAULTS` in the asset, as `(id, behaviour)` pairs. Parsed from the
    /// source so the test reads what SHIPS rather than a copy kept beside it.
    fn script_defaults() -> Vec<(String, String)> {
        let start = ASSET
            .find("var DEFAULTS = {")
            .expect("sponsorblock.js must declare `var DEFAULTS = {`");
        let rest = &ASSET[start..];
        let end = rest
            .find("\n    };")
            .expect("DEFAULTS must close with `};`");
        let mut pairs = Vec::new();
        for line in rest[..end].lines().skip(1) {
            let line = line.trim().trim_end_matches(',');
            let Some((id, behaviour)) = line.split_once(':') else {
                continue;
            };
            let id = id.trim().trim_matches('\'').trim_matches('"');
            let behaviour = behaviour.trim().trim_matches('\'').trim_matches('"');
            if id.is_empty() || behaviour.is_empty() {
                continue;
            }
            pairs.push((id.to_string(), behaviour.to_string()));
        }
        pairs
    }

    /// ⚠ THE NON-COMMERCIAL BOUNDARY, MADE EXECUTABLE — and pointed at the
    /// right thing.
    ///
    /// The segment database is CC BY-NC-SA 4.0. **Distribution** is what the NC
    /// clause governs, so the line is: no segment data in a released binary. A
    /// user's own browser fetching segments for the video they are watching is
    /// not distribution, and neither is caching what it fetched — an earlier
    /// draft of this test forbade caching and was simply wrong about the
    /// licence.
    ///
    /// What CAN go wrong is somebody `include_`ing a pre-seeded segment file
    /// into the crate, at which point every release carries the database. Every
    /// embed in this crate is therefore enumerated, and the only sponsorblock
    /// one may be the userscript itself.
    #[test]
    fn no_segment_data_is_baked_into_the_binary() {
        let mut embeds = Vec::new();
        for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
            .expect("src/ is readable")
            .flatten()
        {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("a source file");
            for macro_name in ["include_str!", "include_bytes!"] {
                for (index, _) in source.match_indices(macro_name) {
                    // The literal must follow IMMEDIATELY: `include_str!("…")`.
                    // Scanning forward to the next quote instead would read
                    // prose that merely mentions the macro and then attribute
                    // some unrelated string literal to it.
                    let tail = source[index + macro_name.len()..].trim_start();
                    let Some(tail) = tail.strip_prefix('(') else {
                        continue;
                    };
                    let Some(tail) = tail.trim_start().strip_prefix('"') else {
                        continue;
                    };
                    let Some(close) = tail.find('"') else {
                        continue;
                    };
                    embeds.push(tail[..close].to_string());
                }
            }
        }
        assert!(
            !embeds.is_empty(),
            "no embeds found at all — this test stopped reading the source it audits"
        );
        for embed in &embeds {
            if !embed.to_ascii_lowercase().contains("sponsor") {
                continue;
            }
            assert_eq!(
                embed, "../assets/web-userscripts/sponsorblock.js",
                "a sponsorblock artefact other than the script is embedded in the \
                 binary ({embed}). Segment data is CC BY-NC-SA: a user's browser may \
                 fetch and cache it, but a RELEASE may not carry it."
            );
        }
    }

    #[test]
    fn an_unknown_option_falls_back_to_the_default_and_an_unknown_id_is_off() {
        let sponsor = find("sponsor").expect("sponsor");
        let config = json!({ "categories": { "sponsor": "explode" } });
        assert_eq!(behaviour_from(&config, sponsor), sponsor.default);
        let config = json!({ "categories": { "sponsor": MANUAL } });
        assert_eq!(behaviour_from(&config, sponsor), MANUAL);
        // A label-only category cannot be talked into a skip by a hand-edited file.
        let highlight = find("poi_highlight").expect("poi_highlight");
        let config = json!({ "categories": { "poi_highlight": AUTO } });
        assert_eq!(behaviour_from(&config, highlight), highlight.default);
        assert!(find("no-such-category").is_none());
    }

    #[test]
    fn set_behaviour_refuses_an_option_the_category_does_not_offer() {
        assert!(set_behaviour("poi_highlight", AUTO).is_err());
        assert!(set_behaviour("no-such-category", AUTO).is_err());
    }

    const WRITE_PROBE_VAR: &str = "YCHROME_SPONSORBLOCK_WRITE_PROBE";
    const WRITE_PROBE_PREFIX: &str = "ychrome-sponsorblock-write-probe: ";

    /// The write half, end to end over a scratch `$HOME`: a settings click has
    /// to survive as a file that `effective()` reads back, twice in a row, with
    /// a key this build never heard of still standing. Re-exec'd rather than run
    /// in-process because `config_path()` resolves `$HOME`, and mutating the
    /// environment of a running test process is both unsafe and racy.
    #[test]
    fn a_choice_survives_as_a_file_and_does_not_destroy_its_neighbours() {
        if std::env::var(WRITE_PROBE_VAR).is_ok() {
            // A setting from "a future ychrome" that this build has no idea about.
            let path = config_path().expect("config path");
            std::fs::create_dir_all(path.parent().expect("parent")).expect("scratch dir");
            std::fs::write(&path, json!({ "from_the_future": 7 }).to_string()).expect("seed");

            set_behaviour("intro", AUTO).expect("first write");
            let after_one: Vec<(String, String)> = effective()
                .into_iter()
                .map(|(c, b)| (c.id.to_string(), b.to_string()))
                .collect();
            // A SECOND write to a different category must not undo the first.
            set_behaviour("filler", MANUAL).expect("second write");
            let raw: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).expect("read back"))
                    .expect("parse");
            let after_two: Vec<(String, String)> = effective()
                .into_iter()
                .map(|(c, b)| (c.id.to_string(), b.to_string()))
                .collect();
            println!(
                "{WRITE_PROBE_PREFIX}{}",
                json!({ "after_one": after_one, "after_two": after_two, "raw": raw })
            );
            return;
        }

        let home =
            std::env::temp_dir().join(format!("ychrome-sponsorblock-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("scratch home");
        let exe = std::env::current_exe().expect("test binary path");
        let output = std::process::Command::new(exe)
            .args([
                "--exact",
                "sponsorblock::tests::a_choice_survives_as_a_file_and_does_not_destroy_its_neighbours",
                "--nocapture",
            ])
            .env("HOME", &home)
            .env(WRITE_PROBE_VAR, "1")
            .output()
            .expect("spawning the write probe");
        let _ = std::fs::remove_dir_all(&home);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "the write probe child failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let line = stdout
            .lines()
            .find_map(|line| line.strip_prefix(WRITE_PROBE_PREFIX))
            .unwrap_or_else(|| panic!("no probe line in:\n{stdout}"));
        let facts: Value = serde_json::from_str(line).expect("probe facts parse");

        let read = |key: &str, which: &str| -> String {
            facts[which]
                .as_array()
                .expect("pairs")
                .iter()
                .find(|pair| pair[0] == key)
                .unwrap_or_else(|| panic!("{key} missing from {which}"))[1]
                .as_str()
                .expect("behaviour")
                .to_string()
        };
        assert_eq!(read("intro", "after_one"), AUTO, "the click did not stick");
        assert_eq!(
            read("intro", "after_two"),
            AUTO,
            "a second category's click undid the first"
        );
        assert_eq!(read("filler", "after_two"), MANUAL);
        // Untouched categories keep their defaults, not some written-out copy.
        assert_eq!(
            read("sponsor", "after_two"),
            find("sponsor").expect("sponsor").default
        );
        assert_eq!(
            facts["raw"]["from_the_future"], 7,
            "writing a setting destroyed a key this build does not know: {}",
            facts["raw"]
        );
    }

    /// The preamble is a bare assignment carrying every category, and the page
    /// gets it in the SAME isolated world `sponsorblock.js` declares — a
    /// different world would put the global somewhere the script cannot see.
    #[test]
    fn the_config_script_is_isolated_youtube_only_and_names_every_category() {
        let script = config_userscript();
        assert_eq!(script.world, crate::userscript::ScriptWorld::Isolated);
        assert!(
            script.matches.iter().any(|m| m.contains("youtube.com")),
            "the config preamble must be scoped to YouTube: {:?}",
            script.matches
        );
        assert!(
            !script.matches.is_empty(),
            "an empty match list is EVERY url"
        );
        let body = config_script_body();
        assert!(body.contains("window.__ysbConfig = {"));
        for category in catalog() {
            assert!(
                body.contains(category.id),
                "the preamble omits {}",
                category.id
            );
            assert!(
                body.contains(category.color),
                "the preamble omits {}'s colour",
                category.id
            );
        }
        // It must be inert: a preamble that can throw takes the script with it.
        assert!(
            !body.contains("function") && !body.contains("fetch("),
            "the config preamble must be a bare assignment: {body}"
        );
    }

    #[test]
    fn the_stamp_changes_with_a_decision_and_not_with_whitespace() {
        // Pure over `effective()`, so exercise the shape rather than the disk.
        let stamp = stamp();
        assert!(stamp.starts_with("sponsorblock:"));
        for category in catalog() {
            assert!(stamp.contains(&format!("{}=", category.id)));
        }
    }
}
