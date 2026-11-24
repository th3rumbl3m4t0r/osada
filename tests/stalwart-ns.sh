#!/bin/bash
# A real Stalwart for a test, alone in a network namespace (none of the host's ports), D-U43:
#
#   T=/root/t-sw   # with dl/stalwart (the release binary) and cred (admin:<random>, 0600)
#   unshare --net --fork tests/stalwart-ns.sh target/release/deps/osada-<hash> --ignored --exact mail::tests::mailboxes_in_a_real_stalwart
#
# Starts it on a fresh data folder in $T, runs the command with OSADA_TEST_STALWART(_AUTH), stops it.
set -u
T=${T:-/root/t-sw}
ip link set lo up
rm -rf "$T/data" && mkdir -p "$T/data"
printf '{"@type":"RocksDb","path":"%s/data"}\n' "$T" > "$T/config.json"
STALWART_RECOVERY_ADMIN="$(cat "$T/cred")" "$T/dl/stalwart" --config="$T/config.json" > "$T/stalwart.log" 2>&1 &
pid=$!
for _ in $(seq 1 60); do curl -s -o /dev/null http://127.0.0.1:8080/ && break; sleep 0.5; done
OSADA_TEST_STALWART=http://127.0.0.1:8080 OSADA_TEST_STALWART_AUTH="$T/cred" "$@"
rc=$?
kill $pid; wait $pid 2>/dev/null
exit $rc
