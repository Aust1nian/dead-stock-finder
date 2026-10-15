#!/usr/bin/env bash
# Scheduler test (spec § Tests, "Scheduler"): installs the app into the
# user's own Applications folder (no admin), registers the background check,
# reads the registration back from launchd, fires both agents, and confirms
# a check ran with no window and that the lock stopped an overlapping run.
# Results go to a JSON file; the script exits nonzero if any check failed.
#
# Usage: tools/ci/scheduler-test-macos.sh <Idle Stock Finder.app | .zip> <out.json>
set -uo pipefail

src="$1"
out="$2"
failed=0
results=()
record() { # name status detail
  results+=("$(python3 -c 'import json,sys; print(json.dumps({"check":sys.argv[1],"status":sys.argv[2],"detail":sys.argv[3]}))' "$1" "$2" "$3")")
  echo "[$2] $1: $3"
  [ "$2" = "fail" ] && failed=1
  return 0
}

apps="$HOME/Applications"
mkdir -p "$apps"
app="$apps/Idle Stock Finder.app"
rm -rf "$app"
if [[ "$src" == *.zip ]]; then
  ditto -x -k "$src" "$apps"
else
  ditto "$src" "$app"
fi
exe="$app/Contents/MacOS/idle-stock-finder"
uid="$(id -u)"
data="$HOME/Library/Application Support/com.tightline.idlestockfinder"
log="$data/logs/check.log"
rm -rf "$data"

[ -x "$exe" ] && record "installed" pass "$app" || record "installed" fail "no executable at $exe"
for p in signin daily; do
  plist="$app/Contents/Library/LaunchAgents/com.tightline.idlestockfinder.$p.plist"
  [ -f "$plist" ] && record "bundled-$p-plist" pass "$(plutil -lint "$plist")" || record "bundled-$p-plist" fail "missing $plist"
done
record "macos" info "$(sw_vers -productVersion) $(uname -m)"

"$exe" --test-mark-setup
reg="$("$exe" --schedule register 2>&1)"
echo "$reg"
if grep -q '"sm_app_service"' <<<"$reg" && ! grep -q '"error"' <<<"$reg"; then
  record "register" pass "$(tr -d '\n' <<<"$reg")"
else
  record "register" fail "$(tr -d '\n' <<<"$reg")"
fi

wait_for() { # pattern seconds
  for _ in $(seq 1 "$2"); do
    grep -q "$1" "$log" 2>/dev/null && return 0
    sleep 1
  done
  return 1
}

# Read the registration back from launchd.
for label in com.tightline.idlestockfinder.signin com.tightline.idlestockfinder.daily; do
  printed="$(launchctl print "gui/$uid/$label" 2>&1)"
  if [ $? -eq 0 ] && grep -q -- "--check" <<<"$printed"; then
    record "launchctl-print-$label" pass "$(grep -E 'program|state|arguments' <<<"$printed" | head -5 | tr -s ' \n' ' ')"
  else
    record "launchctl-print-$label" fail "$(head -c 400 <<<"$printed")"
  fi
done
btm="$(sudo sfltool dumpbtm 2>/dev/null | grep -i -A3 'idlestockfinder' | head -12)"
[ -n "$btm" ] && record "login-items" pass "$(tr -s ' \n' ' ' <<<"$btm")" || record "login-items" fail "not listed by sfltool dumpbtm"

# The sign-in agent runs at load (registration counts as a load).
launchctl kickstart "gui/$uid/com.tightline.idlestockfinder.signin" >/dev/null 2>&1
if wait_for "headless check (signin) finished" 60; then
  line="$(grep "headless check (signin) finished" "$log" | tail -1)"
  grep -q "windows owned by this process: 0" <<<"$line" && record "signin-fired" pass "$line" || record "signin-fired" fail "$line"
else
  record "signin-fired" fail "no sign-in check in the log: $(tail -5 "$log" 2>/dev/null)"
fi

launchctl kickstart "gui/$uid/com.tightline.idlestockfinder.daily" >/dev/null 2>&1
if wait_for "headless check (tick) finished" 60; then
  record "daily-fired" pass "$(grep "headless check (tick) finished" "$log" | tail -1)"
else
  record "daily-fired" fail "no tick check in the log: $(tail -5 "$log" 2>/dev/null)"
fi

# The lock: hold it, fire the sign-in agent, expect it to step aside.
"$exe" --hold-lock 40 &
holder=$!
sleep 3
launchctl kickstart -k "gui/$uid/com.tightline.idlestockfinder.signin" >/dev/null 2>&1
if wait_for "another check is running" 40; then
  record "lock" pass "$(grep "another check is running" "$log" | tail -1)"
else
  record "lock" fail "overlapping run not stopped: $(tail -5 "$log")"
fi
kill "$holder" 2>/dev/null
wait "$holder" 2>/dev/null

# Unregister and confirm launchd forgot both.
"$exe" --schedule unregister >/dev/null
sleep 2
gone=1
for label in com.tightline.idlestockfinder.signin com.tightline.idlestockfinder.daily; do
  launchctl print "gui/$uid/$label" >/dev/null 2>&1 && gone=0
done
[ $gone -eq 1 ] && record "unregister" pass "launchd has neither agent" || record "unregister" fail "an agent is still loaded"

record "log" info "$(tail -20 "$log" 2>/dev/null | tr '\n' '|')"
printf '{"runner":"%s","failed":%s,"checks":[%s]}\n' "${RUNNER_NAME:-local}" "$failed" "$(IFS=,; echo "${results[*]}")" > "$out"
exit $failed
