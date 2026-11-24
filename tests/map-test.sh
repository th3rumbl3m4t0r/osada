#!/bin/bash
# Two nodes of this build on localhost (home with its pages, and an edge), then tests/ui_map.py.
#   tests/map-test.sh target/release/osada [ui_assign.py]   (as root; T=/root/t-map, removed after)
set -u
BIN=$(realpath "$1"); HERE=$(dirname "$(realpath "$0")"); T=${T:-/root/t-map}
rm -rf "$T"; mkdir -p "$T"
"$BIN" init --data "$T/a" --node home > /dev/null
"$BIN" serve --data "$T/a" --listen 127.0.0.1:47801 --http 127.0.0.1:18801 > "$T/a.log" 2>&1 &
a=$!
sleep 1
TOKEN=$("$BIN" token --data "$T/a" --node edge --roles idp,store,edge-http)
"$BIN" join --data "$T/b" --node edge --token "$TOKEN" --peer home@127.0.0.1:47801 > "$T/join.log" 2>&1
"$BIN" serve --data "$T/b" --listen 127.0.0.1:47802 --peer home@127.0.0.1:47801 > "$T/b.log" 2>&1 &
b=$!
for _ in $(seq 1 100); do [ -s "$T/a/first-admin" ] && break; sleep 0.2; done
python3 -I "$HERE/${2:-ui_map.py}" "$T/a" http://127.0.0.1:18801
rc=$?
kill $a $b; wait 2>/dev/null
[ $rc = 0 ] && rm -rf "$T" || echo "logs in $T"
exit $rc
