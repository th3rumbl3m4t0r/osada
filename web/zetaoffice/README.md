# office: LibreOffice in the browser (osada, Round 18)

A site of osada's web module at `/web/zetaoffice/` on osada's own names, marked as the office
suite: osada's file view offers "open in office" for documents, spreadsheets, presentations, rtf
and csv, and opens them here (`?loc=…&path=…`). What LibreOffice saves goes back to osada (with
If-Match: a file changed in osada meanwhile is replaced only when you say so, else this version is
kept next to it); saved in another format, it becomes a new file next to the one opened.

- `soffice.js`, `soffice.wasm`, `soffice.data`, `soffice.data.js.metadata`: ZetaOffice
  (allotropia), LibreOffice compiled to WebAssembly, from https://cdn.zetaoffice.net/zetaoffice_latest/;
  LibreOffice is MPL-2.0 (source: https://git.libreoffice.org/core). `.br` and `.gz` copies are
  sent to browsers that take them (about 52 MB with brotli; kept by the browser after).
- `zeta.js`: zetajs (MIT, https://github.com/allotropia/zetajs, `zetajs-LICENSE`).
- `index.html`, `office.js`, `office_thread.js`: osada's page, derived from zetajs's
  `examples/web-office` (MIT).

The site needs these headers (its settings in admin → web): `Cross-Origin-Opener-Policy:
same-origin`, `Cross-Origin-Embedder-Policy: require-corp` (threads: SharedArrayBuffer).
A tab uses 1-2 GB of memory; one person edits a copy at a time (no co-editing).
Made by `deploy/zetaoffice-site.sh <folder> [owner]`.
