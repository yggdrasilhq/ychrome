//! The BUNDLED-ASSET RECONCILER: the one component that decides whether the
//! copy of a ychrome-shipped asset on this host is current, and the only one
//! allowed to replace it.
//!
//! ## The failure this exists to make impossible
//!
//! ychrome ships userscripts and an adblock ruleset as `include_str!` assets.
//! Installing one copies the bundled body to `~/.yggterm/web-userscripts/` (or
//! `~/.yggterm/web-adblock/`) on the host ychrome runs on. Nothing then ever
//! looked at that copy again. Two ways that went wrong, both observed:
//!
//! - **A stale script silently degrades into a WORSE product than none.**
//!   `youtube-adblock.js` was deployed to the GUI host before its metadata block
//!   existed. A body with no `// ==UserScript==` block parses to the documented
//!   defaults, and the default world is `isolated` — where the script's
//!   `window.fetch` / `ytInitialPlayerResponse` patches are invisible to the
//!   page. The prune never ran; only the DOM fallback did, which sets
//!   `playbackRate = 16` (WebKit clamps to ~2x). The user's report was exactly
//!   that: *"I still see youtube ads! They are sped up to 2x automatically!"*
//!   The primary path was dead and the fallback was masking it.
//! - **A bundled asset that was never installed at all.** The adblock ruleset
//!   had to be copied to `~/.yggterm/web-adblock/rules.json` BY HAND. It had
//!   been on the GUI host; it had never been on dev, where ad blocking was therefore
//!   silently off with no message anywhere.
//!
//! One rule covers both: **a bundled asset that is absent or superseded on this
//! host is installed, and every decision is reported out loud.**
//!
//! ## Telling "old release of ours" from "the user's edit"
//!
//! A content hash cannot: both are just "different bytes". A DECLARED VERSION
//! can, so every bundled asset carries one — `@version` in a userscript's
//! metadata block ([`crate::userscript::ScriptVersion`]), `ruleset_version` in
//! the adblock ruleset's sidecar. The verdicts:
//!
//! | installed | verdict | what happens |
//! |---|---|---|
//! | absent | [`Verdict::Absent`] | install the bundled body |
//! | version < bundled (incl. unversioned) | [`Verdict::Superseded`] | replace, keeping a `.superseded` backup |
//! | version == bundled, same bytes | [`Verdict::Current`] | nothing, but RECORD the delivery |
//! | version == bundled, other bytes, IS what we last wrote | [`Verdict::Stale`] | replace, keeping a `.superseded` backup |
//! | version == bundled, other bytes | [`Verdict::Forked`] | KEEP it, say so |
//! | version > bundled | [`Verdict::Ahead`] | KEEP it, say so |
//!
//! Unversioned reads as older than every release because every version ychrome
//! has ever *shipped* declares one — so the only bodies without a stamp are the
//! ones that predate the stamp, which is exactly the population that needs
//! healing. `Forked` and `Ahead` are never overwritten: an asset the user
//! edited, or a ruleset they refreshed with `ychrome adblock update`, is theirs.
//!
//! ## ⛔ WHY A DECLARED VERSION WAS NOT ENOUGH: the delivery ledger
//!
//! The table above assumes **a version identifies a body**. Twice it has not:
//!
//! - the generated assets stamp `@version` from the wall clock at generation
//!   time, so a **same-day regeneration** ships different bytes under an
//!   identical stamp;
//! - a hand-edited asset kept its hand-written stamp, so an edit that simply
//!   forgot the bump did the same thing with no regeneration involved.
//!
//! In both cases the reconciler reached its last arm — version equal, bodies
//! differ — and returned `Forked`, which does not write. That is worse than
//! failing to update: `Forked` is the one verdict that reads as *a deliberate
//! user choice*, so a human sees it and leaves it alone. The asset becomes
//! **undeployable forever** and reports as the user's own edit.
//!
//! A content hash alone cannot break the tie — an old release of ours and a
//! user's edit are both just "different bytes". **A hash of what this
//! provisioner ITSELF wrote can**, because it is a record of our own act rather
//! than an inference about the file. So every write records `<id> <sha256>` in
//! a `.delivered` ledger beside the asset, and the ambiguous case splits:
//!
//! - bytes differ **and** match what we last wrote ⇒ ours, superseded ⇒ WRITE;
//! - bytes differ **and** do not ⇒ a genuine user edit ⇒ keep, and say so.
//!
//! It is the same distinction `.deleted` already draws between *never
//! delivered* and *deleted on purpose*, applied to *stale* versus *edited*.
//!
//! ⭐ **[`Verdict::Current`] records an entry too, and that is what arms a host
//! that is already in sync.** A ledger written only on a write would leave every
//! correctly-provisioned host — the common case, and the one the trap springs on
//! next — with nothing recorded until some future release happened to change
//! that asset. `Current` means the installed body is byte-for-byte the bundled
//! body; that is not an inference about the file, it is the same comparison the
//! verdict just made. So it is recorded, no write to the asset, and the host is
//! protected from the next same-version regeneration instead of the one after.
//!
//! ⚠ **A host that is already stuck at `Forked` cannot be rescued by this.**
//! With nothing recorded and the bytes already diverged, `Forked` remains the
//! only honest answer. Unsticking one is still a version bump, or a removal, on
//! that host.
//!
//! A replacement is never destructive. The old body moves to
//! `<name>.superseded` (one generation, overwritten) before the new one lands,
//! so a heal the user disagrees with is one `mv` away from undone.
//!
//! ## The loud half
//!
//! Healing is what a working host needs; being told is what a broken one needs.
//! Every action prints to stderr, and [`Verdict::Forked`] / [`Verdict::Ahead`]
//! travel to the settings pane as a row note. And for the one case where a
//! degraded script could still reach a page — a body whose stem is one of ours
//! but which declares NO metadata block, meaning heal did not run or could not
//! — [`placement_refusal`] refuses it outright rather than injecting it into a
//! world its author never chose. That refusal joins `webpolicy::promote_or_refuse`,
//! the existing gate, rather than opening a second one.

