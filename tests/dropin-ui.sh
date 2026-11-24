#!/bin/bash
# A fresh drop-in home (osada 0.1's pages and osada 0.2's) of this build, then a test script
# against it with the first admin's password. As root:
#
#   tests/dropin-ui.sh target/release/osada ui_header.py [more arguments for the script]
#
# The script gets: <http://127.0.0.1:18989> admin <password> <work dir> [more]. Work dir
# /root/t-dui (removed after a pass); ports 18989 (pages), 18988 (link). No agent, no podman.
set -u
D=/root/t-dui; B=$(realpath "$1"); S=$2; shift 2; HTTP=18989
HERE=$(dirname "$(realpath "$0")")
rm -rf $D; mkdir -p $D/home
OSADA_DATA=$D/home OSADA_BIND=127.0.0.1:$HTTP OSADA_LINK_BIND=127.0.0.1:18988 OSADA_NODE=home $B serve >$D/home.log 2>&1 & HP=$!
for i in $(seq 60); do [ -f $D/home/node/first-admin ] && break; sleep 1; done
PW=$(sed -n 's/.*password: *//p' $D/home/node/first-admin)
python3 -I "$HERE/$S" http://127.0.0.1:$HTTP admin "$PW" $D "$@"; R=$?
kill $HP; wait $HP 2>/dev/null
[ $R = 0 ] && rm -rf $D || echo "logs in $D"
exit $R
