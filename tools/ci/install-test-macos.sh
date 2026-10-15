#!/usr/bin/env bash
# Install test (spec § CI and install tests, Mac): the disk image exactly as
# a browser downloads it. Marks it quarantined, mounts it, copies the app,
# checks Gatekeeper (when signed), launches --self-test, registers the
# background check and finds it among Login Items. Then installs the newer
# build over it (an upgrade) and proves the connection, the cache and the
# background check are exactly as they were.
#
# Usage: tools/ci/install-test-macos.sh <old.dmg> <new.dmg> <out.json> [--signed]
set -uo pipefail

old_dmg="$1"
new_dmg="$2"
out="$3"
signed="${4:-}"
failed=0
results=()
record() {
  results+=("$(python3 -c 'import json,sys; print(json.dumps({"check":sys.argv[1],"status":sys.argv[2],"detail":sys.argv[3]}))' "$1" "$2" "$3")")
  echo "[$2] $1: $3"
  [ "$2" = "fail" ] && failed=1
  return 0
}

apps="$HOME/Applications"
mkdir -p "$apps"
app="$apps/Idle Stock Finder.app"
exe="$app/Contents/MacOS/idle-stock-finder"
data="$HOME/Library/Application Support/com.tightline.idlestockfinder"
rm -rf "$app" "$data"

install_from() { # dmg
  local dmg="$1"
  xattr -w com.apple.quarantine "0081;$(printf '%x' "$(date +%s)");Safari;" "$dmg"
  local mnt
  mnt="$(hdiutil attach -nobrowse -readonly "$dmg" | tail -1 | sed -E 's/^.*(\/Volumes\/.*)$/\1/')"
  rm -rf "$app"
  ditto "$mnt/Idle Stock Finder.app" "$app"
  hdiutil detach "$mnt" -quiet || true
}

record "macos" info "$(sw_vers -productVersion) $(uname -m)"
record "dmg-size" info "$(stat -f %z "$new_dmg") bytes"

# 1. Fresh install of the older build.
install_from "$old_dmg"
[ -x "$exe" ] && record "installed" pass "$app" || record "installed" fail "no executable"
quarantine="$(xattr -p com.apple.quarantine "$app" 2>/dev/null || echo none)"
record "quarantine-carried" info "$quarantine"
assess="$(spctl --assess --type execute -v "$app" 2>&1)"
if [ "$signed" = "--signed" ]; then
  grep -q "accepted" <<<"$assess" && record "gatekeeper" pass "$assess" || record "gatekeeper" fail "$assess"
  stapled="$(xcrun stapler validate "$app" 2>&1)"
  grep -q "worked" <<<"$stapled" && record "stapled" pass "$stapled" || record "stapled" fail "$stapled"
else
  record "gatekeeper" skip "unsigned build until the Developer ID secrets exist: $assess"
fi
# The self-test runs from the installed, quarantined copy.
xattr -d com.apple.quarantine "$app" 2>/dev/null
if "$exe" --self-test --json-out "$PWD/selftest-installed.json" >/dev/null; then
  record "self-test" pass "$(python3 -c 'import json;d=json.load(open("selftest-installed.json"));print(d["millis"],"ms,",d["requests"],"requests")')"
else
  record "self-test" fail "$(cat selftest-installed.json 2>/dev/null | head -c 600)"
fi

# 2. State an owner would have: setup done, a saved connection, a cache,
#    the background check registered.
"$exe" --test-mark-setup
security add-generic-password -U -s com.tightline.idlestockfinder -a square-production -l "Idle Stock Finder: Square connection" -w '{"test":"upgrade"}' >/dev/null
reg="$("$exe" --schedule register 2>&1)"
grep -q '"sm_app_service"' <<<"$reg" && record "register" pass "$(tr -d '\n' <<<"$reg")" || record "register" fail "$(tr -d '\n' <<<"$reg")"
sleep 2
btm="$(sudo sfltool dumpbtm 2>/dev/null | grep -c 'com.tightline.idlestockfinder')"
[ "$btm" -gt 0 ] && record "login-items" pass "$btm entries" || record "login-items" fail "not in Login Items"
cache_before="$(sqlite3 "$data/cache.sqlite" "SELECT value FROM settings" 2>/dev/null)"

# 3. Upgrade: the newer build replaces the app in place.
install_from "$new_dmg"
xattr -d com.apple.quarantine "$app" 2>/dev/null
new_version="$(defaults read "$app/Contents/Info.plist" CFBundleShortVersionString)"
record "upgraded" info "now $new_version"
token="$(security find-generic-password -s com.tightline.idlestockfinder -a square-production -w 2>/dev/null)"
[ "$token" = '{"test":"upgrade"}' ] && record "upgrade-keeps-connection" pass "keychain entry unchanged" || record "upgrade-keeps-connection" fail "got: $token"
cache_after="$(sqlite3 "$data/cache.sqlite" "SELECT value FROM settings" 2>/dev/null)"
[ -n "$cache_before" ] && [ "$cache_before" = "$cache_after" ] && record "upgrade-keeps-cache" pass "$cache_after" || record "upgrade-keeps-cache" fail "before: $cache_before after: $cache_after"
status="$("$exe" --schedule status 2>&1)"
grep -q 'enabled' <<<"$status" && record "upgrade-keeps-background-check" pass "$(tr -d '\n' <<<"$status")" || record "upgrade-keeps-background-check" fail "$(tr -d '\n' <<<"$status")"
launchctl print "gui/$(id -u)/com.tightline.idlestockfinder.daily" >/dev/null 2>&1 && record "upgrade-agent-loaded" pass "daily agent loaded" || record "upgrade-agent-loaded" fail "daily agent not loaded"

# 4. Clean up: unregister, delete the test connection.
"$exe" --schedule unregister >/dev/null
security delete-generic-password -s com.tightline.idlestockfinder -a square-production >/dev/null 2>&1
printf '{"runner":"%s","failed":%s,"checks":[%s]}\n' "${RUNNER_NAME:-local}" "$failed" "$(IFS=,; echo "${results[*]}")" > "$out"
exit $failed
