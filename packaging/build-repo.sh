#!/bin/bash
# osada's packages and their repository (repo.th3rumbl3.com): a static site with dnf repositories
# for EL9, EL10 (Rocky, Alma, RHEL, CentOS Stream) and Fedora, an apt repository for Debian and
# Ubuntu, the key that signs them, a tarball for other systems and a page that says how to
# install. On a build host with podman, createrepo_c, rpmsign and gpg, from the source tree:
#
#   OSADA_SIGN_KEY=<fingerprint> GNUPGHOME=<its keyring> STALWART_SHA256=<sha256> \
#       [OSADA_RELEASE=2] packaging/build-repo.sh <out>
#
# <out>/site is the site (packages of earlier builds stay in it); <out>/cache keeps cargo's
# downloads, the build and Stalwart's release between runs. The program is built once, in a
# Rocky 9 container (glibc 2.34: the same program runs on all of them); the RPMs in Rocky 9,
# Rocky 10 and Fedora containers, the .debs in a Debian 12 one. Stalwart is upstream's release,
# checked against STALWART_SHA256.
set -euo pipefail
SRC=$(cd "$(dirname "$0")/.." && pwd)
OUT=$(realpath -m "${1:?the output folder}")
KEY=${OSADA_SIGN_KEY:?OSADA_SIGN_KEY: fingerprint of the signing key (in GNUPGHOME)}
VER=$(sed -n 's/^version = "\(.*\)"/\1/p' "$SRC/Cargo.toml" | head -1)
REL=${OSADA_RELEASE:-1}
STALWART=${STALWART_VERSION:-0.16.27}
STALWART_SHA256=${STALWART_SHA256:?STALWART_SHA256: sha256 of the release tarball}
SITE_URL=${OSADA_REPO_URL:-https://repo.th3rumbl3.com}
FEDORA=${OSADA_FEDORA:-44}
IMG9=docker.io/rockylinux/rockylinux:9
IMG10=docker.io/rockylinux/rockylinux:10
IMGDEB=docker.io/library/debian:12
CACHE=$OUT/cache
WORK=$OUT/work
mkdir -p "$CACHE/cargo" "$CACHE/target" "$CACHE/dl" "$OUT/site"
rm -rf "$WORK"; mkdir -p "$WORK"

echo "== osada $VER-$REL: the program (Rocky 9)"
podman run --rm -v "$SRC:/src:ro,Z" -v "$CACHE/cargo:/cargo:Z" -v "$CACHE/target:/target:Z" \
    -e CARGO_HOME=/cargo -e CARGO_TARGET_DIR=/target "$IMG9" bash -euc '
    dnf -q -y install rust cargo gcc make >/dev/null
    cd /src && cargo build --release --locked 2>&1 | tail -2
    /target/release/osada version | head -1'
BIN=$CACHE/target/release/osada
NEED=$(objdump -T "$BIN" | grep -oE 'GLIBC_[0-9.]+' | sed 's/GLIBC_//' | sort -V | tail -1)
echo "   needs glibc $NEED at most"

echo "== Stalwart $STALWART (upstream's release)"
T=$CACHE/dl/stalwart-$STALWART-x86_64-unknown-linux-gnu.tar.gz
if [ ! -s "$T" ]; then
    curl -fsSL --max-time 600 -o "$T.part" "https://github.com/stalwartlabs/stalwart/releases/download/v$STALWART/stalwart-x86_64-unknown-linux-gnu.tar.gz"
    mv "$T.part" "$T"
fi
echo "$STALWART_SHA256  $T" | sha256sum -c --quiet - || { echo "Stalwart's tarball is not the one expected" >&2; exit 1; }
cp "$T" "$WORK/stalwart-x86_64-unknown-linux-gnu.tar.gz"

echo "== the sources of the packages and the tarball"
S=$WORK/osada-$VER
mkdir -p "$S"
install -m 755 "$BIN" "$S/osada"
install -m 755 "$SRC/packaging/osada-setup" "$S/osada-setup"
install -m 644 "$SRC/packaging/INSTALL.md" "$SRC/README.md" "$SRC/LICENSE" "$S/"
# the webmail osada-setup mail installs in Stalwart (maildot's build, maildot-webmail-<version>-<hash>.zip)
WEBMAIL=${OSADA_WEBMAIL:-$(ls "$CACHE"/dl/maildot-webmail-*.zip 2>/dev/null | sort -V | tail -1)}
[ -s "$WEBMAIL" ] || { echo "OSADA_WEBMAIL: maildot's webmail zip (or one in $CACHE/dl)" >&2; exit 1; }
install -m 644 "$WEBMAIL" "$S/"
cat > "$S/mail.txt" <<EOM
osada-mail: Stalwart (the mail server) for an osada node. Set it up with

    osada-setup mail --firewall

then admin -> modules (a module of kind mail on this node) and admin -> mail. See
/usr/share/doc/osada/INSTALL.md, "Mail".
EOM
tar -C "$WORK" --owner=0 --group=0 -czf "$WORK/osada-$VER.tar.gz" "osada-$VER"
TB=osada-$VER-$REL-linux-x86_64
mkdir -p "$WORK/$TB"
cp -a "$S/osada" "$S/osada-setup" "$S/INSTALL.md" "$S/README.md" "$S/LICENSE" "$S"/maildot-webmail-*.zip "$WORK/$TB/"
(cd "$WORK/$TB" && sha256sum -- * > SHA256SUMS)
tar -C "$WORK" --owner=0 --group=0 -czf "$WORK/$TB.tar.gz" "$TB"

# the RPMs: <folder in work> <image>
rpms() {
    local dir=$1 img=$2 sw=0
    [ "$dir" = el9 ] && sw=1
    echo "== the RPMs ($dir: $img)"
    podman run --rm -v "$SRC/packaging/rpm:/spec:ro,Z" -v "$WORK:/work:Z" "$img" bash -euc "
        dnf -q -y install rpm-build systemd-rpm-macros >/dev/null
        mkdir -p /tmp/rb/SOURCES /work/$dir
        cp /work/osada-$VER.tar.gz /work/stalwart-x86_64-unknown-linux-gnu.tar.gz /tmp/rb/SOURCES/
        rpmbuild -bb --quiet --define '_topdir /tmp/rb' --define 'ver $VER' --define 'rel $REL' /spec/osada.spec
        if [ $sw = 1 ]; then rpmbuild -bb --quiet --define '_topdir /tmp/rb' --define 'ver $STALWART' /spec/stalwart.spec; fi
        cp /tmp/rb/RPMS/x86_64/*.rpm /work/$dir/"
}
rpms el9 "$IMG9"
rpms el10 "$IMG10"
rpms "fc$FEDORA" "registry.fedoraproject.org/fedora:$FEDORA"
# one Stalwart package for all of them
cp "$WORK"/el9/stalwart-*.rpm "$WORK/el10/"
cp "$WORK"/el9/stalwart-*.rpm "$WORK/fc$FEDORA/"

echo "== the .debs (Debian 12)"
podman run --rm -v "$SRC/packaging/deb:/deb:ro,Z" -v "$WORK:/work:Z" "$IMGDEB" \
    bash /deb/build-debs.sh /work "$VER" "$REL" "$STALWART"

echo "== signed"
rpmsign --define "_gpg_name $KEY" --define "_gpg_digest_algo sha256" --addsign "$WORK"/el9/*.rpm "$WORK"/el10/*.rpm "$WORK/fc$FEDORA"/*.rpm >/dev/null
for r in "$WORK"/el9/*.rpm "$WORK"/el10/*.rpm "$WORK/fc$FEDORA"/*.rpm; do rpm -Kv --nosignature "$r" >/dev/null; done
# a .deb is not signed itself: apt checks the repository's signed Release (InRelease)

echo "== the site"
SITE=$OUT/site
# a dnf repository: <its folder in the site> <the RPMs>
dnfrepo() {
    mkdir -p "$SITE/$1"
    cp "$2"/*.rpm "$SITE/$1/"
    createrepo_c -q --update "$SITE/$1"
    rm -f "$SITE/$1/repodata/repomd.xml.asc"
    gpg --batch --yes -u "$KEY" --detach-sign --armor "$SITE/$1/repodata/repomd.xml"
}
dnfrepo el9/x86_64 "$WORK/el9"
dnfrepo el10/x86_64 "$WORK/el10"
dnfrepo "fedora/$FEDORA/x86_64" "$WORK/fc$FEDORA"

# the apt repository: one suite, stable, for every Debian and Ubuntu
D=$SITE/deb
mkdir -p "$D/pool/main/o/osada" "$D/pool/main/s/stalwart"
cp "$WORK"/deb/osada*.deb "$D/pool/main/o/osada/"
cp "$WORK"/deb/stalwart_*.deb "$D/pool/main/s/stalwart/"
rm -f "$D/dists/stable/Release" "$D/dists/stable/InRelease" "$D/dists/stable/Release.gpg"
podman run --rm -v "$D:/repo:Z" "$IMGDEB" bash -euc '
    apt-get -qq update && apt-get -qq install -y apt-utils xz-utils >/dev/null
    cd /repo
    mkdir -p dists/stable/main/binary-amd64
    apt-ftparchive packages pool > dists/stable/main/binary-amd64/Packages
    gzip -9nkf dists/stable/main/binary-amd64/Packages
    xz -kf dists/stable/main/binary-amd64/Packages
    apt-ftparchive -o APT::FTPArchive::Release::Origin=osada -o APT::FTPArchive::Release::Label=osada \
        -o APT::FTPArchive::Release::Suite=stable -o APT::FTPArchive::Release::Codename=stable \
        -o APT::FTPArchive::Release::Architectures=amd64 -o APT::FTPArchive::Release::Components=main \
        -o "APT::FTPArchive::Release::Description=osada for Debian and Ubuntu" \
        release dists/stable > /tmp/Release
    mv /tmp/Release dists/stable/Release'
gpg --batch --yes -u "$KEY" --digest-algo SHA512 --clearsign -o "$D/dists/stable/InRelease" "$D/dists/stable/Release"
gpg --batch --yes -u "$KEY" --digest-algo SHA512 --detach-sign --armor -o "$D/dists/stable/Release.gpg" "$D/dists/stable/Release"

mkdir -p "$SITE/tarballs"
cp "$WORK/$TB.tar.gz" "$SITE/tarballs/"
gpg --batch --yes -u "$KEY" --detach-sign --armor -o "$SITE/tarballs/$TB.tar.gz.asc" "$WORK/$TB.tar.gz"
gpg --batch --export --armor "$KEY" > "$SITE/RPM-GPG-KEY-osada"
gpg --batch --export "$KEY" > "$SITE/osada.gpg"
cp "$SRC/packaging/INSTALL.md" "$SITE/INSTALL.md"
cat > "$SITE/osada.repo" <<EOR
[osada]
name=osada for EL\$releasever (\$basearch)
baseurl=$SITE_URL/el\$releasever/\$basearch/
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=$SITE_URL/RPM-GPG-KEY-osada
EOR
cat > "$SITE/osada-fedora.repo" <<EOR
[osada]
name=osada for Fedora \$releasever (\$basearch)
baseurl=$SITE_URL/fedora/\$releasever/\$basearch/
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=$SITE_URL/RPM-GPG-KEY-osada
EOR
cat > "$SITE/osada.sources" <<EOR
Types: deb
URIs: $SITE_URL/deb
Suites: stable
Components: main
Architectures: amd64
Signed-By: /usr/share/keyrings/osada.gpg
EOR
FPR=$(gpg --batch --with-colons --fingerprint "$KEY" | awk -F: '/^fpr/ {print $10; exit}' | sed 's/.\{4\}/& /g;s/ $//')
python3 -I "$SRC/packaging/repo-page.py" "$SITE" "$SITE_URL" "$FPR" > "$SITE/index.html.new"
mv "$SITE/index.html.new" "$SITE/index.html"
(cd "$SITE" && find . -type f ! -name SHA256SUMS ! -name index.html -printf '%P\n' | sort | xargs sha256sum > SHA256SUMS)
du -sh "$SITE"; find "$SITE" \( -name '*.rpm' -o -name '*.deb' \) -printf '%P %s\n' | sort
