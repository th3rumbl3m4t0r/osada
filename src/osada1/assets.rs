//! The web UI, compiled into the binary. Each file is served under its own content hash
//! (`?v=<hash>`), so the browser keeps it for good and fetches only what changed; text
//! files are gzipped once at start. The pages load little up front: viewers and editors
//! are parts the page loads when a file needs them (static/util.js), and of the UI texts
//! only the language in use (texts.rs). The page learns every file's hash from
//! `<meta name="osada-assets">`. The service worker (`/sw.js`, static/sw.js) keeps them, and the
//! pages, for when home does not answer.

use std::{collections::HashMap, io::Write, sync::Arc};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use sha2::{Digest, Sha256};

use crate::osada1::{error::AppError, texts, App};

const CSS: &str = "text/css; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";

macro_rules! asset {
    ($n:literal, $t:expr) => {
        ($n, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/static1/", $n)) as &[u8], $t)
    };
}

/// Everything under /static/ but texts.js, which is served as lang-<code>.js.
static FILES: &[(&str, &[u8], &str)] = &[
    asset!("i18n.js", JS),
    asset!("x11.css", CSS),
    asset!("x11.js", JS),
    asset!("app.css", CSS),
    asset!("util.js", JS),
    asset!("viewer.js", JS),
    asset!("app.js", JS),
    asset!("public.js", JS),
    asset!("favicon.svg", "image/svg+xml"),
    // parts, loaded when a file needs them
    asset!("zip.js", JS),
    asset!("cfb.js", JS),
    asset!("xls.js", JS),
    asset!("xlsx.js", JS),
    asset!("csv.js", JS),
    asset!("csv.css", CSS),
    asset!("ods.js", JS),
    asset!("sheet.js", JS),
    asset!("sheet.css", CSS),
    asset!("rich.js", JS),
    asset!("rich.css", CSS),
    asset!("rtf.js", JS),
    asset!("odt.js", JS),
    asset!("docx.js", JS),
    asset!("msdoc.js", JS),
    asset!("slides.js", JS),
    asset!("slides.css", CSS),
    asset!("odp.js", JS),
    asset!("pptx.js", JS),
    asset!("msppt.js", JS),
    asset!("mail.js", JS),
    asset!("mail.css", CSS),
    asset!("msg.js", JS),
    asset!("pub.js", JS),
    asset!("archive.js", JS),
    asset!("archive.css", CSS),
    asset!("sevenz.js", JS),
    asset!("rar.js", JS),
    asset!("lzma.js", JS),
    asset!("bzip2.js", JS),
    asset!("ppmd.js", JS),
    asset!("img.js", JS),
    asset!("img.css", CSS),
    asset!("epub.js", JS),
    asset!("epub.css", CSS),
    asset!("cal.js", JS),
    asset!("cal.css", CSS),
    asset!("font.js", JS),
    asset!("font.css", CSS),
    asset!("iwork.js", JS),
    asset!("iwork.css", CSS),
    // pages most sessions never open, loaded with the page
    asset!("edit.js", JS),
    asset!("edit.css", CSS),
    asset!("share.js", JS),
    asset!("versions.js", JS),
    asset!("versions.css", CSS),
    asset!("pictures.js", JS),
    asset!("pictures.css", CSS),
    asset!("sftp.js", JS),
    asset!("settings.js", JS),
    asset!("admin.js", JS),
    asset!("wiki.js", JS),
    // 0.2's grid (static/grid.js), for an article's tables (T8)
    ("grid.js", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/static/grid.js")) as &[u8], JS),
    asset!("wiki.css", CSS),
    asset!("tree.js", JS),
    asset!("tree.css", CSS),
];

const TEXTS: &str = include_str!("../../static1/texts.js");
const INDEX: &str = include_str!("../../static1/index.html");
const PUBLIC: &str = include_str!("../../static1/public.html");
const SW: &[u8] = include_bytes!("../../static1/sw.js");

/// Pages only load our own scripts; styles may be inline (editors set them). Frames may
/// show, and the viewer read, a blob: (a file taken out of an archive or document).
/// (`{frames}`: other sites a page may show in a frame: apptab's)
const CSP_HTML: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data: blob:; media-src 'self' blob:; frame-src 'self' blob:{frames}; connect-src 'self' blob:; object-src 'none'; \
    base-uri 'none'; form-action 'self'; frame-ancestors 'self'";

struct Asset {
    body: Vec<u8>,
    /// the body gzipped, for text
    gz: Option<Vec<u8>>,
    ctype: &'static str,
    ver: String,
}

pub struct Assets {
    /// the pages' Content-Security-Policy
    csp: std::sync::RwLock<String>,
    files: HashMap<String, Asset>,
    index: String,
    public: String,
    sw: Asset,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn gzip(b: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(b).expect("gzip to memory");
    e.finish().expect("gzip to memory")
}

impl Assets {
    pub fn new(host: &str, frames: &[String]) -> Self {
        let mut list: Vec<(String, Vec<u8>, &'static str)> = FILES.iter().map(|(n, b, t)| (n.to_string(), b.to_vec(), *t)).collect();
        match texts::packs(TEXTS) {
            Ok(packs) => list.extend(packs.into_iter().map(|(n, js)| (n, js.into_bytes(), JS))),
            // the UI is then English everywhere; texts.rs has a test against this
            Err(e) => tracing::error!("static/texts.js: {e}"),
        }
        let asset = |body: Vec<u8>, ctype: &'static str| {
            let ver = hex::encode(Sha256::digest(&body))[..10].to_string();
            let gz = ctype.starts_with("text/") || ctype == "image/svg+xml";
            Asset { gz: gz.then(|| gzip(&body)), body, ctype, ver }
        };
        let files: HashMap<String, Asset> = list.into_iter().map(|(name, body, ctype)| (name, asset(body, ctype))).collect();
        let mut names: Vec<&String> = files.keys().collect();
        names.sort();
        let meta = names.iter().map(|n| format!("{n}={}", files[*n].ver)).collect::<Vec<_>>().join(" ");
        let fill = |page: &str| {
            let mut s = page.replace("{{assets}}", &meta).replace("{{host}}", &esc(host));
            for (n, a) in &files {
                s = s.replace(&format!("/static/{n}?v={{{{v}}}}"), &format!("/static/{n}?v={}", a.ver));
            }
            s
        };
        let csp = CSP_HTML.replace("{frames}", &frames.iter().map(|f| format!(" {f}")).collect::<String>());
        Assets { csp: std::sync::RwLock::new(csp), index: fill(INDEX), public: fill(PUBLIC), files, sw: asset(SW.to_vec(), JS) }
    }

    fn csp(&self) -> String {
        self.csp.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Other sites the pages may frame, when the proxies change.
    pub fn set_frames(&self, frames: &[String]) {
        *self.csp.write().unwrap_or_else(|e| e.into_inner()) = CSP_HTML.replace("{frames}", &frames.iter().map(|f| format!(" {f}")).collect::<String>());
    }

    /// Inside osada 0.2: the login page hands over to its sign-in (/signin).
    pub fn bridged(&mut self) {
        self.index = self.index.replacen("<html lang=\"en\">", "<html lang=\"en\" data-signin=\"/signin\">", 1);
    }

    pub fn index_page(&self) -> Response {
        html(self.index.clone(), StatusCode::OK, &self.csp())
    }

    pub fn public_page(&self, status: StatusCode) -> Response {
        html(self.public.clone(), status, &self.csp())
    }
}

fn html(body: String, status: StatusCode, csp: &str) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::CACHE_CONTROL, "no-cache".to_string()),
            (header::CONTENT_SECURITY_POLICY, csp.to_string()),
        ],
        body,
    )
        .into_response()
}

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/static/{file}", get(file)).route("/sw.js", get(worker))
}