use std::path::{Path, PathBuf};

use crate::userscript::{ScriptVersion, Userscript};

/// Which population an asset belongs to. They differ in exactly one way: a
/// userscript is a file a user may legitimately edit, and a ruleset is
/// generated output whose only user-facing knob is `config.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Userscript,
    AdblockRuleset,
}

/// What the reconciler concluded about one asset on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing installed. The bundled body is written.
    Absent,
    /// Installed at the bundled version, byte-identical. Nothing to do.
    Current,
    /// Installed at an older version, or with no version at all. Replaced,
    /// with the previous body kept as `<name>.superseded`.
    Superseded {
        installed: Option<ScriptVersion>,
        bundled: ScriptVersion,
    },
    /// Same version, different bytes — but the installed body is EXACTLY what
    /// this provisioner last wrote here, so it is an old release of ours that
    /// the bundle has since changed without minting a new version. Replaced,
    /// with the previous body kept as `<name>.superseded`.
    ///
    /// This is the arm that used to fall through to [`Verdict::Forked`] and
    /// strand the asset forever. It can only be reached when the delivery
    /// ledger has an entry, which is why it is a distinct verdict rather than a
    /// smarter `Forked`: "we know this is ours" and "we cannot tell" must not
    /// print the same sentence.
    Stale { version: ScriptVersion },
    /// Same version, different bytes, and NOT what we last wrote (or nothing
    /// recorded): the user edited it. KEPT.
    Forked { version: ScriptVersion },
    /// A version newer than the one we bundle (a `ychrome adblock update`
    /// ruleset, or a script the user upgraded themselves). KEPT.
    Ahead {
        installed: ScriptVersion,
        bundled: ScriptVersion,
    },
}

impl Verdict {
    /// Whether this verdict means the bundled body should be written to disk.
    pub fn needs_write(&self) -> bool {
        matches!(
            self,
            Verdict::Absent | Verdict::Superseded { .. } | Verdict::Stale { .. }
        )
    }

    /// The one-line note the settings pane shows beside the asset, or `None`
    /// when there is nothing the user needs to know. `Current` and the two
    /// verdicts that just wrote the bundled body are silent in the pane: the
    /// state they describe is the expected one.
    pub fn pane_note(&self) -> Option<String> {
        match self {
            Verdict::Forked { version } => Some(format!(
                "Modified locally (still at bundled v{}) — ychrome will not overwrite it",
                version.to_string_dotted()
            )),
            Verdict::Ahead { installed, bundled } => Some(format!(
                "v{} on this host is newer than the bundled v{} — kept",
                installed.to_string_dotted(),
                bundled.to_string_dotted()
            )),
            _ => None,
        }
    }
}

/// One asset's identity, verdict, and what was done about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetStatus {
    /// `youtube-adblock` for a script, `rules.json` for the ruleset.
    pub id: String,
    pub kind: AssetKind,
    pub path: PathBuf,
    pub verdict: Verdict,
    /// `Some(message)` when a write was attempted and failed. A heal that
    /// could not run must be as loud as one that did, or the host is back to
    /// silently degraded.
    pub error: Option<String>,
}

impl AssetStatus {
    /// The stderr line for this asset, or `None` when there is nothing to say.
    /// This is the whole "loud" contract in one place.
    pub fn report_line(&self) -> Option<String> {
        if let Some(error) = &self.error {
            return Some(format!(
                "ychrome: could NOT update {} ({}): {error}\n\
                 ychrome:   this host is running an out-of-date copy; ad blocking or a \
                 bundled script may be degraded.",
                self.id,
                self.path.display()
            ));
        }
        match &self.verdict {
            Verdict::Absent => Some(format!(
                "ychrome: installed {} (was missing on this host) -> {}",
                self.id,
                self.path.display()
            )),
            Verdict::Superseded {
                installed, bundled, ..
            } => Some(format!(
                "ychrome: updated {} from {} to v{} (previous body kept as {}.superseded)",
                self.id,
                match installed {
                    Some(version) => format!("v{}", version.to_string_dotted()),
                    None => "an unversioned copy".to_string(),
                },
                bundled.to_string_dotted(),
                self.path.display()
            )),
            Verdict::Stale { version } => Some(format!(
                "ychrome: refreshed {} — this host carried the body ychrome last delivered at \
                 v{}, and the bundle has changed under that same version (previous body kept \
                 as {}.superseded)",
                self.id,
                version.to_string_dotted(),
                self.path.display()
            )),
            Verdict::Forked { version } => Some(format!(
                "ychrome: {} on this host is modified from the bundled v{} — left alone",
                self.id,
                version.to_string_dotted()
            )),
            Verdict::Ahead { installed, bundled } => Some(format!(
                "ychrome: {} on this host is v{}, newer than the bundled v{} — left alone",
                self.id,
                installed.to_string_dotted(),
                bundled.to_string_dotted()
            )),
            Verdict::Current => None,
        }
    }
}

/// `~/.yggterm` on the host ychrome runs on. The same home
/// [`crate::webpolicy`] reads, deliberately not the GUI's.
fn yggterm_home() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".yggterm"))
}

fn userscript_dir() -> Option<PathBuf> {
    Some(yggterm_home()?.join("web-userscripts"))
}

fn adblock_dir() -> Option<PathBuf> {
    Some(yggterm_home()?.join("web-adblock"))
}

