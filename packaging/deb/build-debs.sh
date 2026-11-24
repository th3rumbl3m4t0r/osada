#!/bin/bash
# osada's .deb packages (Debian 12+, Ubuntu 22.04+), built by build-repo.sh in a Debian container:
#
#   build-debs.sh <work folder> <version> <release> <Stalwart's version>
#
# <work>/osada-<version>/ has the program, osada-setup and the docs (the same as the RPMs get),
# <work>/stalwart-x86_64-unknown-linux-gnu.tar.gz is Stalwart's release; the packages go to
# <work>/deb. One set for every Debian and Ubuntu: the program needs glibc 2.34 at most.
set -euo pipefail
W=$1 VER=$2 REL=$3 SV=$4
S=$W/osada-$VER
OUT=$W/deb
B=$(mktemp -d)
mkdir -p "$OUT"
MAINT="osada <packages@repo.th3rumbl3.com>"
UNITS="osada.service osada-web-agent.service osada-node.service osada-node-web-agent.service"
size() { du -sk "$1" | cut -f1; }
script() { # package folder, name, content
    printf '%s' "$3" > "$1/DEBIAN/$2"
    chmod 755 "$1/DEBIAN/$2"
}

# ---- osada: the program and osada-setup
P=$B/osada
install -Dm755 "$S/osada" "$P/usr/bin/osada"
install -Dm755 "$S/osada-setup" "$P/usr/sbin/osada-setup"
install -Dm644 "$S/INSTALL.md" "$P/usr/share/doc/osada/INSTALL.md"
install -Dm644 "$S/README.md" "$P/usr/share/doc/osada/README.md"
install -Dm644 "$S/LICENSE" "$P/usr/share/doc/osada/copyright"
n=$(size "$P")
mkdir -p "$P/DEBIAN"
cat > "$P/DEBIAN/control" <<EOF
Package: osada
Version: $VER-$REL
Architecture: amd64
Maintainer: $MAINT
Installed-Size: $n
Depends: libc6 (>= 2.34), libgcc-s1, systemd, passwd, util-linux, iproute2, hostname, curl, ca-certificates
Suggests: podman, ufw | firewalld
Section: net
Priority: optional
Homepage: https://repo.th3rumbl3.com/
Description: self-hosted files, wiki, sign-in and groupware
 osada: files (with versions, sharing, public links, sync for the apps), a wiki,
 sign-in for other programs (OpenID Connect), chat, and mail through Stalwart, on
 one machine or spread over several (a first node that keeps the data, edges that
 answer https and relay to it). osada-setup makes a node: the first one of a new
 cluster, an edge, another node, certificates, mail.
