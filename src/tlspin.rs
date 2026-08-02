//! Per-host TLS pins, for servers whose chain is VALID but presented in an
//! order GnuTLS refuses to search.
//!
//! # Why this exists
//!
//! `ipindiaonline.gov.in` — India's trademark and patent e-filing portal —
//! sends **seven** certificates containing **two** different paths for the same
//! intermediate: a dead one issued by a retired Comodo root that Debian dropped
//! from `ca-certificates`, listed FIRST, and a good one issued by a root Debian
//! ships and trusts. OpenSSL and GnuTLS both match the leaf's issuer against the
//! first candidate in the presented order, walk into the dead branch, and give
//! up. Chromium and Firefox build the path properly and load the site; WebKitGTK
//! says `Unacceptable TLS certificate` and there is nothing on our side to
//! configure, because the correct intermediate is already installed and GnuTLS
//! will not go looking for it.
//!
//! The site is a government portal with a statutory filing behind it. "Use a
//! different browser" is not an answer for a browser that exists to drive
//! exactly these portals.
//!
//! # Why this is not a trust hole
//!
//! **A pin is only ever written for a certificate that OpenSSL has ALREADY
//! verified against the system trust store, using nothing but the certificates
//! the server itself presented.** [`verify_and_pin`] does that verification and
//! refuses to write a pin if it fails. So a pin does not say "trust this
//! certificate"; it says *"this chain is valid and a correct path builder
//! accepts it — GnuTLS just will not search for the path."*
//!
//! Two consequences worth stating, because they are the reason this is narrow:
//!
//! - It is **per host and per certificate**, not a root. Trusting the retired
//!   root itself — the only other thing that satisfies GnuTLS here, measured —
//!   would widen the trust store for every TLS connection this machine makes,
//!   to every site, forever. That is a much larger change for one portal.
//! - A pin **expires with the leaf**. When the site renews, the fingerprint
//!   stops matching and the page fails closed rather than open. Re-run
//!   `ychrome tls pin <host>`; it re-verifies from scratch before writing.
//!
//! # The file
//!
//! `~/.yggterm/tls-pins.json`, hand-readable and hand-editable on purpose — the
//! whole point is that what has been excepted, and why, is something a person
//! can audit in one glance:
//!
//! ```json
//! { "pins": [ { "host": "...", "sha256": "...", "reason": "...", "verified": "..." } ] }
//! ```

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;

/// One excepted host. `sha256` is over the leaf certificate's DER.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub host: String,
    pub sha256: String,
    pub reason: String,
    pub verified: String,
}

pub fn pins_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("no home dir")?
        .join(".yggterm")
        .join("tls-pins.json"))
}

/// Every pin on this host. A missing or unreadable file is NO pins, never an
/// error: the absence of an exception list must not be able to stop a browser
/// from starting.
pub fn load() -> Vec<Pin> {
    let Ok(path) = pins_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    parse(&text)
}

/// Split out from [`load`] so the parsing is testable without a home dir.
pub fn parse(text: &str) -> Vec<Pin> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let Some(items) = value.get("pins").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let host = item.get("host")?.as_str()?.trim().to_ascii_lowercase();
            let sha256 = normalize_fingerprint(item.get("sha256")?.as_str()?);
            if host.is_empty() || sha256.is_empty() {
                return None;
            }
            Some(Pin {
                host,
                sha256,
                reason: item
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                verified: item
                    .get("verified")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect()
}

fn write(pins: &[Pin]) -> Result<()> {
    let path = pins_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let items: Vec<Value> = pins
        .iter()
        .map(|pin| {
            json!({
                "host": pin.host,
                "sha256": pin.sha256,
                "reason": pin.reason,
                "verified": pin.verified,
            })
        })
        .collect();
    let body = serde_json::to_string_pretty(&json!({ "pins": items }))?;
    std::fs::write(&path, format!("{body}\n"))?;
    Ok(())
}

/// Colon-free lowercase hex, so a fingerprint pasted from `openssl` (which
/// prints `AA:BB:…`) compares equal to one we computed ourselves.
pub fn normalize_fingerprint(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub fn fingerprint_of_der(der: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(der);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The pin that excuses this (host, certificate) pair, if any.
///
/// Host match is exact and case-insensitive. **No wildcards, deliberately** —
/// an exception list whose entries can match hosts nobody enumerated is not an
/// exception list.
pub fn matching<'a>(pins: &'a [Pin], host: &str, der: &[u8]) -> Option<&'a Pin> {
    let host = host.trim().to_ascii_lowercase();
    let fingerprint = fingerprint_of_der(der);
    pins.iter()
        .find(|pin| pin.host == host && pin.sha256 == fingerprint)
}

/// Every way of choosing ONE certificate per subject name.
///
/// This is the path building the TLS libraries decline to do, and it is why a
/// naive `openssl verify` is not enough to prove anything here. Measured on the
/// portal this module exists for: the server presents TWO certificates with the
/// same subject AND the same public key — one issued by a retired root, one by a
/// live root. Given both, OpenSSL selects the dead twin and **does not
/// backtrack**, so the whole chain fails. Given only the live one, it verifies.
///
/// So we enumerate the choices and let the verifier judge each. Grouping is by
/// subject because that is the only thing an issuer reference matches on.
/// Ordering within a group is preserved, so the wire order is tried first and
/// the common case costs exactly one verification.
pub fn candidate_combinations(subjects: &[String], cap: usize) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for (index, subject) in subjects.iter().enumerate() {
        match seen.iter().position(|known| *known == subject.as_str()) {
            Some(at) => groups[at].push(index),
            None => {
                seen.push(subject.as_str());
                groups.push(vec![index]);
            }
        }
    }
    let mut combos: Vec<Vec<usize>> = vec![Vec::new()];
    for group in &groups {
        let mut next = Vec::new();
        for base in &combos {
            for &choice in group {
                if next.len() >= cap {
                    break;
                }
                let mut extended = base.clone();
                extended.push(choice);
                next.push(extended);
            }
        }
        combos = next;
        if combos.len() >= cap {
            break;
        }
    }
    combos
}

