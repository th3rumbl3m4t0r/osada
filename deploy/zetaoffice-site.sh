#!/bin/sh
# The office suite's site (Round 18): ZetaOffice's files (LibreOffice compiled to WebAssembly, as
# its CDN serves them: soffice.wasm and soffice.data brotli only) with plain and gzip copies next
# to them, and osada's page (web/zetaoffice/: index.html, office.js, office_thread.js, zeta.js).
#
#   deploy/zetaoffice-site.sh <the site's folder> [owner]
#   e.g. deploy/zetaoffice-site.sh /home/osada/node/web/zetaoffice osada
#
# Then the site in node control (admin → web → add a site, or `osada web import`):
#   {"zetaoffice": {"title": "office (LibreOffice in the browser)", "node": "<the node>",
#    "site": {"names": [], "under_web": true, "office": true, "content": {"kind": "static"},
#             "headers": {"Cross-Origin-Opener-Policy": "same-origin",
#                         "Cross-Origin-Embedder-Policy": "require-corp"}}}}
# Needs curl, brotli and gzip. ZETA_BASE: another place to take the files from.
set -eu
to=${1:?the site folder}
owner=${2:-}
here=$(cd "$(dirname "$0")/.." && pwd)
base=${ZETA_BASE:-https://cdn.zetaoffice.net/zetaoffice_latest}
tmp=$(mktemp -d "${TMPDIR:-/var/tmp}/zetaoffice.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
for f in soffice.js soffice.wasm soffice.data; do
	curl -sS --fail --max-time 900 -H 'accept-encoding: br' -o "$f.br" "$base/$f"
	brotli -t "$f.br"
	brotli -d -c "$f.br" > "$f"
	gzip -9 -k -n "$f"
done
curl -sS --fail --max-time 120 -o soffice.data.js.metadata "$base/soffice.data.js.metadata"
head -c 4 soffice.wasm | od -A n -t x1 | grep -q '00 61 73 6d' || { echo "soffice.wasm is not WebAssembly" >&2; exit 1; }
cp "$here"/web/zetaoffice/index.html "$here"/web/zetaoffice/office.js "$here"/web/zetaoffice/office_thread.js \
	"$here"/web/zetaoffice/zeta.js "$here"/web/zetaoffice/zetajs-LICENSE "$here"/web/zetaoffice/README.md .
sha256sum soffice.* > SHA256SUMS
mkdir -p "$to"
for f in *; do
	install -m 0644 "$f" "$to/.$f.new"
	mv -f "$to/.$f.new" "$to/$f"
done
[ -n "$owner" ] && chown -R "$owner:$owner" "$to"
command -v restorecon >/dev/null && restorecon -R "$to" || true
du -sh "$to"