EOF
# as dnf does: an upgrade restarts the services that run; a removal stops them
script "$P" postinst "#!/bin/sh
set -e
if [ \"\$1\" = configure ] && [ -n \"\$2\" ] && [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || true
    for u in $UNITS; do systemctl try-restart \"\$u\" >/dev/null 2>&1 || true; done
fi
exit 0
"
script "$P" prerm "#!/bin/sh
set -e
if [ \"\$1\" = remove ] && [ -d /run/systemd/system ]; then
    for u in $UNITS; do systemctl disable --now \"\$u\" >/dev/null 2>&1 || true; done
fi
exit 0
"
dpkg-deb --root-owner-group -Zxz --build "$P" "$OUT/osada_${VER}-${REL}_amd64.deb" >/dev/null

# ---- osada-mail: Stalwart and what osada-setup mail needs
P=$B/osada-mail
mkdir -p "$P/usr/share/doc/osada-mail"
sed 's/dnf/apt/g' "$S/mail.txt" > "$P/usr/share/doc/osada-mail/README"
install -m644 "$S/LICENSE" "$P/usr/share/doc/osada-mail/copyright"
install -Dm644 -t "$P/usr/share/osada" "$S"/maildot-webmail-*.zip
n=$(size "$P")
mkdir -p "$P/DEBIAN"
cat > "$P/DEBIAN/control" <<EOF
Package: osada-mail
Version: $VER-$REL
Architecture: all
Maintainer: $MAINT
Installed-Size: $n
Depends: osada (= $VER-$REL), stalwart (>= 0.16), python3
Section: mail
Priority: optional
Homepage: https://repo.th3rumbl3.com/
Description: osada's mail: Stalwart, the mail server, for osada-setup mail
 Pulls in Stalwart (the mail and groupware server), the webmail (maildot) and
 what osada-setup mail needs to set it all up behind an osada node: the name,
 the domain, Let's Encrypt, DKIM, the webmail signing in with osada and the
 mail instance in the cluster; osada then keeps the mailboxes.
EOF
dpkg-deb --root-owner-group -Zxz --build "$P" "$OUT/osada-mail_${VER}-${REL}_all.deb" >/dev/null

# ---- stalwart: upstream's release, unchanged, into /opt/stalwart (as the RPM)
P=$B/stalwart
mkdir -p "$B/sw" "$P/opt/stalwart/data" "$P/opt/stalwart/etc" "$P/opt/stalwart/logs" "$P/usr/share/doc/stalwart"
tar xzf "$W/stalwart-x86_64-unknown-linux-gnu.tar.gz" -C "$B/sw"
install -Dm755 "$B/sw/stalwart" "$P/opt/stalwart/bin/stalwart"
cat > "$P/usr/share/doc/stalwart/copyright" <<EOF
Stalwart $SV, as released by Stalwart Labs (https://stalw.art): the x86_64 gnu build of
https://github.com/stalwartlabs/stalwart/releases/tag/v$SV, unchanged.
License: AGPL-3.0-only (sources: https://github.com/stalwartlabs/stalwart/tree/v$SV).
Packaged for osada (https://repo.th3rumbl3.com/).
EOF
n=$(size "$P")
mkdir -p "$P/DEBIAN"
cat > "$P/DEBIAN/control" <<EOF
Package: stalwart
Version: $SV-1osada
Architecture: amd64
Maintainer: $MAINT
Installed-Size: $n
Pre-Depends: passwd
Depends: libc6 (>= 2.28)
Section: mail
Priority: optional
Homepage: https://stalw.art
Description: Stalwart, the mail and groupware server
 Stalwart (JMAP, IMAP, SMTP, CalDAV, CardDAV, WebDAV), an all-in-one mail and
 groupware server written in Rust, as its authors release it (the program
 unchanged), in /opt/stalwart. osada-setup mail configures it
 (/opt/stalwart/etc) and starts it (stalwart.service).
EOF
script "$P" preinst '#!/bin/sh
set -e
if [ "$1" = install ] || [ "$1" = upgrade ]; then
    getent group stalwart >/dev/null || groupadd -r stalwart
    getent passwd stalwart >/dev/null || useradd -r -g stalwart -d /opt/stalwart -s /usr/sbin/nologin stalwart
fi
exit 0
'
script "$P" postinst '#!/bin/sh
set -e
if [ "$1" = configure ]; then
    chown stalwart:stalwart /opt/stalwart /opt/stalwart/bin /opt/stalwart/bin/stalwart /opt/stalwart/data /opt/stalwart/etc /opt/stalwart/logs
    chmod 750 /opt/stalwart /opt/stalwart/data /opt/stalwart/etc /opt/stalwart/logs
    if [ -n "$2" ] && [ -d /run/systemd/system ]; then systemctl try-restart stalwart.service >/dev/null 2>&1 || true; fi
fi
exit 0
'
dpkg-deb --root-owner-group -Zxz --build "$P" "$OUT/stalwart_${SV}-1osada_amd64.deb" >/dev/null
rm -rf "$B"
for d in "$OUT"/*.deb; do echo "   $(basename "$d") $(du -h "$d" | cut -f1)"; done
