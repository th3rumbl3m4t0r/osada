#!/bin/bash
# Round 18b: jobs (a container on a schedule that writes named files into a site), on a drop-in
# home with its root agent (real podman), then tests/jobs.py. As root, on a host with
# podman and the SELinux module osada-containers (deploy/selinux/install.sh):
#
#   tests/jobs.sh target/release/osada
#
# Its agent leaves other agents' containers alone (label shelf.data), production's too.
set -u
D=/root/t-jobs; B=$(realpath "${1:-/root/osada-0.2/target/release/osada}"); HTTP=18997
HERE=$(dirname "$(realpath "$0")")
podman rm -f osada-job-jt osada-job-jfail osada-job-jslow >/dev/null 2>&1
rm -rf $D; mkdir -p $D/home
# labelled as production's /home/osada (deploy/selinux/install.sh: its files, tmp/ too, are
# osada_data_t), so the policy is what is tried
chcon -t osada_data_t $D/home
OSADA_DATA=$D/home OSADA_BIND=127.0.0.1:$HTTP OSADA_LINK_BIND=127.0.0.1:18996 OSADA_NODE=home $B serve >$D/home.log 2>&1 & HP=$!
for i in $(seq 60); do [ -f $D/home/node/first-admin ] && break; sleep 1; done
$B web-agent --data $D/home/node >$D/agent.log 2>&1 & AP=$!
PW=$(sed -n 's/.*password: *//p' $D/home/node/first-admin)
python3 -I "$HERE/jobs.py" http://127.0.0.1:$HTTP admin "$PW" $D; R=$?
kill $HP; sleep 6; kill $AP; wait 2>/dev/null
podman rm -f osada-job-jt osada-job-jfail osada-job-jslow >/dev/null 2>&1
[ $R = 0 ] && rm -rf $D || echo "logs in $D"
exit $R