/// The name of the delivery ledger, in whichever asset directory it guards.
///
/// A dotfile, and deliberately not ending in `.js`: `webpolicy::enabled_scripts`
/// takes every `*.js` in the userscript directory, so a ledger that looked like
/// a script would be injected into pages. Same reasoning, and the same
/// spelling, as the `.deleted` tombstone file beside it.
const DELIVERY_LEDGER: &str = ".delivered";

/// SHA-256 of a body, lowercase hex. The ledger stores this rather than the
/// body: it is a record that we wrote a thing, not a second copy of the thing.
fn body_digest(body: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Parse the ledger's text into `id -> digest`. Split out from the read so the
/// format is a unit test rather than a filesystem fixture.
///
/// One `<id> <digest>` pair per line. A malformed or truncated line is DROPPED,
/// never guessed at: the whole value of this file is that an entry means "we
/// wrote exactly this", so half an entry must mean nothing at all. The cost of
/// dropping one is a single `Forked` where a `Stale` was possible — the same
/// answer this file was invented to improve, which is the safe direction.
fn parse_delivery_ledger(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (id, digest) = line.trim().split_once(char::is_whitespace)?;
            let digest = digest.trim();
            if id.is_empty() || digest.is_empty() {
                return None;
            }
            Some((id.to_string(), digest.to_string()))
        })
        .collect()
}

/// What this provisioner last wrote for `id` in `dir`, if anything.
fn last_delivered(dir: &Path, id: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(DELIVERY_LEDGER)).ok()?;
    parse_delivery_ledger(&text)
        .into_iter()
        .find(|(entry, _)| entry == id)
        .map(|(_, digest)| digest)
}

/// Record that we just wrote `body` for `id` into `dir`.
///
/// Best-effort by design, and the caller ignores the result: a ledger that
/// cannot be written costs a future `Stale` its evidence, which degrades to
/// today's behaviour. It must never be able to fail an install that succeeded.
fn record_delivery(dir: &Path, id: &str, body: &str) -> std::io::Result<()> {
    let path = dir.join(DELIVERY_LEDGER);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut entries = parse_delivery_ledger(&existing);
    let digest = body_digest(body);
    match entries.iter_mut().find(|(entry, _)| entry == id) {
        Some(entry) => entry.1 = digest,
        None => entries.push((id.to_string(), digest)),
    }
    entries.sort();
    let mut text: String = entries
        .iter()
        .map(|(entry, digest)| format!("{entry} {digest}\n"))
        .collect();
    if text.is_empty() {
        text.push('\n');
    }
    std::fs::create_dir_all(dir)?;
    std::fs::write(path, text)
}

/// Compare one installed body against one bundled body, given both versions.
/// The pure core of this module: no filesystem, no printing, so every row of
/// the verdict table above is a unit test.
/// `bundled_body` is a CLOSURE because the adblock ruleset's bundled form is
/// 19 MB of gzip that only two of the five verdict rows ever need to look at.
/// Inflating it to answer "is this host at v1.0.0?" would pay for the whole
/// ruleset on every launch of the browser.
///
/// `last_delivered` is the digest this provisioner recorded the last time it
/// wrote this asset here, or `None` when it has never written one (or the
/// ledger predates this mechanism). It is only ever consulted in the one arm
/// the version scheme cannot decide.
pub fn verdict<'a>(
    installed: Option<&str>,
    installed_version: Option<ScriptVersion>,
    bundled_body: impl FnOnce() -> &'a str,
    bundled_version: &ScriptVersion,
    last_delivered: Option<&str>,
) -> Verdict {
    let Some(installed_body) = installed else {
        return Verdict::Absent;
    };
    match &installed_version {
        // Unversioned is older than every release: every body ychrome has
        // shipped declares a version, so an unstamped one predates the stamp.
        None => Verdict::Superseded {
            installed: None,
            bundled: bundled_version.clone(),
        },
        Some(version) if version < bundled_version => Verdict::Superseded {
            installed: Some(version.clone()),
            bundled: bundled_version.clone(),
        },
        Some(version) if version > bundled_version => Verdict::Ahead {
            installed: version.clone(),
            bundled: bundled_version.clone(),
        },
        Some(version) => {
            if installed_body == bundled_body() {
                Verdict::Current
            } else if last_delivered.is_some_and(|digest| digest == body_digest(installed_body)) {
                // Same version, different bytes, and this is byte-for-byte what
                // we put here. The bundle changed without minting a version;
                // the host did nothing. Ours to replace.
                Verdict::Stale {
                    version: version.clone(),
                }
            } else {
                Verdict::Forked {
                    version: version.clone(),
                }
            }
        }
    }
}

/// Write `body` to `path`, moving any existing file aside to
/// `<path>.superseded` first. One backup generation, overwritten each time: a
/// heal must be reversible, and an unbounded pile of backups in a directory the
/// policy loader scans (`enabled_scripts` takes every `*.js`) would be its own
/// bug. `.superseded` deliberately does not end in `.js`, so a backup is never
/// injected.
fn write_with_backup(path: &Path, body: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let backup = path.with_extension(format!(
            "{}.superseded",
            path.extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default()
        ));
        std::fs::rename(path, backup)?;
    }
    std::fs::write(path, body)
}

