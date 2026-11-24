#!/bin/sh
# Builds and loads the osada SELinux module and labels osada's files. As root:
# deploy/selinux/install.sh
set -eu
cd "$(dirname "$0")"
make -f /usr/share/selinux/devel/Makefile osada.pp
semodule -i osada.pp
# Round 18: containers with osada folders, where podman is
if rpm -q container-selinux >/dev/null 2>&1; then
    make -f /usr/share/selinux/devel/Makefile osada-containers.pp
    semodule -i osada-containers.pp
fi
# a local rule, as file_contexts.homedirs would win over one in osada.fc
semanage fcontext -l -C | grep -q '^/home/osada(/\.\*)?' ||
    semanage fcontext -a -t osada_data_t '/home/osada(/.*)?'
# osada 0.2: the node links' UDP port
semanage port -l -C | grep -q '^osada_link_port_t' ||
    semanage port -a -t osada_link_port_t -p udp 7441
[ -e /opt/osada-0.2/osada ] && restorecon -F /opt/osada-0.2/osada
restorecon -RF /opt/osada/target/release /opt/osada/deploy /opt/osada/android/serve.py \
    /etc/osada /srv/osada-apk /home/osada
