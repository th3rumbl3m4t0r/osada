#!/bin/bash
# Round 22: two fresh drop-ins, A and B, each with its own UPN domain, federated with each other;
# then tests/federation.py against both. As root:
#
#   tests/federation.sh target/release/osada
#
# Work dir /root/t-fed (removed after a pass). A: http://127.0.0.1:18921 (link 18922),
# B: http://127.0.0.1:18923 (link 18924); each one's public URL is its loopback address.
set -u
D=/root/t-fed; BIN=$(realpath "$1")
HERE=$(dirname "$(realpath "$0")")
rm -rf $D; mkdir -p $D/a $D/b
OSADA_DATA=$D/a OSADA_BIND=127.0.0.1:18921 OSADA_LINK_BIND=127.0.0.1:18922 OSADA_NODE=a OSADA_PUBLIC_URL=http://127.0.0.1:18921 $BIN serve >$D/a.log 2>&1 & PA=$!
OSADA_DATA=$D/b OSADA_BIND=127.0.0.1:18923 OSADA_LINK_BIND=127.0.0.1:18924 OSADA_NODE=b OSADA_PUBLIC_URL=http://127.0.0.1:18923 $BIN serve >$D/b.log 2>&1 & PB=$!
for i in $(seq 60); do [ -f $D/a/node/first-admin ] && [ -f $D/b/node/first-admin ] && break; sleep 1; done
PWA=$(sed -n 's/.*password: *//p' $D/a/node/first-admin)
PWB=$(sed -n 's/.*password: *//p' $D/b/node/first-admin)
python3 -I "$HERE/federation.py" http://127.0.0.1:18921 "$PWA" http://127.0.0.1:18923 "$PWB" $D; R=$?
kill $PA $PB; wait $PA $PB 2>/dev/null
[ $R = 0 ] && rm -rf $D || echo "logs in $D"
exit $R