/// Reconcile ONE asset: read what is installed, judge it, and write the bundled
/// body when the verdict calls for it.
fn reconcile_one(
    id: &str,
    kind: AssetKind,
    path: PathBuf,
    bundled: fn() -> &'static str,
    bundled_version: &ScriptVersion,
    installed_version_of: impl Fn(&str, &Path) -> Option<ScriptVersion>,
) -> AssetStatus {
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let installed = std::fs::read_to_string(&path).ok();
    let installed_version = installed
        .as_deref()
        .and_then(|body| installed_version_of(body, &path));
    let verdict = verdict(
        installed.as_deref(),
        installed_version,
        bundled,
        bundled_version,
        last_delivered(&dir, id).as_deref(),
    );
    let error = if verdict.needs_write() {
        let body = bundled();
        let outcome = write_with_backup(&path, body).and_then(|()| companion(kind, &path));
        if outcome.is_ok() {
            // Best-effort: a ledger that will not write must not fail an
            // install that did.
            let _ = record_delivery(&dir, id, body);
        }
        outcome.err().map(|error| error.to_string())
    } else {
        // An in-sync host is the one the trap springs on NEXT, and it would
        // otherwise carry no record at all. `Current` is proof the body on disk
        // is ours, so record it without touching the asset.
        if verdict == Verdict::Current
            && let Some(body) = installed.as_deref()
        {
            let _ = record_delivery(&dir, id, body);
        }
        None
    };
    AssetStatus {
        id: id.to_string(),
        kind,
        path,
        verdict,
        error,
    }
}

/// Reconcile every bundled asset against this host, writing what is missing or
/// superseded. Returns one status per asset, in catalog order then the ruleset
/// — deterministic, so a caller can diff two runs.
///
/// The DISABLED form of a userscript (`<stem>.js.disabled`) is reconciled too:
/// a user who turned a script off still gets the current body under it, so
/// re-enabling does not resurrect a version from six releases ago.
/// What else has to land beside an asset for it to be complete. The ruleset's
/// provenance sidecar is not a second copy of anything — it is the ruleset's
/// version stamp, and a ruleset written without it reads as unversioned on the
/// next launch and heals forever.
fn companion(kind: AssetKind, path: &Path) -> std::io::Result<()> {
    match kind {
        AssetKind::Userscript => Ok(()),
        AssetKind::AdblockRuleset => {
            let dir = path.parent().unwrap_or(Path::new("."));
            crate::adblock::write_meta(
                dir,
                crate::adblock::bundled_ruleset(),
                crate::adblock::BUNDLED_RULESET_META,
            )
            .map_err(std::io::Error::other)
        }
    }
}

/// Place a FRESHLY GENERATED companion script where it will actually be
/// INJECTED, under the same contract the reconciler uses.
///
/// ⛔⛔ **THE FAILURE THIS EXISTS TO CLOSE.** `ychrome adblock update` writes
/// four files into ONE directory — `rules.json`, its sidecar, and the two
/// companion userscripts — because they are one conversion's output and two
/// owners of "what the filter lists say" is the divergence this repo forbids.
/// But that directory is `web-adblock/`, and [`crate::webpolicy`] injects
/// userscripts from `web-userscripts/` and **only** from there. So every
/// `adblock update` refreshed the network ruleset, reported success, and left
/// the cosmetic filters and the scriptlets exactly as stale as it found them.
///
/// Measured 2026-08-21 on a real host: the freshly generated cosmetic script
/// carried a DOM-published state attribute its generator had gained weeks
/// earlier; the copy actually being injected did not, and was three weeks old.
/// Nothing anywhere said so, because the ruleset — the visible half — really
/// had been updated.
///
/// ⇒ Generating and PLACING are two steps, and only the first had an owner.
///
/// The placement is not a copy. A generated body is newer than the bundle by
/// construction, but the host's copy may be the user's own edit, or a script
/// they deleted on purpose, and both must survive an update they did not ask
/// for. So it runs the same [`verdict`] the reconciler does, against the
/// generated body instead of the bundled one.
///
/// `None` when there is no userscript directory, or the user deleted this
/// script — a deletion that an update resurrects makes the pane's Delete button
/// a lie, which is the rule `.deleted` already exists to enforce.
pub fn place_generated_companion(stem: &str, body: &str) -> Option<AssetStatus> {
    let dir = userscript_dir()?;
    if crate::webpolicy::deleted_userscripts()
        .iter()
        .any(|entry| entry == stem)
    {
        return None;
    }
    // Whichever spelling is on disk is the one that gets updated: a script the
    // user turned OFF still gets the current body under it, so re-enabling does
    // not resurrect a version from six releases ago.
    let disabled = dir.join(format!("{stem}.js.disabled"));
    let path = if disabled.exists() {
        disabled
    } else {
        dir.join(format!("{stem}.js"))
    };
    let installed = std::fs::read_to_string(&path).ok();
    let installed_version = installed
        .as_deref()
        .and_then(|text| crate::userscript::parse(text).version);
    let generated_version = crate::userscript::parse(body)
        .version
        .unwrap_or_else(|| ScriptVersion::parse("0").expect("0 parses"));
    let verdict = verdict(
        installed.as_deref(),
        installed_version,
        || body,
        &generated_version,
        last_delivered(&dir, stem).as_deref(),
    );
    let error = if verdict.needs_write() {
        let outcome = write_with_backup(&path, body);
        if outcome.is_ok() {
            let _ = record_delivery(&dir, stem, body);
        }
        outcome.err().map(|error| error.to_string())
    } else {
        if verdict == Verdict::Current {
            let _ = record_delivery(&dir, stem, body);
        }
        None
    };
    Some(AssetStatus {
        id: stem.to_string(),
        kind: AssetKind::Userscript,
        path,
        verdict,
        error,
    })
}

