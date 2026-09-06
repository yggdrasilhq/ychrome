#!/usr/bin/env bash
# Install (or refresh) the hourly ychrome daemon census/reap timer for this
# user on this host. Run ON the host, as the user ychrome runs as.
#
# Why this exists: ychrome daemons are per-HOME-namespace singletons
# (~/.yggterm/ychrome/daemon.sock), auto-spawned by the first client and
# supervised by its clients — so a daemon whose clients all die, or whose
# binary was replaced on disk, survives as a zombie until a human notices.
# Measured: dev 2026-08-08 (three daemons, two on deleted binaries) and again
# 2026-09-07 (an under-glass sandbox daemon serving deleted code for 16 days;
# an oc default-root daemon stale for 8 days). The census verbs
# (`daemon list`, `daemon reap`) are safe by construction but manual-only —
# and a manual-only cleanup of a recurring leak is a leak.
#
# Idempotent: re-running refreshes the units and restarts the timer.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

mkdir -p "$unit_dir"
cp "$here/ychrome-daemon-reap.service" "$here/ychrome-daemon-reap.timer" "$unit_dir/"
systemctl --user daemon-reload
systemctl --user enable --now ychrome-daemon-reap.timer
systemctl --user start ychrome-daemon-reap.service
echo "installed: ychrome-daemon-reap.timer (hourly; state: $(systemctl --user is-active ychrome-daemon-reap.timer))"
