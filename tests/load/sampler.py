#!/usr/bin/python3 -I
"""swai load test: one line a second until <seconds> pass, into <csv>: the machine's CPU (% of one
core, summed), load, network (Mbit/s in and out on the default route's interface), TCP
connections established, and per program of interest (osada node, osada web agent, stalwart, k6)
its CPU (% of one core) and memory (MB).  sampler.py <seconds> <csv>"""
import os, sys, time

dur, out = int(sys.argv[1]), sys.argv[2]
HZ = os.sysconf("SC_CLK_TCK")
PAGE = os.sysconf("SC_PAGE_SIZE")


def iface():
    for line in open("/proc/net/route").read().splitlines()[1:]:
        f = line.split()
        if f[1] == "00000000":
            return f[0]
    return "lo"


IF = iface()


def kind(pid):
    try:
        comm = open(f"/proc/{pid}/comm").read().strip()
        if comm not in ("osada", "stalwart", "k6"):
            return None
        if comm == "osada":
            cmd = open(f"/proc/{pid}/cmdline").read()
            return "agent" if "web-agent" in cmd else "osada"
        return comm
    except OSError:
        return None


def snap():
    procs = {}
    for p in os.listdir("/proc"):
        if p.isdigit():
            k = kind(p)
            if k:
                try:
                    st = open(f"/proc/{p}/stat").read().rsplit(")", 1)[1].split()
                    procs.setdefault(k, [0, 0])
                    procs[k][0] += int(st[11]) + int(st[12])  # utime + stime
                    procs[k][1] += int(st[21]) * PAGE  # rss
                except (OSError, IndexError):
                    pass
    cpu = [int(x) for x in open("/proc/stat").readline().split()[1:8]]
    busy = sum(cpu) - cpu[3] - cpu[4]
    net = [int(open(f"/sys/class/net/{IF}/statistics/{d}_bytes").read()) for d in ("rx", "tx")]
    estab = 0
    for t in ("/proc/net/tcp", "/proc/net/tcp6"):
        try:
            estab += sum(1 for l in open(t).read().splitlines()[1:] if l.split()[3] == "01")
        except OSError:
            pass
    return time.time(), busy, procs, net, estab


KINDS = ["osada", "agent", "stalwart", "k6"]
with open(out, "w", buffering=1) as f:
    f.write("t,cpu,load1,rx_mbit,tx_mbit,estab," + ",".join(f"{k}_cpu,{k}_mb" for k in KINDS) + "\n")
    a = snap()
    end = time.time() + dur
    while time.time() < end:
        time.sleep(1)
        b = snap()
        dt = b[0] - a[0]
        row = [f"{b[0]:.0f}", f"{100 * (b[1] - a[1]) / HZ / dt:.0f}", open("/proc/loadavg").read().split()[0],
               f"{(b[3][0] - a[3][0]) * 8 / 1e6 / dt:.2f}", f"{(b[3][1] - a[3][1]) * 8 / 1e6 / dt:.2f}", str(b[4])]
        for k in KINDS:
            if k in b[2]:
                c = 100 * (b[2][k][0] - a[2].get(k, b[2][k])[0]) / HZ / dt
                row += [f"{c:.0f}", f"{b[2][k][1] / 1e6:.0f}"]
            else:
                row += ["", ""]
        f.write(",".join(row) + "\n")
        a = b