pub fn reconcile() -> Vec<AssetStatus> {
    let mut statuses = Vec::new();
    if let Some(dir) = userscript_dir() {
        for ext in crate::extensions::catalog() {
            let bundled_version = bundled_script_version(ext.stem, ext.body);
            // Whichever spelling is on disk is the one that gets updated; a
            // fresh install lands enabled.
            let disabled = dir.join(format!("{}.js.disabled", ext.stem));
            let path = if disabled.exists() {
                disabled
            } else {
                dir.join(format!("{}.js", ext.stem))
            };
            // A script the user DELETED must stay deleted: reinstalling it on
            // the next launch would make the pane's Delete button a lie. Only
            // an asset that is present (in either spelling) is reconciled.
            if !path.exists() {
                continue;
            }
            // `bundled` is a plain fn pointer for the ruleset's sake, so the
            // catalog body is captured through a tiny shim.
            let body: &'static str = ext.body;
            let status = {
                let installed = std::fs::read_to_string(&path).ok();
                let installed_version = installed
                    .as_deref()
                    .and_then(|text| crate::userscript::parse(text).version);
                let verdict = verdict(
                    installed.as_deref(),
                    installed_version,
                    || body,
                    &bundled_version,
                    last_delivered(&dir, ext.stem).as_deref(),
                );
                let error = if verdict.needs_write() {
                    let outcome = write_with_backup(&path, body);
                    if outcome.is_ok() {
                        let _ = record_delivery(&dir, ext.stem, body);
                    }
                    outcome.err().map(|e| e.to_string())
                } else {
                    if verdict == Verdict::Current {
                        let _ = record_delivery(&dir, ext.stem, body);
                    }
                    None
                };
                AssetStatus {
                    id: ext.stem.to_string(),
                    kind: AssetKind::Userscript,
                    path,
                    verdict,
                    error,
                }
            };
            statuses.push(status);
        }
    }
    if let Some(dir) = adblock_dir() {
        // THE GENERATED COSMETIC SCRIPT IS THE RULESET'S OTHER HALF, not an
        // extension the user picked: one `abp::convert` produces both, and a
        // host with the ruleset but not the script is missing the cosmetic
        // filters WebKit cannot express. So it is installed on the launches
        // where the ruleset itself is written — a fresh host, or an upgrade —
        // and NOT reinstated on every launch, which is what keeps the pane's
        // Delete button honest for the launches in between.
        let ruleset_landing = !dir.join(crate::adblock::RULESET_FILE).is_file()
            || crate::adblock::installed_ruleset_version(&dir.join(crate::adblock::RULESET_FILE))
                .is_none_or(|installed| installed < crate::adblock::bundled_ruleset_version());
        // ⛔ THE RULESET LANDING IS NOT THE ONLY MOMENT A COMPANION CAN BE
        // MISSING, and treating it as one left hosts browsing without half the
        // filters they were shipped. Measured 2026-08-20 on two hosts of this
        // fleet: `cosmetic-filters.js` present, `scriptlets.js` ABSENT, no
        // error anywhere — the ruleset had landed BEFORE the scriptlet plane
        // existed, so on every launch since, the only branch that could install
        // the companion was false. Nothing reports a script that was never
        // delivered: it looks exactly like one the user does not want.
        //
        // ⇒ A companion missing while the ruleset is present is repaired on any
        // launch, UNLESS the user deleted it — which is now recorded, so the
        // two states are finally distinguishable (`webpolicy::deleted_userscripts`).
        let deleted = crate::webpolicy::deleted_userscripts();
        // BOTH generated scripts, for the same reason: one `abp::convert` makes
        // the ruleset, the cosmetic script and the scriptlet script, and a host
        // with one and not the others is missing filters nothing will say are
        // missing.
        for stem in [
            crate::abp::COSMETIC_SCRIPT_STEM,
            crate::abp::SCRIPTLET_SCRIPT_STEM,
        ] {
            // The ruleset landing still counts (a fresh host installs both with
            // it); so does a companion simply not being here while the ruleset
            // is. `deleted` is what keeps the Delete button honest.
            let ruleset_present = dir.join(crate::adblock::RULESET_FILE).is_file();
            if (ruleset_landing || ruleset_present)
                && !deleted.iter().any(|entry| entry == stem)
                && let Some(scripts) = userscript_dir()
                && let Some(ext) = crate::extensions::find(stem)
            {
                let path = scripts.join(format!("{}.js", ext.stem));
                let disabled = scripts.join(format!("{}.js.disabled", ext.stem));
                if !path.exists() && !disabled.exists() {
                    let outcome = write_with_backup(&path, ext.body);
                    if outcome.is_ok() {
                        let _ = record_delivery(&scripts, ext.stem, ext.body);
                    }
                    let error = outcome.err().map(|e| e.to_string());
                    statuses.push(AssetStatus {
                        id: ext.stem.to_string(),
                        kind: AssetKind::Userscript,
                        path,
                        verdict: Verdict::Absent,
                        error,
                    });
                }
            }
        }
        statuses.push(reconcile_one(
            crate::adblock::RULESET_FILE,
            AssetKind::AdblockRuleset,
            dir.join(crate::adblock::RULESET_FILE),
            crate::adblock::bundled_ruleset,
            &crate::adblock::bundled_ruleset_version(),
            |_, path| crate::adblock::installed_ruleset_version(path),
        ));
    }
    statuses
}

/// [`reconcile`] plus the stderr report. The one call site a caller needs.
pub fn reconcile_and_report() -> Vec<AssetStatus> {
    let statuses = reconcile();
    for status in &statuses {
        if let Some(line) = status.report_line() {
            eprintln!("{line}");
        }
    }
    statuses
}

