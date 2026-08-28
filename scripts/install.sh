#!/usr/bin/env bash
# Install Birdman, and make the desktop app discoverable as an application.
#
# `cargo install` leaves a bare executable in ~/.cargo/bin, which is not an
# app on either platform: macOS surfaces only .app bundles in Spotlight and
# Launchpad, and Linux launchers read .desktop entries. This installs the
# binaries and then wraps them appropriately.
#
# Usage: scripts/install.sh [--no-desktop]
set -euo pipefail

cd "$(dirname "$0")/.."

WITH_DESKTOP=1
for arg in "$@"; do
  case "$arg" in
    --no-desktop) WITH_DESKTOP=0 ;;
    -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 1 ;;
  esac
done

BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
VERSION="$(awk -F'"' '/^version = / {print $2; exit}' Cargo.toml)"

cargo install --path crates/birdman-daemon --force
cargo install --path crates/birdman-cli --force

if [ "$WITH_DESKTOP" -eq 0 ]; then
  echo "installed birdmand and birdman into $BIN_DIR"
  exit 0
fi

cargo install --path crates/birdman-ui --force

case "$(uname -s)" in
  Darwin)
    APP="$HOME/Applications/Birdman.app"
    rm -rf "$APP"
    mkdir -p "$APP/Contents/MacOS"
    sed "s/__VERSION__/$VERSION/g" packaging/Info.plist > "$APP/Contents/Info.plist"

    # The daemon goes inside the bundle too. The client looks for `birdmand`
    # beside its own binary and then on PATH, and an app launched from Finder
    # gets LaunchServices' minimal PATH, which has no ~/.cargo/bin -- so the
    # sibling copy is the only one it can find.
    cp "$BIN_DIR/birdman-desktop" "$APP/Contents/MacOS/birdman-desktop"
    cp "$BIN_DIR/birdmand" "$APP/Contents/MacOS/birdmand"
    mkdir -p "$APP/Contents/Resources"
    cp packaging/Birdman.icns "$APP/Contents/Resources/Birdman.icns"

    # macOS ties a keychain item's access control to the code signature, so an
    # unsigned build re-prompts every time it is replaced. Same reasoning as
    # scripts/dev-run.sh; override with BIRDMAN_SIGN_ID, or "-" for ad-hoc.
    IDENTITY="${BIRDMAN_SIGN_ID:-$(security find-identity -v -p codesigning \
      | awk 'NR==1 && /\)/ {print $2}')}"
    if [ -n "$IDENTITY" ]; then
      # Inner binaries before the bundle: signing the wrapper first is what
      # `--deep` did, and Apple deprecated it.
      codesign --force --sign "$IDENTITY" "$APP/Contents/MacOS/birdmand"
      codesign --force --sign "$IDENTITY" "$APP/Contents/MacOS/birdman-desktop"
      codesign --force --sign "$IDENTITY" "$APP"
      echo "signed with $IDENTITY"
    else
      echo "no code signing identity found; expect a keychain prompt on first sync" >&2
    fi

    lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
    [ -x "$lsregister" ] && "$lsregister" -f "$APP" || true
    echo "installed $APP -- it is now in Launchpad and Spotlight"
    ;;
  Linux)
    APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
    mkdir -p "$APPS"
    # Absolute Exec: a launcher does not run a login shell, so ~/.cargo/bin is
    # not on its PATH.
    sed "s|^Exec=.*|Exec=$BIN_DIR/birdman-desktop|" \
      packaging/birdman.desktop > "$APPS/birdman.desktop"
    # Icon=birdman resolves through the theme, so the file has to live there.
    ICONS="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/256x256/apps"
    mkdir -p "$ICONS"
    cp packaging/birdman-256.png "$ICONS/birdman.png"
    if command -v gtk-update-icon-cache >/dev/null; then
      gtk-update-icon-cache -f -t "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor" 2>/dev/null || true
    fi
    if command -v update-desktop-database >/dev/null; then
      update-desktop-database "$APPS" || true
    fi
    echo "installed $APPS/birdman.desktop -- Birdman is in your application launcher"
    ;;
  *)
    echo "unsupported platform: $(uname -s); binaries are in $BIN_DIR" >&2
    exit 1
    ;;
esac