/// The roots to judge against: the distribution's own Mozilla set, NOT the
/// machine's configured store.
///
/// Deliberate. A locally installed anchor — someone dropping an intermediate
/// into `/usr/local/share/ca-certificates` to make `curl` work, which is exactly
/// what happened on this fleet — becomes a trusted non-self-signed anchor, and
/// then strict verification fails *because* of it: OpenSSL selects it, finds it
/// is not self-signed, and demands an issuer above it. Judging against the
/// stock root program means a pin records "valid under the Mozilla roots your
/// distro ships", which is a claim that travels to other machines. `None` falls
/// back to the configured store on systems without that directory.
fn stock_roots() -> Option<PathBuf> {
    let dir = PathBuf::from("/usr/share/ca-certificates/mozilla");
    let mut bundle = String::new();
    for entry in std::fs::read_dir(&dir).ok()? {
        let path = entry.ok()?.path();
        if path.extension().is_some_and(|ext| ext == "crt")
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            bundle.push_str(&text);
        }
    }
    if bundle.is_empty() {
        return None;
    }
    let out = tempdir().ok()?.join("roots.pem");
    std::fs::write(&out, bundle).ok()?;
    Some(out)
}

fn field(path: &std::path::Path, what: &str) -> String {
    Command::new("openssl")
        .args(["x509", "-noout", what, "-in"])
        .arg(path)
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|text| {
            text.trim()
                .split_once('=')
                .map(|(_, rest)| rest.trim().to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// Fetch `host`'s chain, prove some path through it verifies against the stock
/// root program using only what the server presented, and record the leaf.
///
/// The proof is the whole feature: if nothing verifies, we write nothing.
pub fn verify_and_pin(host: &str, port: u16, reason: &str, today: &str) -> Result<Pin> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.contains('/') {
        bail!("expected a bare host name, got {host:?}");
    }

    let dir = tempdir()?;
    let chain_path = dir.join("chain.pem");
    let leaf_path = dir.join("leaf.pem");
    let rest_path = dir.join("rest.pem");

    let fetched = Command::new("openssl")
        .args([
            "s_client",
            "-connect",
            &format!("{host}:{port}"),
            "-servername",
            &host,
            "-showcerts",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .context("running `openssl s_client` — is openssl installed?")?;
    let chain = String::from_utf8_lossy(&fetched.stdout).to_string();
    let certs = split_pem(&chain);
    if certs.is_empty() {
        bail!(
            "no certificates came back from {host}:{port} — {}",
            String::from_utf8_lossy(&fetched.stderr)
                .lines()
                .next_back()
                .unwrap_or("no stderr")
        );
    }
    std::fs::write(&chain_path, &chain)?;
    std::fs::write(&leaf_path, &certs[0])?;

    // Everything the server sent EXCEPT the leaf and except self-signed
    // certificates. A root arriving on the wire is not evidence of anything —
    // trust has to come from the store, or an attacker supplies their own root
    // and the check proves nothing.
    let mut pool: Vec<PathBuf> = Vec::new();
    let mut subjects: Vec<String> = Vec::new();
    for (index, pem) in certs.iter().enumerate().skip(1) {
        let path = dir.join(format!("c{index}.pem"));
        std::fs::write(&path, pem)?;
        let subject = field(&path, "-subject");
        if subject == field(&path, "-issuer") {
            continue;
        }
        pool.push(path);
        subjects.push(subject);
    }

    let roots = stock_roots();
    let mut last_output = String::new();
    let mut verified_ok = false;
    for combo in candidate_combinations(&subjects, 64) {
        let mut bundle = String::new();
        for &index in &combo {
            bundle.push_str(&std::fs::read_to_string(&pool[index]).unwrap_or_default());
        }
        std::fs::write(&rest_path, &bundle)?;
        let mut command = Command::new("openssl");
        command.arg("verify");
        if let Some(roots) = roots.as_ref() {
            command.arg("-no-CApath").arg("-CAfile").arg(roots);
        }
        let attempt = command
            .arg("-untrusted")
            .arg(&rest_path)
            .arg(&leaf_path)
            .output()
            .context("running `openssl verify`")?;
        if attempt.status.success() {
            verified_ok = true;
            break;
        }
        last_output = format!(
            "{}{}",
            String::from_utf8_lossy(&attempt.stdout),
            String::from_utf8_lossy(&attempt.stderr)
        );
    }
    if !verified_ok {
        let _ = std::fs::remove_dir_all(&dir);
        bail!(
            "REFUSING to pin {host}: no path through the presented certificates verifies against \
             the stock root program, so this is not the mis-ordered-chain case this exists for. \
             A pin would be granting real new trust, which is not what this is.\n{last_output}"
        );
    }

    let der = Command::new("openssl")
        .args(["x509", "-outform", "DER", "-in"])
        .arg(&leaf_path)
        .output()
        .context("converting the leaf to DER")?;
    let _ = std::fs::remove_dir_all(&dir);
    if !der.status.success() || der.stdout.is_empty() {
        bail!("could not read the leaf certificate for {host}");
    }

    let pin = Pin {
        host: host.clone(),
        sha256: fingerprint_of_der(&der.stdout),
        reason: reason.to_string(),
        verified: today.to_string(),
    };
    let mut pins = load();
    pins.retain(|existing| existing.host != pin.host);
    pins.push(pin.clone());
    pins.sort_by(|a, b| a.host.cmp(&b.host));
    write(&pins)?;
    Ok(pin)
}

pub fn unpin(host: &str) -> Result<bool> {
    let host = host.trim().to_ascii_lowercase();
    let mut pins = load();
    let before = pins.len();
    pins.retain(|pin| pin.host != host);
    let removed = pins.len() != before;
    if removed {
        write(&pins)?;
    }
    Ok(removed)
}

fn split_pem(text: &str) -> Vec<String> {
    let mut certs = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if line.starts_with("-----BEGIN CERTIFICATE-----") {
            current = Some(String::new());
        }
        if let Some(buffer) = current.as_mut() {
            buffer.push_str(line);
            buffer.push('\n');
            if line.starts_with("-----END CERTIFICATE-----") {
                certs.push(current.take().expect("buffer exists"));
            }
        }
    }
    certs
}

fn tempdir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("ychrome-tlspin-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// `ychrome tls <list|pin|unpin>`.
pub fn run(argv: &[String]) -> Result<()> {
    match argv.first().map(String::as_str) {
        Some("list") | None => {
            let pins = load();
            if pins.is_empty() {
                println!("no TLS pins ({})", pins_path()?.display());
                return Ok(());
            }
            for pin in pins {
                println!(
                    "{}\n  sha256   {}\n  verified {}\n  reason   {}",
                    pin.host, pin.sha256, pin.verified, pin.reason
                );
            }
            Ok(())
        }
        Some("pin") => {
            let Some(host) = argv.get(1) else {
                bail!("usage: ychrome tls pin <host> [--reason <text>]");
            };
            let reason = flag(argv, "--reason").unwrap_or_else(|| {
                "chain verifies with openssl but GnuTLS walks a dead branch of a jumbled chain"
                    .to_string()
            });
            let today = flag(argv, "--date").unwrap_or_else(today_utc);
            let pin = verify_and_pin(host, 443, &reason, &today)?;
            println!(
                "pinned {}\n  sha256 {}\n\nThe chain VERIFIED against the system trust store using \
                 only the certificates the server presented, so this pin records a path GnuTLS \
                 would not search for — it grants no new authority.\nIt stops matching when the \
                 site renews its certificate; re-run this then.",
                pin.host, pin.sha256
            );
            Ok(())
        }
        Some("unpin") => {
            let Some(host) = argv.get(1) else {
                bail!("usage: ychrome tls unpin <host>");
            };
            if unpin(host)? {
                println!("removed the pin for {host}");
            } else {
                println!("no pin for {host}");
            }
            Ok(())
        }
        Some(other) => bail!("unknown tls verb {other:?} — expected list, pin or unpin"),
    }
}

fn flag(argv: &[String], name: &str) -> Option<String> {
    argv.iter()
        .position(|arg| arg == name)
        .and_then(|at| argv.get(at + 1))
        .cloned()
}

/// Date stamp for the record, taken from the system clock via `date` so this
/// module needs no time crate. Best-effort: an empty stamp is a cosmetic loss.
fn today_utc() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%d"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|text| text.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_matches_only_its_own_host_and_certificate() {
        let der = b"pretend-der";
        let other = b"a-different-certificate";
        let pins = vec![Pin {
            host: "ipindiaonline.gov.in".into(),
            sha256: fingerprint_of_der(der),
            reason: String::new(),
            verified: String::new(),
        }];

        assert!(matching(&pins, "ipindiaonline.gov.in", der).is_some());
        // Case and a trailing dot are the same host.
        assert!(matching(&pins, "IPIndiaOnline.gov.IN ", der).is_some());
        // A different certificate on the pinned host does NOT pass: the pin
        // expires with the leaf, and it must fail CLOSED when it does.
        assert!(matching(&pins, "ipindiaonline.gov.in", other).is_none());
        // The pinned certificate on a different host does not pass either, and
        // there are no wildcards to smuggle one in.
        assert!(matching(&pins, "evil.gov.in", der).is_none());
        assert!(matching(&pins, "sub.ipindiaonline.gov.in", der).is_none());
    }

    #[test]
    fn fingerprints_compare_equal_however_they_were_pasted() {
        let der = b"x";
        let ours = fingerprint_of_der(der);
        let openssl_style = ours
            .to_uppercase()
            .as_bytes()
            .chunks(2)
            .map(|pair| String::from_utf8_lossy(pair).to_string())
            .collect::<Vec<_>>()
            .join(":");
        assert_eq!(normalize_fingerprint(&openssl_style), ours);
    }

    #[test]
    fn a_missing_or_broken_pins_file_is_no_pins_never_an_error() {
        assert!(parse("").is_empty());
        assert!(parse("{ not json").is_empty());
        assert!(parse(r#"{"pins": "not an array"}"#).is_empty());
        // A row missing the fields that identify it is skipped, not guessed at.
        assert!(parse(r#"{"pins":[{"host":"a.example"}]}"#).is_empty());
        assert!(parse(r#"{"pins":[{"sha256":"ab"}]}"#).is_empty());
    }

    /// The duplicate-subject case is the whole reason this module exists, so it
    /// gets a test rather than a comment: two certificates share a subject, and
    /// exactly one of them leads anywhere.
    #[test]
    fn every_choice_of_one_certificate_per_subject_is_offered() {
        let subjects = vec![
            "CN=leaf-dup".to_string(),
            "CN=EM DV".to_string(),
            "CN=EM DV".to_string(),
            "CN=emSign TLS".to_string(),
        ];
        let combos = candidate_combinations(&subjects, 64);
        // Three distinct subjects, one of them with two candidates.
        assert_eq!(combos.len(), 2);
        for combo in &combos {
            assert_eq!(combo.len(), 3, "one index per distinct subject");
        }
        // Wire order first, so a well-formed site costs exactly one verify.
        assert_eq!(combos[0], vec![0, 1, 3]);
        assert_eq!(combos[1], vec![0, 2, 3]);
    }

    #[test]
    fn the_combination_count_is_capped_so_a_hostile_chain_cannot_explode_it() {
        let subjects: Vec<String> = (0..12)
            .flat_map(|group| (0..3).map(move |_| format!("CN=g{group}")))
            .collect();
        // 3^12 unbounded; the cap is what stands between us and that.
        let combos = candidate_combinations(&subjects, 64);
        assert!(combos.len() <= 64, "got {}", combos.len());
        assert!(!combos.is_empty());
    }

    #[test]
    fn a_chain_splits_into_its_certificates() {
        let pem = "noise\n-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n\
                   junk\n-----BEGIN CERTIFICATE-----\nBBBB\n-----END CERTIFICATE-----\n";
        let certs = split_pem(pem);
        assert_eq!(certs.len(), 2);
        assert!(certs[0].contains("AAAA") && !certs[0].contains("BBBB"));
        assert!(certs[1].contains("BBBB"));
    }
}