/// `ychrome provision [--json]`: run the reconcile and say what it found, for
/// a human or for an agent. The same call the browser makes at launch, so what
/// this prints IS what a launch does — there is no second code path that could
/// report one thing and do another.
pub fn run(as_json: bool) -> anyhow::Result<()> {
    let statuses = reconcile_and_report();
    if as_json {
        let rows: Vec<serde_json::Value> = statuses
            .iter()
            .map(|status| {
                serde_json::json!({
                    "id": status.id,
                    "path": status.path.display().to_string(),
                    "kind": match status.kind {
                        AssetKind::Userscript => "userscript",
                        AssetKind::AdblockRuleset => "adblock-ruleset",
                    },
                    "verdict": match &status.verdict {
                        Verdict::Absent => "absent".to_string(),
                        Verdict::Current => "current".to_string(),
                        Verdict::Superseded { installed, bundled } => format!(
                            "superseded:{}->{}",
                            installed
                                .as_ref()
                                .map(ScriptVersion::to_string_dotted)
                                .unwrap_or_else(|| "unversioned".to_string()),
                            bundled.to_string_dotted()
                        ),
                        Verdict::Stale { version } =>
                            format!("stale:{}", version.to_string_dotted()),
                        Verdict::Forked { version } =>
                            format!("forked:{}", version.to_string_dotted()),
                        Verdict::Ahead { installed, bundled } => format!(
                            "ahead:{}>{}",
                            installed.to_string_dotted(),
                            bundled.to_string_dotted()
                        ),
                    },
                    "wrote": status.verdict.needs_write() && status.error.is_none(),
                    "error": status.error,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if statuses.iter().all(|s| s.verdict == Verdict::Current) {
        println!("ychrome: every bundled asset on this host is current.");
    }
    Ok(())
}

/// The version a bundled userscript declares. A bundled body without one is a
/// packaging bug, not a user's problem: the whole mechanism above rests on the
/// bundle being versioned, so it fails the build's own test
/// (`every_bundled_script_declares_a_version`) rather than degrading here.
/// At runtime an unstamped bundle reads as v0, which makes every installed copy
/// `Ahead` and therefore untouched — the inert direction, never a destructive
/// one.
fn bundled_script_version(_stem: &str, body: &str) -> ScriptVersion {
    crate::userscript::parse(body)
        .version
        .unwrap_or_else(|| ScriptVersion::parse("0").expect("0 parses"))
}

/// The pane's view: the note for one installed userscript, judged against the
/// bundle without writing anything. Read off the same [`verdict`] the
/// reconciler uses, so the pane can never disagree with what happened at
/// launch. `None` for a script that is not one of ours, or that is current.
pub fn userscript_note(stem: &str, installed_body: &str) -> Option<String> {
    let ext = crate::extensions::find(stem)?;
    let bundled_version = bundled_script_version(ext.stem, ext.body);
    verdict(
        Some(installed_body),
        crate::userscript::parse(installed_body).version,
        || ext.body,
        &bundled_version,
        userscript_dir()
            .and_then(|dir| last_delivered(&dir, stem))
            .as_deref(),
    )
    .pane_note()
}

/// THE BACKSTOP, and the second reason `webpolicy::promote_or_refuse` refuses a
/// script.
///
/// A body whose stem is one ychrome ships, but which declares NO metadata block
/// at all, is refused rather than injected. Every bundled body has a block, so
/// this can only be a copy that predates the block — which means the reconciler
/// could not heal it (a read-only directory, a permission error) and the
/// placement it would get is the DEFAULTS, not the placement its author chose.
///
/// Injecting it anyway is strictly worse than not injecting it, and that is not
/// a general claim, it is this file's opening paragraph: `youtube-adblock` in
/// the isolated world runs only its DOM fallback, which sets `playbackRate` on
/// the ad instead of removing it. The user sees every ad, sped up, and the
/// thing that was supposed to block them is the thing making them weird.
///
/// Deliberately narrow. A user who edits one of our scripts keeps the header —
/// it is the top six lines of the file — so this cannot fire on a genuine edit;
/// it fires only on a body that declares nothing while carrying our name. A
/// script of the user's OWN with no header keeps the documented defaults, which
/// is a contract `crate::userscript` states and this must not break.
pub fn placement_refusal(path: &Path, script: &Userscript) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".js")?;
    let ext = crate::extensions::find(stem)?;
    if script.body.contains("==UserScript==") {
        return None;
    }
    Some(format!(
        "{stem} is a ychrome-bundled script but the copy on this host declares no \
         metadata block, so it would run with the DEFAULT placement (every URL, \
         isolated world) instead of the one it needs (bundled: {}). It was NOT \
         injected. ychrome could not update it — check that {} is writable.",
        bundled_placement_summary(ext.body),
        path.display()
    ))
}

/// How the bundled body of a script declares itself, for the refusal message.
fn bundled_placement_summary(body: &str) -> String {
    let script = crate::userscript::parse(body);
    let scope = if script.matches.is_empty() {
        "every URL".to_string()
    } else {
        script.matches.join(", ")
    };
    format!("{} world, {scope}", script.world.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> ScriptVersion {
        ScriptVersion::parse(s).expect("parses")
    }

    // The verdict table in this module's doc comment, row by row. This is the
    // decision the whole mechanism rests on, and it is pure, so every row is
    // checkable without touching a disk.
    #[test]
    fn the_verdict_table_holds_row_for_row() {
        assert_eq!(
            verdict(None, None, || "body", &v("1.0.0"), None),
            Verdict::Absent
        );
        assert_eq!(
            verdict(Some("old"), None, || "new", &v("1.0.0"), None),
            Verdict::Superseded {
                installed: None,
                bundled: v("1.0.0")
            },
            "an UNVERSIONED copy is older than every release — this row is the \
             youtube-adblock bug"
        );
        assert_eq!(
            verdict(Some("old"), Some(v("0.9")), || "new", &v("1.0.0"), None),
            Verdict::Superseded {
                installed: Some(v("0.9")),
                bundled: v("1.0.0")
            }
        );
        assert_eq!(
            verdict(Some("same"), Some(v("1.0.0")), || "same", &v("1.0.0"), None),
            Verdict::Current
        );
        assert_eq!(
            verdict(
                Some("edited"),
                Some(v("1.0.0")),
                || "bundled",
                &v("1.0.0"),
                None
            ),
            Verdict::Forked {
                version: v("1.0.0")
            },
            "same version, different bytes, and NOTHING recorded as delivered — we \
             cannot tell an edit from an old release, so it is the user's and kept"
        );
        assert_eq!(
            verdict(
                Some("newer"),
                Some(v("2.0")),
                || "bundled",
                &v("1.0.0"),
                None
            ),
            Verdict::Ahead {
                installed: v("2.0"),
                bundled: v("1.0.0")
            }
        );
    }

    // ⛔ THE ROW THE VERSION SCHEME CANNOT DECIDE, and the whole reason the
    // ledger exists. Identical inputs on both sides of this test except for one
    // thing: whether we have a record of having written that exact body.
    #[test]
    fn a_body_we_delivered_ourselves_is_stale_not_forked() {
        let ours = "the body ychrome shipped last time";
        assert_eq!(
            verdict(
                Some(ours),
                Some(v("1.0.0")),
                || "a NEW bundle at the SAME version",
                &v("1.0.0"),
                Some(&body_digest(ours)),
            ),
            Verdict::Stale {
                version: v("1.0.0")
            },
            "we wrote this body ourselves — the bundle moved under an unchanged \
             version, so it is ours to replace"
        );
        assert_eq!(
            verdict(
                Some("the user rewrote this by hand"),
                Some(v("1.0.0")),
                || "a NEW bundle at the SAME version",
                &v("1.0.0"),
                Some(&body_digest(ours)),
            ),
            Verdict::Forked {
                version: v("1.0.0")
            },
            "a ledger entry that does NOT match is the strongest evidence of a \
             real edit there is — it must still be kept"
        );
        assert_eq!(
            verdict(
                Some(ours),
                Some(v("1.0.0")),
                || ours,
                &v("1.0.0"),
                Some(&body_digest(ours)),
            ),
            Verdict::Current,
            "matching the ledger must never turn an up-to-date host into a write"
        );
    }

    // The ledger is a record of OUR act, so it must survive a round trip
    // exactly, tolerate the file not existing, and drop anything it cannot read
    // in full rather than guess.
    #[test]
    fn the_ledger_round_trips_and_drops_what_it_cannot_read() {
        assert_eq!(
            parse_delivery_ledger("scriptlets abc123\ncosmetic-filters def456\n"),
            vec![
                ("scriptlets".to_string(), "abc123".to_string()),
                ("cosmetic-filters".to_string(), "def456".to_string()),
            ]
        );
        assert_eq!(
            parse_delivery_ledger("scriptlets\n\n   \nrules.json  ff00  \n"),
            vec![("rules.json".to_string(), "ff00".to_string())],
            "a stem with no digest is half an entry, and half an entry must mean \
             nothing — the file's only value is that an entry is exact"
        );
    }

    #[test]
    fn a_delivery_is_recorded_and_read_back() {
        let dir = std::env::temp_dir().join(format!(
            "ychrome-ledger-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        assert_eq!(last_delivered(&dir, "scriptlets"), None, "nothing yet");
        record_delivery(&dir, "scriptlets", "body one").expect("record");
        record_delivery(&dir, "rules.json", "another body").expect("record");
        assert_eq!(
            last_delivered(&dir, "scriptlets").as_deref(),
            Some(body_digest("body one").as_str())
        );
        // A second delivery of the same asset REPLACES the entry — the question
        // is only ever "what did we write LAST", never a history.
        record_delivery(&dir, "scriptlets", "body two").expect("record");
        assert_eq!(
            last_delivered(&dir, "scriptlets").as_deref(),
            Some(body_digest("body two").as_str())
        );
        assert_eq!(
            last_delivered(&dir, "rules.json").as_deref(),
            Some(body_digest("another body").as_str()),
            "recording one asset must not disturb another"
        );
        // An entry recorded for an in-sync host must be the SAME evidence a
        // written one is — otherwise arming a `Current` host would be a
        // different, weaker promise than arming a freshly installed one.
        assert_eq!(
            verdict(
                Some("body two"),
                Some(v("1.0.0")),
                || "the bundle, changed under an unchanged version",
                &v("1.0.0"),
                last_delivered(&dir, "scriptlets").as_deref(),
            ),
            Verdict::Stale {
                version: v("1.0.0")
            }
        );
        // ⛔ It must not look like a userscript: `webpolicy::enabled_scripts`
        // takes every *.js in this directory and would inject it into pages.
        assert!(!DELIVERY_LEDGER.ends_with(".js"));
        assert!(DELIVERY_LEDGER.starts_with('.'));
        std::fs::remove_dir_all(&dir).ok();
    }

    // ⛔ A GENERATED COMPANION IS JUDGED, NOT COPIED. `adblock update` produces
    // a body newer than the bundle by construction, but the host's copy may be
    // the user's own edit — and an update they did not ask for must not eat it.
    #[test]
    fn a_generated_companion_is_judged_against_the_generated_body() {
        let old_release = "// ==UserScript==\n// @version 1.20260731\n// ==/UserScript==\nold";
        let regenerated = "// ==UserScript==\n// @version 1.20260821\n// ==/UserScript==\nnew";
        let v = |body: &str| crate::userscript::parse(body).version;

        // The whole point: a stale host copy is SUPERSEDED by the regenerated
        // body, so `adblock update` finally moves the script it injects.
        assert_eq!(
            verdict(
                Some(old_release),
                v(old_release),
                || regenerated,
                &v(regenerated).unwrap(),
                None
            ),
            Verdict::Superseded {
                installed: v(old_release),
                bundled: v(regenerated).unwrap()
            }
        );

        // A user's copy that is NEWER than what we just generated is still
        // theirs — regeneration is not a licence to downgrade.
        assert_eq!(
            verdict(
                Some(regenerated),
                v(regenerated),
                || old_release,
                &v(old_release).unwrap(),
                None
            ),
            Verdict::Ahead {
                installed: v(regenerated).unwrap(),
                bundled: v(old_release).unwrap()
            }
        );
    }

    // Only the verdicts that mean "this host is behind" write anything. A fork
    // or an ahead copy is the user's, and a write here would destroy it.
    #[test]
    fn only_absent_and_superseded_write() {
        assert!(Verdict::Absent.needs_write());
        assert!(
            Verdict::Superseded {
                installed: None,
                bundled: v("1.0.0")
            }
            .needs_write()
        );
        assert!(
            Verdict::Stale {
                version: v("1.0.0")
            }
            .needs_write(),
            "a body we delivered ourselves is ours to replace — this is the arm \
             whose absence made an asset undeployable forever"
        );
        assert!(!Verdict::Current.needs_write());
        assert!(
            !Verdict::Forked {
                version: v("1.0.0")
            }
            .needs_write(),
            "a heal must never overwrite the user's edit"
        );
        assert!(
            !Verdict::Ahead {
                installed: v("2.0"),
                bundled: v("1.0.0")
            }
            .needs_write(),
            "a heal must never downgrade a host that is ahead"
        );
    }

    // Every verdict except Current says something on stderr. A silent heal is
    // half the bug: the user's copy changed under them and nothing said so.
    #[test]
    fn every_verdict_but_current_reports() {
        let status = |verdict| AssetStatus {
            id: "youtube-adblock".to_string(),
            kind: AssetKind::Userscript,
            path: PathBuf::from("/tmp/youtube-adblock.js"),
            verdict,
            error: None,
        };
        assert!(status(Verdict::Current).report_line().is_none());
        for verdict in [
            Verdict::Absent,
            Verdict::Superseded {
                installed: None,
                bundled: v("1.0.0"),
            },
            Verdict::Stale {
                version: v("1.0.0"),
            },
            Verdict::Forked {
                version: v("1.0.0"),
            },
            Verdict::Ahead {
                installed: v("2.0"),
                bundled: v("1.0.0"),
            },
        ] {
            let line = status(verdict.clone())
                .report_line()
                .unwrap_or_else(|| panic!("{verdict:?} said nothing"));
            assert!(line.contains("youtube-adblock"), "{line}");
        }
        // A failed write is the loudest case of all: the host stays degraded.
        let mut failed = status(Verdict::Absent);
        failed.error = Some("Permission denied".to_string());
        let line = failed.report_line().expect("a failed write must report");
        assert!(line.contains("could NOT update"), "{line}");
        assert!(line.contains("Permission denied"), "{line}");
    }

    // The backup is what makes a heal reversible, and it must NOT land back in
    // the policy loader's `*.js` sweep.
    #[test]
    fn a_replacement_keeps_one_reversible_backup_that_is_not_a_dot_js() {
        let dir = std::env::temp_dir().join(format!("ychrome-provision-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("youtube-adblock.js");
        std::fs::write(&path, "OLD").expect("seed");
        write_with_backup(&path, "NEW").expect("write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "NEW");
        let backup = dir.join("youtube-adblock.js.superseded");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "OLD",
            "the previous body must survive a heal"
        );
        assert!(
            !backup.to_string_lossy().ends_with(".js"),
            "a backup ending in .js would be INJECTED by the policy loader"
        );
        // A second heal overwrites the one backup rather than piling up.
        write_with_backup(&path, "NEWER").expect("write again");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "NEW");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            2,
            "backups must not accumulate"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // THE BUG, as a test. A body whose stem is ours and which declares nothing
    // must be refused; a user's own header-less script must not be; and a
    // bundled script with its block intact must not be.
    #[test]
    fn a_bundled_stem_with_no_metadata_block_is_refused() {
        let headerless = crate::userscript::parse("(function () { var x = 1; })();\n");
        let refusal = placement_refusal(Path::new("/h/youtube-adblock.js"), &headerless)
            .expect("a bundled stem with no metadata block must refuse");
        assert!(refusal.contains("youtube-adblock"), "{refusal}");
        assert!(
            refusal.contains("main world"),
            "the refusal must name the placement it needed: {refusal}"
        );
        assert!(refusal.contains("NOT injected"), "{refusal}");

        assert!(
            placement_refusal(Path::new("/h/my-own-script.js"), &headerless).is_none(),
            "a script of the USER'S with no header keeps the documented defaults"
        );

        let bundled = crate::userscript::parse(
            crate::extensions::find("youtube-adblock")
                .expect("in catalog")
                .body,
        );
        assert!(
            placement_refusal(Path::new("/h/youtube-adblock.js"), &bundled).is_none(),
            "the bundled body itself must never be refused"
        );
    }

    // The pane's note and the reconciler's write decision must come from ONE
    // verdict, or the pane can say "modified locally" about a file that was
    // just overwritten.
    #[test]
    fn the_pane_note_is_the_same_verdict_the_reconciler_acts_on() {
        let ext = crate::extensions::find("idcac").expect("idcac in catalog");
        assert!(
            userscript_note("idcac", ext.body).is_none(),
            "the bundled body is Current and needs no note"
        );
        let edited = format!("{}\n// my edit\n", ext.body);
        let note = userscript_note("idcac", &edited).expect("an edited copy gets a note");
        assert!(note.contains("Modified locally"), "{note}");
        assert!(
            userscript_note("not-ours", "whatever").is_none(),
            "a script ychrome does not ship has no bundled version to compare to"
        );
    }
}
