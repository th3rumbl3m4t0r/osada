#!/bin/bash
# Round 19: egress. A drop-in home with its root agent (real podman) in this host's network, and
# a second node, "exit", in a network namespace of its own (eg-x, 10.78.0.2; home dials it, so
# nothing comes in to this host) with an echo service there (10.78.0.2:18995, and the name
# echo.test that only eg-x knows: /etc/netns/eg-x/hosts). Then tests/egress.py. As root, on a host
# with podman and the SELinux module osada-containers:
#
#   tests/egress.sh target/release/osada
#
# Its agent leaves other agents' containers alone (label shelf.data), production's too.
set -u
D=/root/t-egress; B=$(realpath "${1:-/root/osada-0.2/target/release/osada}"); HTTP=18993
HERE=$(dirname "$(realpath "$0")")
X=eg-x
cleanup() {
	for p in $(ip netns pids $X 2>/dev/null); do kill "$p" 2>/dev/null; done
	sleep 1
	ip netns del $X 2>/dev/null
	ip link del eg-vh 2>/dev/null
	rm -rf /etc/netns/$X
	podman rm -f osada-job-eg osada-job-egdirect osada-web-egweb >/dev/null 2>&1
}
cleanup
rm -rf $D; mkdir -p $D/home $D/exit
ip netns add $X
ip link add eg-vh type veth peer name eg-vx
ip link set eg-vx netns $X
ip addr add 10.78.0.1/24 dev eg-vh && ip link set eg-vh up
ip -n $X addr add 10.78.0.2/24 dev eg-vx && ip -n $X link set eg-vx up && ip -n $X link set lo up
mkdir -p /etc/netns/$X && { cat /etc/hosts; echo "10.78.0.2 echo.test"; } > /etc/netns/$X/hosts
# the echo service: who connected, what was asked
cat > $D/echo.py <<'EOF'
import http.server, json
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        b = json.dumps({"client": self.client_address[0], "path": self.path, "host": self.headers.get("host"), "proxy_headers": [k for k in self.headers if k.lower().startswith("proxy-")]}).encode()
        self.send_response(200); self.send_header("content-type", "application/json"); self.send_header("content-length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def log_message(self, *a): pass
http.server.ThreadingHTTPServer(("10.78.0.2", 18995), H).serve_forever()
EOF
ip netns exec $X python3 -I $D/echo.py > $D/echo.log 2>&1 &
# labelled as production's /home/osada, so the policy is what is tried
chcon -t osada_data_t $D/home
OSADA_DATA=$D/home OSADA_BIND=127.0.0.1:$HTTP OSADA_LINK_BIND=10.78.0.1:18994 OSADA_NODE=home $B serve >$D/home.log 2>&1 & HP=$!
for i in $(seq 60); do [ -f $D/home/node/first-admin ] && break; sleep 1; done
$B web-agent --data $D/home/node >$D/agent.log 2>&1 & AP=$!
tok=$($B token --data $D/home/node --node exit --roles worker --dial 10.78.0.2:47402)
timeout 90 ip netns exec $X $B join --data $D/exit --node exit --token "$tok" --listen 10.78.0.2:47402 --wait 2 > $D/join.log 2>&1 || { echo "exit did not join"; cat $D/join.log; }
ip netns exec $X $B serve --data $D/exit --listen 10.78.0.2:47402 > $D/exit.log 2>&1 & XP=$!
PW=$(sed -n 's/.*password: *//p' $D/home/node/first-admin)
python3 -I "$HERE/egress.py" http://127.0.0.1:$HTTP admin "$PW" $D; R=$?
kill $HP $XP
sleep 6; kill $AP; wait $HP $XP $AP 2>/dev/null
cleanup
[ $R = 0 ] && rm -rf $D || echo "logs in $D"
exit $R
