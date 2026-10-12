#!/usr/bin/env bash
# Allowlist trace (spec § CI, Linux): runs the self-test, a full check
# against the loopback mock, under strace and fails on any outbound
# connection or DNS lookup that isn't the mock on 127.0.0.1.
# Usage: tools/ci/allowlist-trace.sh <dead-stock-finder binary> <out.json>
set -euo pipefail
exe="$1"
out="$2"
strace -f -qq -e trace=connect,sendto,sendmsg -o trace-connect.txt "$exe" --self-test --json-out "$out"
python3 - trace-connect.txt <<'PY'
import re, sys
bad = []
for line in open(sys.argv[1]):
    if "connect(" not in line and "sendto(" not in line and "sendmsg(" not in line:
        continue
    if "AF_UNIX" in line or "AF_NETLINK" in line:
        continue
    m = re.search(r'sin_addr=inet_addr\("([^"]+)"\)', line) or re.search(r'inet_pton\(AF_INET6, "([^"]+)"', line)
    if m and m.group(1) in ("127.0.0.1", "::1"):
        continue
    if "AF_INET" in line or "htons(53)" in line:
        bad.append(line.strip())
for b in bad:
    print("outside the allowlist:", b)
print(f"allowlist trace: {len(bad)} connections outside loopback")
sys.exit(1 if bad else 0)
PY
