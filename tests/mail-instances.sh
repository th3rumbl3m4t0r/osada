#!/bin/bash
# Two mail instances (Round 18): two real Stalwarts 0.16, each with a node of a fresh cluster of
# this build, in network namespaces of their own (each Stalwart has 127.0.0.1:8080 to itself),
# joined by a veth pair; then tests/mail_instances.py (the API and the mail tab in headless
# Chromium) from the first. As root, with $T/dl/stalwart and $T/cred as tests/stalwart-ns.sh:
#
#   T=/root/t-sw tests/mail-instances.sh target/release/osada
set -u
T=${T:-/root/t-sw}
W=$T/mi
BIN=$(realpath "$1")
HERE=$(dirname "$(realpath "$0")")
A=mi-a; B=mi-b
cleanup() {
	for p in $(ip netns pids $A 2>/dev/null) $(ip netns pids $B 2>/dev/null); do kill "$p" 2>/dev/null; done
	sleep 1
	ip netns del $A 2>/dev/null; ip netns del $B 2>/dev/null
}
cleanup
trap cleanup EXIT
rm -rf "$W" && mkdir -p "$W/a/data" "$W/b/data"
ip netns add $A && ip netns add $B
ip link add mi-va type veth peer name mi-vb
ip link set mi-va netns $A && ip link set mi-vb netns $B
ip -n $A addr add 10.77.0.1/24 dev mi-va && ip -n $B addr add 10.77.0.2/24 dev mi-vb
for n in $A $B; do ip -n $n link set lo up; done
ip -n $A link set mi-va up && ip -n $B link set mi-vb up
for x in a b; do
	printf '{"@type":"RocksDb","path":"%s/%s/data"}\n' "$W" "$x" > "$W/$x/config.json"
	ns=mi-$x
	STALWART_RECOVERY_ADMIN="$(cat "$T/cred")" ip netns exec $ns "$T/dl/stalwart" --config="$W/$x/config.json" > "$W/$x/stalwart.log" 2>&1 &
done
for ns in $A $B; do
	for _ in $(seq 1 60); do ip netns exec $ns curl -s -o /dev/null http://127.0.0.1:8080/ && break; sleep 0.5; done
done
"$BIN" init --data "$W/a/node" --node mail-a > /dev/null
ip netns exec $A "$BIN" serve --data "$W/a/node" --listen 10.77.0.1:47301 --http 127.0.0.1:18301 > "$W/a/node.log" 2>&1 &
for _ in $(seq 1 100); do [ -s "$W/a/node/first-admin" ] && break; sleep 0.2; done
tok=$("$BIN" token --data "$W/a/node" --node mail-b --roles edge-mail,store --dial 10.77.0.2:47302)
timeout 90 ip netns exec $B "$BIN" join --data "$W/b/node" --node mail-b --token "$tok" --listen 10.77.0.2:47302 --wait 2 > "$W/b/join.log" 2>&1 || { echo "mail-b did not join"; cat "$W/b/join.log"; exit 1; }
ip netns exec $B "$BIN" serve --data "$W/b/node" --listen 10.77.0.2:47302 > "$W/b/node.log" 2>&1 &
for x in a b; do
	mkdir -p -m 700 "$W/$x/node/mail"
	(umask 077; printf 'Basic %s\n' "$(base64 -w0 < "$T/cred")" > "$W/$x/node/mail/stalwart.auth")
done
ip netns exec $A python3 -I "$HERE/mail_instances.py" "$W" http://127.0.0.1:18301
