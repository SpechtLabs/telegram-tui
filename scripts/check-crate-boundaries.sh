#!/usr/bin/env bash
set -euo pipefail
fail=0
if cargo tree -p tgt-core -e normal --prefix none | grep -qE '^(ratatui|crossterm) v'; then
  echo "FORBIDDEN: tgt-core depends on ratatui/crossterm" >&2; fail=1
fi
if cargo tree -p tgt-ui -e normal --prefix none | grep -qE '^tdlib-rs v'; then
  echo "FORBIDDEN: tgt-ui depends on tdlib-rs" >&2; fail=1
fi
exit "$fail"
