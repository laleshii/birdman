#!/usr/bin/env bash
# Build, code-sign, and run Birdman.
#
# The signing step is not cosmetic. macOS ties a keychain item's access
# control to the *code signature* of the program reading it. An unsigned
# `cargo build` binary gets a fresh ad-hoc identity on every relink, so
# "Always Allow" applies only to the binary you approved and the next build
# prompts again -- which during development means a dialog every launch, and
# a keyring read that blocks until it is answered.
#
# Signing with a stable identity makes one approval stick.
#
# Override the identity with BIRDMAN_SIGN_ID, or set it to "-" for ad-hoc
# signing (no certificate needed, but the prompts come back).
set -euo pipefail

cd "$(dirname "$0")/.."

IDENTITY="${BIRDMAN_SIGN_ID:-$(security find-identity -v -p codesigning \
  | awk 'NR==1 && /\)/ {print $2}')}"

cargo build "$@"

if [ -n "$IDENTITY" ]; then
  # Every binary that reads the keyring needs signing, not just the app: the
  # daemon reads credentials too, and an unsigned one blocks on its own
  # keychain dialog before any account can sync.
  for binary in birdman-desktop birdmand birdman; do
    [ -f "target/debug/$binary" ] && codesign --force --sign "$IDENTITY" "target/debug/$binary"
  done
  echo "signed with $IDENTITY"
else
  echo "no code signing identity found; running unsigned (expect keychain prompts)" >&2
fi

exec ./target/debug/birdman-desktop
