#!/bin/bash
# admin → mail against a real Stalwart (D-U43, D-U44), alone in a network namespace: Stalwart
# 0.16 (as tests/stalwart-ns.sh), a new one-node cluster of this build with the mail instance on
# it, and tests/ui_mailboxes.py in headless Chromium. As root:
#
#   T=/root/t-sw unshare --net --fork tests/mail-ns.sh target/release/osada
set -u
T=${T:-/root/t-sw}
BIN=$(realpath "$1")
HERE=$(dirname "$(realpath "$0")")
ip link set lo up
rm -rf "$T/data" "$T/node" && mkdir -p "$T/data"
printf '{"@type":"RocksDb","path":"%s/data"}\n' "$T" > "$T/config.json"
STALWART_RECOVERY_ADMIN="$(cat "$T/cred")" "$T/dl/stalwart" --config="$T/config.json" > "$T/stalwart.log" 2>&1 &
sw=$!
for _ in $(seq 1 60); do curl -s -o /dev/null http://127.0.0.1:8080/ && break; sleep 0.5; done
"$BIN" init --data "$T/node" --node mailtest > /dev/null
mkdir -m 700 "$T/node/mail"
(umask 077; printf 'Basic %s\n' "$(base64 -w0 < "$T/cred")" > "$T/node/mail/stalwart.auth")
"$BIN" serve --data "$T/node" --listen 127.0.0.1:47101 --http 127.0.0.1:18096 > "$T/node.log" 2>&1 &
node=$!
for _ in $(seq 1 100); do [ -s "$T/node/first-admin" ] && break; sleep 0.2; done
python3 -I "$HERE/ui_mailboxes.py" "$T/node" http://127.0.0.1:18096
rc=$?
kill $node $sw; wait 2>/dev/null
exit $rc
