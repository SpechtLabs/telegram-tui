#!/usr/bin/env bash
# Asserts that SIGNING_IDENTITY/OIDC_ISSUER agree between the two places that
# have to make the same real-world claim in two languages that can't share a
# `const`:
#
#   - crates/app/src/update.rs — what a real client (`tgt update
#     --require-signature`) verifies a downloaded release against.
#   - .github/workflows/release.yaml's `build` job env — what a repair's
#     "Decide whether to rebuild this target" step verifies an
#     already-published release against, to answer "is this still good"
#     before deciding whether to touch it (task #73).
#
# A drift in either direction is a real failure, not a lint nit. Rust ahead
# of the workflow: `--require-signature` starts rejecting every genuine
# release, and the error reads as tampering, not as a stale pin. Workflow
# ahead of Rust: the rebuild-skip check verifies published assets against an
# identity no real client checks against, which can pass when a real
# client's check would fail — silently reintroducing the mutable-release
# window task #73 exists to close.
#
# Not a Rust test: the constants in update.rs are private, and this file is
# only ever meant to be read text — reading it under `cargo test` would mean
# either weakening its visibility for a check that gains nothing from
# compiling it, or coordinating that change with whoever else has the file
# open (see the task's own history). scripts/check-crate-boundaries.sh is
# the same shape of precedent: what's being checked here — two files, in two
# languages, agreeing byte-for-byte — isn't a property of any crate's
# compiled behavior, so a Rust `#[test]` isn't a more natural home for it,
# just a more roundabout one.
set -euo pipefail
cd "$(dirname "$0")/.."

rust_file="crates/app/src/update.rs"
workflow_file=".github/workflows/release.yaml"

# SIGNING_IDENTITY is a `concat!("...", "...")` split across two lines, not
# a plain literal — pull out both quoted fragments and join them with no
# separator, exactly what concat! does at compile time.
rust_identity="$(awk '/const SIGNING_IDENTITY/,/\);/' "$rust_file" |
  grep -o '"[^"]*"' | tr -d '"' | tr -d '\n')"
rust_issuer="$(sed -n 's/^const OIDC_ISSUER: &str = "\(.*\)";$/\1/p' "$rust_file")"

# The workflow's copies are plain unquoted YAML scalars in the build job's
# `env:` block. Anchored to the start of the (trimmed) line so this can't
# also match the `--certificate-identity "$SIGNING_IDENTITY"` usage further
# down, which has no trailing colon.
workflow_identity="$(sed -n 's/^ *SIGNING_IDENTITY: *//p' "$workflow_file")"
workflow_issuer="$(sed -n 's/^ *OIDC_ISSUER: *//p' "$workflow_file")"

fail=0

for pair in \
  "SIGNING_IDENTITY in $rust_file:rust_identity" \
  "SIGNING_IDENTITY in $workflow_file:workflow_identity" \
  "OIDC_ISSUER in $rust_file:rust_issuer" \
  "OIDC_ISSUER in $workflow_file:workflow_issuer"; do
  label="${pair%%:*}"
  var="${pair##*:}"
  if [ -z "${!var}" ]; then
    echo "error: could not find $label — has the surrounding code moved or been reworded?" >&2
    fail=1
  fi
done
[ "$fail" -eq 1 ] && exit 1

if [ "$rust_identity" != "$workflow_identity" ]; then
  echo "error: SIGNING_IDENTITY disagrees between the two files that must agree" >&2
  echo "  $rust_file:     $rust_identity" >&2
  echo "  $workflow_file: $workflow_identity" >&2
  echo "Update both. See the comment above OIDC_ISSUER in $rust_file for why they must match." >&2
  fail=1
fi

if [ "$rust_issuer" != "$workflow_issuer" ]; then
  echo "error: OIDC_ISSUER disagrees between the two files that must agree" >&2
  echo "  $rust_file:     $rust_issuer" >&2
  echo "  $workflow_file: $workflow_issuer" >&2
  echo "Update both. See the comment above OIDC_ISSUER in $rust_file for why they must match." >&2
  fail=1
fi

if [ "$fail" -eq 0 ]; then
  echo "ok: SIGNING_IDENTITY and OIDC_ISSUER agree between $rust_file and $workflow_file"
fi

exit "$fail"