/// From the top of the site, to look after all of it; browsers check it for changes themselves.
async fn worker(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    send(&app.assets.sw, "no-cache", &headers)
}

async fn file(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(a) = app.assets.files.get(&name) else {
        return AppError::not_found().into_response();
    };
    send(a, if q.get("v") == Some(&a.ver) { "public, max-age=31536000, immutable" } else { "no-cache" }, &headers)
}

fn send(a: &Asset, cache: &str, headers: &HeaderMap) -> Response {
    let etag = format!("\"{}\"", a.ver);
    let base = [(header::CACHE_CONTROL, cache), (header::ETAG, etag.as_str()), (header::VARY, "accept-encoding")];
    if headers.get(header::IF_NONE_MATCH).is_some_and(|v| v.as_bytes() == etag.as_bytes()) {
        return (StatusCode::NOT_MODIFIED, base).into_response();
    }
    let gzip_ok = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',').any(|e| {
                let mut p = e.split(';').map(str::trim);
                p.next() == Some("gzip") && !p.any(|q| q.strip_prefix("q=").and_then(|q| q.parse::<f32>().ok()) == Some(0.0))
            })
        });
    match &a.gz {
        Some(gz) if gzip_ok => (base, [(header::CONTENT_TYPE, a.ctype), (header::CONTENT_ENCODING, "gzip")], gz.clone()).into_response(),
        _ => (base, [(header::CONTENT_TYPE, a.ctype)], a.body.clone()).into_response(),
    }
}

/// Every other GET is a page of the single-page app.
pub async fn fallback(State(app): State<Arc<App>>, method: Method, uri: Uri) -> Response {
    let p = uri.path();
    let api = ["/api/", "/s/", "/static/", "/oidc/", "/.well-known/"].iter().any(|x| p.starts_with(x)) || p == "/favicon.ico";
    if api || (method != Method::GET && method != Method::HEAD) {
        return AppError::not_found().into_response();
    }
    app.assets.index_page()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_get_a_version_for_every_file() {
        let a = Assets::new("h", &[]);
        for page in [&a.index, &a.public] {
            assert!(!page.contains("{{"), "a placeholder is left: {page}");
            for src in page.split("/static/").skip(1) {
                let name = src.split('?').next().unwrap();
                assert!(a.files.contains_key(name), "{name} is not an asset");
            }
        }
        assert!(a.files.contains_key("lang-cs.js") && a.files["app.js"].gz.is_some() && a.files["favicon.svg"].gz.is_some());
        assert!(a.index.contains(&format!("app.js={}", a.files["app.js"].ver)));
    }
}
