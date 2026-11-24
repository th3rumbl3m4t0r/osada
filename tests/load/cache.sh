#!/bin/bash
# swai: the edge cache with big files (signed-in downloads and public links).
#   cache.sh <step>   (USER_PW: the test account's password; links in /root/load/links.txt)
set -u
cd /root/load
FSN=198.18.1.228 ORE=198.18.1.128 SIN=198.18.1.109
H=test.example.net
declare -A SUM=([big1]=3e508ff28e8f7482 [big2]=09b60e6cd1bdd114 [herd]=4c540b28061f27af)
L1=$(awk '$1=="big1"{print $2}' links.txt) L2=$(awk '$1=="big2"{print $2}' links.txt)
cookie() { # the test account's session at an edge
    curl -s --resolve $H:443:$1 -c - -H 'x-shelf: 1' -H 'content-type: application/json' \
        -d "{\"name\":\"alice\",\"password\":\"$USER_PW\"}" https://$H/api/signin | awk '$6=="osada"{print $7}'
}
# dl <label> <edge> <path> <file name> [curl options…]: one download, its line
dl() {
    local label=$1 edge=$2 path=$3 name=$4; shift 4
    local out=/tmp/dl-$label-$RANDOM
    local w; w=$(curl -s --resolve $H:443:$edge -o $out -D $out.h -w '%{http_code} %{size_download} %{time_total} %{speed_download} %{time_starttransfer}' "$@" "https://$H$path")
    local code size t speed ttfb; read -r code size t speed ttfb <<<"$w"
    local hit; hit=$(grep -i '^x-edge-cache' $out.h | tr -d '\r' | awk '{print $2}')
    local ok=""; [ -n "$name" ] && [ "$code" = 200 ] && ok=$( [ "$(sha256sum $out | cut -c1-16)" = "${SUM[$name]}" ] && echo "sum ok" || echo "SUM WRONG")
    printf '%-22s %s %5.1f MB in %6.2f s = %7.1f Mbit/s  ttfb %5.0f ms  cache %-4s %s  [%s]\n' "$label" "$code" "$(echo "$size/1048576" | bc -l)" "$t" "$(echo "$speed*8/1000000" | bc -l)" "$(echo "$ttfb*1000" | bc -l)" "${hit:--}" "$ok" "$(date +%s)"
    rm -f $out $out.h
}
probe() { ./run.sh "$1" me 20 "$2" $FSN >/tmp/probe-$1.txt 2>&1 & }
step=${1:-}
case $step in
cold-warm)
    C=$(cookie $FSN)
    probe c1-probe-cold 60s; sleep 5
    dl c1-cold-signed-in $FSN /api/raw/home/big1.bin big1 -H "cookie: osada=$C"
    wait; cat /tmp/probe-c1-probe-cold.txt
    probe c2-probe-warm 25s; sleep 5
    dl c2-warm-signed-in $FSN /api/raw/home/big1.bin big1 -H "cookie: osada=$C"
    dl c2-warm-again $FSN /api/raw/home/big1.bin big1 -H "cookie: osada=$C"
    wait; cat /tmp/probe-c2-probe-warm.txt
    t0=$(date +%s)
    for i in 1 2 3 4 5 6 7 8; do dl c3-warm-x8-$i $FSN /api/raw/home/big1.bin big1 -H "cookie: osada=$C" & done; wait
    echo "c3: 8 at once, $(( $(date +%s) - t0 )) s for 800 MB"
    dl c4-warm-range-10MB $FSN /api/raw/home/big1.bin "" -H "cookie: osada=$C" -H 'range: bytes=52428800-62914559'
    dl c5-link-same-file $FSN /s/$L1/raw big1
    ;;
link)
    dl c6-link-cold $FSN /s/$L2/raw big2
    dl c6-link-warm $FSN /s/$L2/raw big2
    ;;
herd)
    C=$(cookie $FSN)
    for i in 1 2 3 4; do dl c8-herd-cold-$i $FSN /api/raw/home/herd.bin herd -H "cookie: osada=$C" & done; wait
    dl c8-herd-after $FSN /api/raw/home/herd.bin herd -H "cookie: osada=$C"
    ;;
far)
    dl c9-sin-link-cold $SIN /s/$L1/raw big1
    dl c9-sin-link-warm $SIN /s/$L1/raw big1
    ;;
away)
    dl c7-away-link $FSN /s/$L2/raw big2
    curl -s --resolve $H:443:$FSN -o /dev/null -w "c7-away-link-page      %{http_code} %{size_download} bytes\n" https://$H/s/$L2
    C=$(cookie $FSN); echo "c7-away sign-in: ${C:+a session}${C:-no session (home away)}"
    ;;
*) echo "steps: cold-warm link herd far away"; exit 2 ;;
esac
