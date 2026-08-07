//! The ychrome **agent engine** — headless browsing at fleet scale
//! (`docs/agent-engine.md`).
//!
//! Per the 2026-07-18 amendment the engine has no socket, token or lifecycle
//! of its own: it is a subsystem of the per-host ychrome daemon, it mounts
//! under `/engine/*` on `~/.yggterm/ychrome/daemon.sock`, and it shares that
//! daemon's journal so governor actions and routing verbs interleave in
//! reading order. Phase A therefore adds no transport at all — only the
//! substrate, the page verbs, and the gate that proves them.
//!
//! Module map:
//!
//! - `substrate` — the one owner of "what draws a page", plus the live probe
//!   that decides which substrate this host can actually run.
//! - `host` — the engine thread and the page verbs (`open`/`goto`/`eval`/
//!   `shot`/`click_trusted`).
//! - `gate` — Phase A's five proofs, committed and re-runnable.

pub mod api;
pub mod ctl;
pub mod embedsdk;
pub mod flow;
pub mod frames;
pub mod gate;
pub mod gateway;
pub mod hit;
pub mod host;
pub mod identity;
pub mod js;
pub mod parity;
pub mod pool;
pub mod substrate;

use std::io::Write;

use anyhow::{Result, bail};
use serde_json::json;

/// End the process without running libc's exit handlers.
///
/// **Why this is not a shortcut.** WebKit registers an atexit handler that
/// unrefs its process-global objects (the default `WebKitWebContext` and its
/// network session). That unref needs a running main loop for WebKit's async
/// teardown — and by the time atexit handlers run, `main` has returned, the
/// engine's GTK loop has stopped and its headless display is gone, so WebKit
/// aborts. Proven under gdb: `exit` -> `__run_exit_handlers` ->
/// `g_object_unref` -> `abort`, on the main thread, with every gate proof
/// already passed and printed.
///
/// Letting that stand would make the exit code useless — a passing gate and a
/// crashing one would both leave 134 — so the engine owns its own exit
/// instead. That is safe here because the engine keeps nothing in memory that
/// matters: the journal lines and the PNG artifacts are closed files before we
/// get here, and this call flushes the two streams that are not.
fn exit_now(code: i32) -> ! {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: `_exit` is always available and, by construction, this is the
    // last thing this process does.
    unsafe { libc::_exit(code) }
}

/// End the process the engine's way, reporting an error first.
///
/// Every entry point that may have started an engine goes through this, because
/// `_exit` is the only safe way out once WebKit has been initialised (see
/// [`exit_now`]) and because the registry's engine must be torn down first.
pub fn exit_with(outcome: Result<()>) -> ! {
    api::shutdown();
    match outcome {
        Ok(()) => exit_now(0),
        Err(error) => {
            eprintln!("ychrome: {error:#}");
            exit_now(1)
        }
    }
}

/// `engine` verbs, with the engine-owned exit above. `main` calls this;
/// `run_verb` stays a normal `Result` so it is testable.
pub fn run_verb_and_exit(sub: Option<&str>, as_json: bool) -> ! {
    let outcome = run_verb(sub, as_json);
    // Before `_exit`, because `_exit` runs no destructors and the registry's
    // engine lives in a static that Rust would never drop anyway.
    api::shutdown();
    match outcome {
        Ok(()) => exit_now(0),
        Err(error) => {
            eprintln!("ychrome: {error:#}");
            exit_now(1)
        }
    }
}

