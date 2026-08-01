#!/usr/bin/env bash
# Regenerates the `tgt --demo` asciinema recording embedded in the README
# and the docs site. See crates/app/src/demo/ (mod.rs's module docs explain
# what `--demo` is and why it can't reach a real account; content.rs/
# script.rs explain the fixed chat/message data this recording drives
# through) — this script is the other half: the exact keystroke sequence
# that turns that fixture into a recording, so a future UI change is a
# `git diff` away from a fresh one rather than a re-performed take.
#
# Requires tmux and asciinema (pinned in .mise.toml — `mise install`).
#
# Usage: ./scripts/record-demo.sh [output-file]
#   Defaults to assets/demo.cast, deliberately outside docs/** (owned by the
#   README/docs pass — see that work's own notes on this). Whoever wires up
#   the README/docs embed may move it; this script only produces the file
#   and does not assume where it ends up.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

out="${1:-assets/demo.cast}"
mkdir -p "$(dirname "$out")"
rm -f "$out"

cols=110
rows=30
session="tgt-demo-record-$$"

cleanup() {
    tmux kill-session -t "$session" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "Building tgt (release)..." >&2
cargo build -p tgt-app --release >&2

bin="$repo_root/target/release/tgt"

tmux kill-session -t "$session" >/dev/null 2>&1 || true
tmux new-session -d -s "$session" -x "$cols" -y "$rows" \
    mise exec -- asciinema record \
        --overwrite \
        --window-size "${cols}x${rows}" \
        --title "tgt: a keyboard-driven terminal Telegram client" \
        --idle-time-limit 2 \
        --command "$bin --demo" \
        "$out"

wait_for() {
    # $2 is a count of 0.2s polls, not seconds (keeps this integer-only —
    # no `bc`/`awk` dependency for something this small).
    local pattern="$1" max_checks="${2:-50}" checks=0
    while ! tmux capture-pane -t "$session" -p 2>/dev/null | grep -qF "$pattern"; do
        sleep 0.2
        checks=$((checks + 1))
        if [ "$checks" -ge "$max_checks" ]; then
            echo "timed out waiting for: $pattern" >&2
            tmux capture-pane -t "$session" -p >&2 || true
            exit 1
        fi
    done
}

# Boot: chat list with folders and the unread badges (Ada: 3, Release Notes: 1).
wait_for "select a chat"
sleep 1.2

# Open Nova — the reply, the ❤️ reaction and the photo are all visible the
# instant it opens (its history is a padded 50-message page ending on them;
# see content.rs's module docs).
tmux send-keys -t "$session" Down
sleep 0.5
tmux send-keys -t "$session" Enter
wait_for "See you soon"
sleep 1.4

# Selection mode (↑ from the empty composer), walk up three messages to land
# on the spoiler, reveal it with its chip shortcut ('v').
tmux send-keys -t "$session" Up
sleep 0.4
tmux send-keys -t "$session" Up
sleep 0.4
tmux send-keys -t "$session" Up
sleep 0.6
tmux send-keys -t "$session" v
wait_for "hunter2, don't tell anyone"
sleep 1.6

# Back down to the newest message before leaving selection mode. Escape
# does NOT reset scroll to `Scroll::Bottom` on its own — only landing
# selection on the newest loaded message does (state::conversation's
# module docs: "Selecting the newest loaded message re-pins to
# Scroll::Bottom"), and the spoiler is two messages up from it. Skip this
# and the conversation stays anchored where the spoiler was: the two sends
# below still work (`FakeTd` answers them regardless of what's on screen),
# but the newly sent messages render off the anchored viewport, which is
# just as invisible in the recording as no send happening at all — this bit
# the first version of this script, verified live before it did.
tmux send-keys -t "$session" Down
sleep 0.3
tmux send-keys -t "$session" Down
sleep 0.3

tmux send-keys -t "$session" Escape
# Waits for the composer's own hint bar rather than a blind sleep: proof
# focus is back on it (and not, say, still mid-redraw from the reveal)
# before typing — `asciinema record`'s extra pty hop is slower than driving
# the binary directly, and a typed string that lands before the composer is
# ready is silently dropped rather than queued.
wait_for "alt+⏎ newline"
sleep 0.5

# Two live sends, so the composer's real Sending -> Sent transition plays
# out. The gap between them is what makes the "⋯" -> "✓" flip on message #1
# visible at all — see demo/script.rs's module docs ("Showing Sending ->
# Sent without racing FakeTd's own timing") for why it has to be two
# messages rather than one message plus a timed confirmation.
tmux send-keys -t "$session" -l "Recording this for the docs — thanks for reviewing! 🎬"
wait_for "Recording this for the docs" 25
sleep 0.4
tmux send-keys -t "$session" Enter
wait_for "🎬 ⋯"
sleep 1.3

tmux send-keys -t "$session" -l "Back in a bit 👋"
wait_for "Back in a bit" 25
sleep 0.4
tmux send-keys -t "$session" Enter
wait_for "🎬 ✓"
sleep 1.2

tmux send-keys -t "$session" C-c

# `tgt --demo` exits, which ends `asciinema record`'s wrapped command and
# closes the tmux pane (and with it, since it's the only one, the session).
waited=0
while tmux has-session -t "$session" >/dev/null 2>&1; do
    sleep 0.3
    waited=$((waited + 1))
    if [ "$waited" -gt 30 ]; then
        echo "tgt --demo did not exit after ctrl+c" >&2
        exit 1
    fi
done

echo "Recorded to $out" >&2
