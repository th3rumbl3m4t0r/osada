#!/bin/bash
# Round 18: sites at /web/<site>/ and the office suite (ZetaOffice) on a drop-in home, then
# tests/office.py in headless Chromium (as root). ZETA: a folder with ZetaOffice's soffice.*
# files (plain and .br), e.g. as deploy/zetaoffice-site.sh makes them.
#
#   ZETA=/root/dl/zetaoffice-20261118/files tests/office.sh target/release/osada
set -u
D=/root/t-office; B=$(realpath "${1:-/root/osada-0.2/target/release/osada}"); HTTP=18981
HERE=$(dirname "$(realpath "$0")")
ZETA=${ZETA:?a folder with ZetaOffice\'s files}
rm -rf $D; mkdir -p $D/home
OSADA_DATA=$D/home OSADA_BIND=127.0.0.1:$HTTP OSADA_LINK_BIND=127.0.0.1:18980 OSADA_NODE=home $B serve >$D/home.log 2>&1 & HP=$!
for i in $(seq 60); do [ -f $D/home/node/first-admin ] && break; sleep 1; done
PW=$(sed -n 's/.*password: *//p' $D/home/node/first-admin)
# the site's folder: ZetaOffice's files and osada's page
mkdir -p $D/home/node/web/zetaoffice
cp "$ZETA"/soffice.* $D/home/node/web/zetaoffice/
cp "$HERE"/../web/zetaoffice/* $D/home/node/web/zetaoffice/
python3 -I "$HERE/office.py" http://127.0.0.1:$HTTP admin "$PW" $D; R=$?
kill $HP; wait 2>/dev/null
[ $R = 0 ] && rm -rf $D
exit $R
