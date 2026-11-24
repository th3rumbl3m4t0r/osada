#!/bin/bash
# swai: one k6 run and its line.  run.sh <label> <mode> <rate> <dur> [edge address]
# (password of the test account in USER_PW; results also in /root/load/results.csv)
set -u
label=$1 mode=$2 rate=$3 dur=$4 edge=${5:-}
out=/root/load/out-$label.json
t0=$(date +%s)
ulimit -n 65536
USER_NAME=${USER_NAME:-alice} MODE=$mode RATE=$rate DUR=$dur EDGE=$edge OUT=$out \
    k6 run --quiet --no-color /root/load/osada.js >/root/load/log-$label.txt 2>&1
rc=$?
t1=$(date +%s)
python3 -I - "$label" "$mode" "$rate" "$edge" "$t0" "$t1" "$out" "$rc" <<'EOF'
import json, sys
label, mode, rate, edge, t0, t1, out, rc = sys.argv[1:]
try:
    m = json.load(open(out))
except Exception as e:
    print(f"{label}: no summary ({e}); k6 exit {rc}"); sys.exit()
v = lambda k: m.get(k, {}).get("values", {})
d, w, tls = v("http_req_duration"), v("http_req_waiting"), v("http_req_tls_handshaking")
reqs, fail, drop = v("http_reqs"), v("http_req_failed"), v("dropped_iterations")
line = (f"{label:<16} {mode:<8} {edge or 'dns':<13} want {rate:>4}/s got {reqs.get('rate', 0):7.1f}/s "
        f"p50 {d.get('med', 0):7.1f} p90 {d.get('p(90)', 0):7.1f} p95 {d.get('p(95)', 0):7.1f} p99 {d.get('p(99)', 0):7.1f} "
        f"max {d.get('max', 0):7.0f} ms  ttfb50 {w.get('med', 0):6.1f}  tls {tls.get('avg', 0):5.1f}  "
        f"fail {100 * fail.get('rate', 0):5.2f}%  dropped {int(drop.get('count', 0))}  [{t0}-{t1}]")
print(line)
open("/root/load/results.csv", "a").write(",".join([label, mode, edge or "dns", rate, f"{reqs.get('rate', 0):.1f}",
    *[f"{d.get(k, 0):.1f}" for k in ("med", "p(90)", "p(95)", "p(99)", "max")], f"{w.get('med', 0):.1f}",
    f"{tls.get('avg', 0):.1f}", f"{100 * fail.get('rate', 0):.2f}", str(int(drop.get('count', 0))), t0, t1]) + "\n")
EOF
