#!/usr/bin/python3 -I
"""swai load test: the sampler's CSV summed up per window.  window.py <csv> <label:t0:t1>...
Per window: machine CPU (% of one core) avg/max, each program's CPU avg/max and memory max (MB),
network in/out avg (Mbit/s), TCP established max."""
import csv, sys

rows = list(csv.DictReader(open(sys.argv[1])))
f = lambda r, k: float(r[k]) if r.get(k) not in (None, "") else None
for w in sys.argv[2:]:
    label, t0, t1 = w.split(":")
    rs = [r for r in rows if int(t0) <= int(r["t"]) <= int(t1)]
    if not rs:
        print(f"{label}: no samples"); continue
    def st(k):
        v = [x for x in (f(r, k) for r in rs) if x is not None]
        return (sum(v) / len(v), max(v)) if v else None
    out = [f"{label:<14}"]
    c = st("cpu"); out.append(f"cpu {c[0]:4.0f}/{c[1]:4.0f}%")
    for k in ("osada", "agent", "stalwart", "k6"):
        c, m = st(f"{k}_cpu"), st(f"{k}_mb")
        if c:
            out.append(f"{k} {c[0]:4.0f}/{c[1]:4.0f}% {m[1]:4.0f}MB")
    rx, tx, es = st("rx_mbit"), st("tx_mbit"), st("estab")
    out.append(f"net in {rx[0]:6.2f} out {tx[0]:6.2f} Mbit/s  tcp {es[1]:.0f}")
    print("  ".join(out))