/// `ychrome engine <verb>` — dispatched from `main` before clap, the same way
/// `status` and `daemon` are, so the browser's url arg shape is untouched.
pub fn run_verb(sub: Option<&str>, as_json: bool) -> Result<()> {
    match sub {
        Some("probe") => {
            let probes = substrate::probe_all();
            if as_json {
                println!(
                    "{}",
                    json!({ "substrates": probes.iter().map(|p| p.to_json()).collect::<Vec<_>>() })
                );
            } else {
                for probe in &probes {
                    let mark = if probe.available {
                        "available"
                    } else {
                        "UNAVAILABLE"
                    };
                    println!("{:<24} {mark}  {}", probe.substrate.id(), probe.reason);
                }
            }
            crate::daemon::journal(
                "engine.substrate.probe",
                json!({ "substrates": probes.iter().map(|p| p.to_json()).collect::<Vec<_>>() }),
            );
            Ok(())
        }
        Some("gate") => {
            let report = gate::run()?;
            if as_json {
                println!("{report}");
            } else {
                println!(
                    "phase-a gate on substrate {} (display {})",
                    report["substrate"].as_str().unwrap_or("?"),
                    report["display"].as_str().unwrap_or("?"),
                );
                for proof in report["proofs"].as_array().into_iter().flatten() {
                    let pass = proof["pass"].as_bool().unwrap_or(false);
                    println!(
                        "  {} proof {} — {}",
                        if pass { "PASS" } else { "FAIL" },
                        proof["proof"],
                        proof["name"].as_str().unwrap_or(""),
                    );
                }
                println!(
                    "  artifacts: {}",
                    report["artifacts"].as_str().unwrap_or("")
                );
            }
            if report["pass"].as_bool() != Some(true) {
                bail!("phase-a gate FAILED — see the journal and the report above");
            }
            Ok(())
        }
        Some("flow") => {
            let report = flow::run()?;
            if as_json {
                println!("{report}");
            } else {
                println!(
                    "phase-b verb flow ({} steps)",
                    report["steps"].as_array().map(Vec::len).unwrap_or(0)
                );
                for step in report["steps"].as_array().into_iter().flatten() {
                    println!(
                        "  {} {:<28} {}",
                        if step["pass"].as_bool() == Some(true) {
                            "PASS"
                        } else {
                            "FAIL"
                        },
                        step["verb"].as_str().unwrap_or(""),
                        step["step"].as_str().unwrap_or(""),
                    );
                }
            }
            if report["pass"].as_bool() != Some(true) {
                bail!("phase-b verb flow FAILED — see the journal and the report above");
            }
            Ok(())
        }
        Some("hit") => {
            let report = hit::run()?;
            if as_json {
                println!("{report}");
            } else {
                println!(
                    "selector-click hittability ({} steps)",
                    report["steps"].as_array().map(Vec::len).unwrap_or(0)
                );
                for step in report["steps"].as_array().into_iter().flatten() {
                    println!(
                        "  {} {}",
                        if step["pass"].as_bool() == Some(true) {
                            "PASS"
                        } else {
                            "FAIL"
                        },
                        step["step"].as_str().unwrap_or(""),
                    );
                }
            }
            if report["pass"].as_bool() != Some(true) {
                bail!("selector-click hittability FAILED — see the journal and the report above");
            }
            Ok(())
        }
        Some("parity") => {
            let report = parity::run()?;
            if as_json {
                println!("{report}");
            } else {
                println!("phase-c identity parity");
                for check in report["checks"].as_array().into_iter().flatten() {
                    println!(
                        "  {} {}",
                        if check["pass"].as_bool() == Some(true) {
                            "PASS"
                        } else {
                            "FAIL"
                        },
                        check["check"].as_str().unwrap_or(""),
                    );
                }
                println!("  sponsorblock: {}", report["sponsorblock"]);
            }
            if report["pass"].as_bool() != Some(true) {
                bail!("phase-c identity parity FAILED");
            }
            Ok(())
        }
        Some("govern") => {
            let pages = std::env::args()
                .nth(3)
                .and_then(|arg| arg.parse::<usize>().ok())
                .unwrap_or(300);
            let report = api::govern(pages)?;
            if as_json {
                println!("{report}");
            } else {
                println!(
                    "phase-d governor: {} logical pages, budget {}",
                    report["requested_pages"], report["budget"]
                );
                for (name, value) in report["checks"].as_object().into_iter().flatten() {
                    println!(
                        "  {} {name}",
                        if value.as_bool() == Some(true) {
                            "PASS"
                        } else {
                            "FAIL"
                        }
                    );
                }
                println!("  measured: {}", report["measured"]);
            }
            if report["ok"].as_bool() != Some(true) {
                bail!("phase-d governor run FAILED");
            }
            Ok(())
        }
        Some("bench") => {
            let pages = std::env::args()
                .nth(3)
                .and_then(|arg| arg.parse::<usize>().ok())
                .unwrap_or(10);
            let report = api::bench(pages)?;
            if as_json {
                println!("{report}");
            } else {
                println!(
                    "engine bench: {}/{} pages opened, {} listed live, {} shots",
                    report["opened"],
                    report["requested_pages"],
                    report["listed_live"],
                    report["shots"],
                );
                println!(
                    "  open wall {}ms (p50 {}ms, p95 {}ms) | shot mean {}ms | smallest PNG {}B",
                    report["open_wall_ms"],
                    report["open_p50_ms"],
                    report["open_p95_ms"],
                    report["shot_mean_ms"],
                    report["png_bytes_min"],
                );
                for failure in report["failures"].as_array().into_iter().flatten() {
                    println!("  FAILURE: {failure}");
                }
            }
            if report["ok"].as_bool() != Some(true) {
                bail!("engine bench FAILED");
            }
            Ok(())
        }
        Some("gateway") => {
            let report = gateway::run()?;
            if as_json {
                println!("{report}");
            } else {
                println!(
                    "gateway hand-off ({} steps)",
                    report["steps"].as_array().map(Vec::len).unwrap_or(0)
                );
                for step in report["steps"].as_array().into_iter().flatten() {
                    println!(
                        "  {} {}",
                        if step["pass"].as_bool() == Some(true) {
                            "PASS"
                        } else {
                            "FAIL"
                        },
                        step["step"].as_str().unwrap_or(""),
                    );
                }
            }
            if report["pass"].as_bool() != Some(true) {
                bail!("gateway hand-off FAILED — see the journal and the report above");
            }
            Ok(())
        }
        Some("embed") => {
            let report = embedsdk::run()?;
            if as_json {
                println!("{report}");
            } else {
                println!(
                    "embedded-SDK iframe mechanisms ({} cases)",
                    report["steps"].as_array().map(Vec::len).unwrap_or(0)
                );
                for step in report["steps"].as_array().into_iter().flatten() {
                    println!(
                        "  {} {}",
                        if step["pass"].as_bool() == Some(true) {
                            "WORKS "
                        } else {
                            "BROKEN"
                        },
                        step["step"].as_str().unwrap_or(""),
                    );
                }
            }
            // Reports, never gates — see the module doc.
            Ok(())
        }
        Some("frames") => {
            let report = frames::run()?;
            if as_json {
                println!("{report}");
            } else {
                println!(
                    "cross-origin frame reach — {} ({} steps)",
                    report["mechanism"].as_str().unwrap_or(""),
                    report["steps"].as_array().map(Vec::len).unwrap_or(0),
                );
                for step in report["steps"].as_array().into_iter().flatten() {
                    println!(
                        "  {} {}",
                        if step["pass"].as_bool() == Some(true) {
                            "PASS"
                        } else {
                            "FAIL"
                        },
                        step["step"].as_str().unwrap_or(""),
                    );
                }
                println!(
                    "  a web-process extension is required: {}",
                    report["web_process_extension_required"],
                );
            }
            if report["pass"].as_bool() != Some(true) {
                bail!("cross-origin frame reach FAILED — see the journal and the report above");
            }
            Ok(())
        }
        Some("worlds") => {
            let report = frames::run_worlds()?;
            if as_json {
                println!("{report}");
            } else {
                println!(
                    "postMessage delivery is world-scoped: {}",
                    report["postmessage_delivery_is_world_scoped"],
                );
                for step in report["steps"].as_array().into_iter().flatten() {
                    println!(
                        "  {} {}",
                        if step["pass"].as_bool() == Some(true) {
                            "PASS"
                        } else {
                            "FAIL"
                        },
                        step["step"].as_str().unwrap_or(""),
                    );
                }
                println!("  verb shape: {}", report["verb_shape"].as_str().unwrap_or(""));
            }
            if report["pass"].as_bool() != Some(true) {
                bail!("the world-delivery INSTRUMENT failed — the answer above is not measured");
            }
            Ok(())
        }
        Some(other) => {
            bail!(
                "unknown engine verb {other:?} (known: probe, gate, flow, hit, \
                 gateway, embed, frames, worlds, bench, govern, parity)"
            )
        }
        None => {
            bail!(
                "usage: ychrome engine \
                 <probe|gate|flow|hit|gateway|embed|frames|worlds|bench|govern|parity> [--json]"
            )
        }
    }
}
