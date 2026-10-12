#!/usr/bin/env bash
# Makes the disk image from the app bundle: the app and a link to
# Applications, compressed. Signs, notarizes and staples it when a Developer
# ID identity and notarization credentials are in the environment.
# Usage: tools/release/make-dmg.sh <Dead Stock Finder.app> <out.dmg>
set -euo pipefail
app="$1"
out="$2"
stage="$(mktemp -d)"
ditto "$app" "$stage/Dead Stock Finder.app"
ln -s /Applications "$stage/Applications"
rm -f "$out"
hdiutil create -volname "Dead Stock Finder" -srcfolder "$stage" -ov -format UDZO -fs HFS+ "$out" >/dev/null
rm -rf "$stage"
if [ -n "${APPLE_SIGNING_IDENTITY:-}" ] && [ "${APPLE_SIGNING_IDENTITY}" != "-" ]; then
  codesign --force --sign "$APPLE_SIGNING_IDENTITY" --timestamp "$out"
  if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
    xcrun notarytool submit "$out" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait
    xcrun stapler staple "$out"
  fi
fi
ls -l "$out"
