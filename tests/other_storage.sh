#!/bin/bash
# T2/Round 17: a private sshd + a drop-in home, then tests/other_storage.py and tests/ui_files_where.py against them (as root)
set -u
D=/root/t-os; B=${1:-/root/osada-0.2/target/release/osada}; HTTP=18961; SFTP=18962; S3=18963
rm -rf $D; mkdir -p $D/sshd/root/box $D/home; chmod 700 $D/sshd; chmod 755 $D/sshd/root
cp /usr/sbin/sshd /usr/libexec/openssh/sshd-session $D/sshd/
for k in hostkey client; do ssh-keygen -q -t ed25519 -N '' -C $k -f $D/sshd/$k; done
cp $D/sshd/client.pub $D/sshd/authorized_keys; chmod 600 $D/sshd/authorized_keys
cat > $D/sshd/sshd_config <<C
Port $SFTP
ListenAddress 127.0.0.1
HostKey $D/sshd/hostkey
PidFile $D/sshd/pid
AuthorizedKeysFile $D/sshd/authorized_keys
KbdInteractiveAuthentication no
AllowUsers root
UsePAM no
StrictModes no
Subsystem sftp internal-sftp
ForceCommand internal-sftp
ChrootDirectory $D/sshd/root
SshdSessionPath $D/sshd/sshd-session
C
$D/sshd/sshd -f $D/sshd/sshd_config -D -e >$D/sshd.log 2>&1 & SP=$!
OSADA_DATA=$D/home OSADA_BIND=127.0.0.1:$HTTP OSADA_LINK_BIND=127.0.0.1:18960 OSADA_NODE=home $B serve >$D/home.log 2>&1 & HP=$!
for i in $(seq 60); do [ -f $D/home/node/first-admin ] && break; sleep 1; done
PW=$(sed -n 's/.*password: *//p' $D/home/node/first-admin)
python3 -I /root/osada-0.2/tests/other_storage.py http://127.0.0.1:$HTTP admin "$PW" $S3 $SFTP $D/sshd/client $D/sshd/root && python3 -I /root/osada-0.2/tests/ui_files_where.py http://127.0.0.1:$HTTP admin "$PW"; R=$?
kill $HP $SP; wait 2>/dev/null
[ $R = 0 ] && rm -rf $D
exit $R
