//! osada 0.1, carried whole as a built-in module of 0.2 (docs/PLAN-0.2.0.md,
//! D-U1): files, the wiki, shares, links, mounts, backup, versions, pictures,
//! phone sync, API keys, OpenID Connect, mail sync, apptab, the edge
//! listener. Its storage stays as it was (shelf.db, plain files).

pub mod api;
pub mod assets;
pub mod auth;
pub mod backup;
pub mod livebackup;
pub mod crypto;
pub mod db;
pub mod edge;
pub mod error;
pub mod federation;
pub mod files;
pub mod http_util;
pub mod mail;
pub mod md;
pub mod mounts;
pub mod apptab;
pub mod oidc;
pub mod pictures;
pub mod public;
pub mod quota;
pub mod sendmail;
pub mod shares;
pub mod sizes;
pub mod sync;
pub mod texts;
pub mod tls;
pub mod users;
pub mod versions;
pub mod vfs;
pub mod wiki;

use std::{
    fs,
    future::IntoFuture,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use axum::{
    extract::{DefaultBodyLimit, Request},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router,
};

use crate::osada1::error::AppError;

pub struct Config {
    /// database, secret.key, users/, deleted/, wiki/, tmp/, thumbs/
    pub data: PathBuf,
    pub bind: String,
    /// `https://files.example.org` if links should be absolute; None = relative
    pub public_url: Option<String>,
    /// set when served over https (behind a proxy)
    pub secure_cookie: bool,
    /// shown in the title bar
    pub host: String,
    /// TLS listener for the public edge (edge.rs), e.g. `0.0.0.0:8080`
    pub edge_bind: Option<String>,
    /// HTTPS on the LAN with the public name's certificate (tls.rs), e.g. `0.0.0.0:443`
    pub tls_bind: Option<String>,
    /// apps1's apps in a tab (apptab.rs): the name they are shown at and where they are
    pub apptab_host: Option<String>,
    pub apptab_upstream: Option<String>,
    /// the webmail (`https://mail.example.org/maildot/`): a "maildot" link in the top bar for
    /// accounts with a mail address
    pub maildot_url: Option<String>,
    /// UPS status page (JSON); the backup runs when it leaves line power and
    /// prunes only on line power
    pub ups_url: Option<String>,
    /// `http://192.168.1.10`: sign-in for other programs (oidc.rs) is on, as this issuer
    pub oidc_issuer: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let host = var("OSADA_HOST").unwrap_or_else(|| {
            fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "osada".into())
        });
        Config {
            data: var("OSADA_DATA").unwrap_or_else(|| "/home/osada".into()).into(),
            bind: var("OSADA_BIND").unwrap_or_else(|| "0.0.0.0:80".into()),
            public_url: var("OSADA_PUBLIC_URL").map(|u| u.trim().trim_end_matches('/').to_string()),
            secure_cookie: var("OSADA_SECURE_COOKIE").is_some_and(|v| v == "1" || v == "true"),
            edge_bind: var("OSADA_EDGE_BIND"),
            tls_bind: var("OSADA_TLS_BIND"),
            apptab_host: var("OSADA_APPTAB_HOST"),
            apptab_upstream: var("OSADA_APPTAB_UPSTREAM"),
            maildot_url: var("OSADA_MAILDOT_URL").map(|u| u.trim().to_string()).filter(|u| u.starts_with("https://") || u.starts_with("http://")),
            ups_url: var("OSADA_UPS_URL"),
            oidc_issuer: var("OSADA_OIDC_ISSUER").map(|u| u.trim().trim_end_matches('/').to_string()),
            host,
        }
    }
}

pub struct App {
    pub cfg: Config,
    pub db: Arc<db::Db>,
    pub key: crypto::Key,
    pub sftp: vfs::sftp::Pool,
    pub limiter: auth::Limiter,
    pub assets: assets::Assets,
    pub backup: backup::Runner,
    pub edge: Option<edge::Edge>,
    pub asns: edge::Asns,
    pub pics: pictures::Index,
    pub sizes: sizes::Sizes,
    pub quotas: quota::Quotas,
    pub snaps: versions::Snapshots,
    pub oidc: oidc::Oidc,
    /// other programs' pages at names of their own (apptab.rs): OSADA_APPTAB_*, or inside osada 0.2
    /// its proxy modules
    pub proxies: apptab::Proxies,
    /// What file cache keys are made with (edge.rs `probe`): the edge's token, or this server's own.
    pub cache_secret: String,
    /// osada 0.2's identity, when this runs inside it (auth::Bridge).
    pub bridge: Option<Arc<dyn auth::Bridge>>,
    /// Inside osada 0.2: where the router for requests an edge node relays goes (relay.rs), once made.
    pub relayed: Option<Arc<std::sync::OnceLock<Router>>>,
}

impl App {
    pub fn user_root(&self, name: &str) -> PathBuf {
        self.cfg.data.join("users").join(name)
    }
    pub fn tmp_dir(&self) -> PathBuf {
        self.cfg.data.join("tmp")
    }
}

/// Files created as root in the data folder would lock the service out.
pub fn check_owner(data: &Path) -> anyhow::Result<()> {
    // SAFETY: geteuid cannot fail
    let euid = unsafe { libc::geteuid() };
    if let Ok(m) = fs::metadata(data) {
        if euid == 0 && m.uid() != 0 {
            anyhow::bail!("{} belongs to another user; run as that user: runuser -u osada -- osada …", data.display());
        }
    }
    Ok(())
}

pub fn open_app(cfg: Config) -> anyhow::Result<App> {
    for d in ["users", "tmp", "deleted", "wiki"] {
        fs::create_dir_all(cfg.data.join(d))?;
    }
    let db = Arc::new(db::Db::open(&cfg.data.join("shelf.db"))?);
    let key = crypto::Key::load_or_create(&cfg.data.join("secret.key"))?;
    let edge = cfg.edge_bind.is_some().then(|| edge::Edge::load(&cfg.data)).transpose()?;
    // passes are signed with the edge's token, which osada-edge checks them with; without an edge, with
    // a key of this server's own
    let secret = match &edge {
        Some(e) => e.token().to_string(),
        None => hex::encode(<sha2::Sha256 as sha2::Digest>::digest([b"shelf proxy v1 ".as_slice(), &fs::read(cfg.data.join("secret.key"))?].concat())),
    };
    let apptab = match (&cfg.apptab_host, &cfg.apptab_upstream) {
        (Some(h), Some(u)) => vec![apptab::Apptab::new("apptab", "apptab", h, u, &secret)?],
        _ => Vec::new(),
    };
    let proxies = apptab::Proxies::new(apptab, secret.clone());
    let frames = proxies.frames();
    Ok(App {
        cache_secret: secret,
        proxies,
        edge,
        assets: assets::Assets::new(&cfg.host, &frames),
        sftp: vfs::sftp::Pool::new(db.clone()),
        db,
        key,
        limiter: Default::default(),
        backup: Default::default(),
        asns: Default::default(),
        pics: pictures::Index::new(),
        sizes: Default::default(),
        quotas: Default::default(),
        snaps: Default::default(),
        oidc: Default::default(),
        bridge: None,
        relayed: None,
        cfg,
    })
}

/// State-changing API calls must carry `X-Shelf` (a form on another site
/// cannot set it), and every response gets the basic hardening headers.
/// `/api/v1` and `/api/mail` are left out: they take only their tokens, never the login cookie.
async fn guard(req: Request, next: Next) -> Response {
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let path = req.uri().path();
    let api = path.starts_with("/api/");
    let keyed = path == "/api/v1" || path.starts_with("/api/v1/") || path.starts_with("/api/mail/");
    if !safe && api && !keyed && !req.headers().contains_key("x-shelf") {
        return AppError::new(StatusCode::FORBIDDEN, "missing X-Shelf header").into_response();
    }
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    // a caching front server must never keep per-user answers
    h.entry(header::CACHE_CONTROL).or_insert(HeaderValue::from_static(if api { "no-store" } else { "no-cache" }));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.entry(header::REFERRER_POLICY).or_insert(HeaderValue::from_static("same-origin"));
    h.entry(header::X_FRAME_OPTIONS).or_insert(HeaderValue::from_static("SAMEORIGIN"));
    res
}

/// With HTTPS on: a plain http request for the public name (or apptab's) goes there (the home DNS points
/// them here); the address of this server, and anything else, stay plain (the android app, other programs' sign-in)
async fn to_https(axum::extract::State(app): axum::extract::State<Arc<App>>, req: Request, next: Next) -> Response {
    let public = app.cfg.public_url.as_deref().and_then(|u| u.strip_prefix("https://"));
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).map(|h| h.rsplit_once(':').map_or(h, |(n, _)| n));
    let proxies = app.proxies.all();
    let names: Vec<&str> = public.into_iter().chain(proxies.iter().map(|m| m.host.as_str())).collect();
    match host.and_then(|h| names.into_iter().find(|n| h.eq_ignore_ascii_case(n))) {
        Some(p) => {
            let to = format!("https://{p}{}", req.uri().path_and_query().map_or("/", |q| q.as_str()));
            let code = if matches!(*req.method(), Method::GET | Method::HEAD) { StatusCode::MOVED_PERMANENTLY } else { StatusCode::PERMANENT_REDIRECT };
            (code, [(header::LOCATION, to)]).into_response()
        }
        None => next.run(req).await,
    }
}

async fn housekeeping(app: Arc<App>) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    for n in 0u64.. {
        tick.tick().await;
        app.sftp.reap(Duration::from_secs(600));
        app.snaps.reap(Duration::from_secs(600));
        if n % 10 == 0 {
            pictures::kick(&app, Duration::ZERO);
        }
        if n % 60 != 0 {
            continue;
        }
        let a = app.clone();
        tokio::spawn(async move { a.asns.housekeeping(&a.db).await });
        let t = db::now();
        app.db
            .with(|c| {
                c.execute("DELETE FROM sessions WHERE expires < ?1", [t])?;
                c.execute("DELETE FROM links WHERE expires IS NOT NULL AND expires < ?1", [t])?;
                c.execute("DELETE FROM api_keys WHERE expires IS NOT NULL AND expires < ?1", [t])?;
                c.execute("DELETE FROM oidc_tokens WHERE expires < ?1", [t])
            })
            .ok();
        // uploads that died half way
        let day_ago = SystemTime::now() - Duration::from_secs(86400);
        for e in fs::read_dir(app.tmp_dir()).into_iter().flatten().flatten() {
            if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < day_ago) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

async fn shutdown(stop: tokio_util::sync::CancellationToken) {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    tracing::info!("shutting down");
    pictures::stop();
    stop.cancel();
}

/// osada 0.1 on its own: its runtime, its listeners.
pub fn serve(app: App) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(run(app, None))
}

/// What is merged into this module's router, made once its App is shared.
pub type Extra = Box<dyn FnOnce(&Arc<App>) -> Router<Arc<App>> + Send>;

/// The listeners (LAN, https, edge) until SIGTERM, on the caller's runtime;
/// `extra` (osada 0.2's pages and API) is merged into every one of them.
pub async fn run(mut app: App, extra: Option<Extra>) -> anyhow::Result<()> {
    app.pics.load(&app.db).map_err(|e| anyhow::anyhow!(e.msg))?;
    oidc::ready(&app)?;
    if app.bridge.is_some() {
        app.assets.bridged();
    }
    let app = Arc::new(app);
    let mut router = Router::new()
        .merge(users::routes())
        .merge(files::routes())
        .merge(shares::routes())
        .merge(federation::routes())
        .merge(mounts::routes())
        .merge(backup::routes())
        .merge(versions::routes())
        .merge(pictures::routes())
        .merge(sync::routes())
        .merge(api::routes())
        .merge(oidc::routes())
        .merge(mail::routes())
        .merge(apptab::routes())
        .merge(wiki::routes())
        .merge(public::routes())
        .merge(assets::routes());
    if let Some(extra) = extra {
        router = router.merge(extra(&app));
    }
    let router = router
        .fallback(assets::fallback)
        // JSON bodies; uploads stream past this
        .layer(DefaultBodyLimit::max(8 << 20))
        .layer(middleware::from_fn(guard));
    tokio::spawn(housekeeping(app.clone()));
    tokio::spawn(backup::scheduler(app.clone()));
    // Round 17: the nightly backup into the cluster's settings, once
    tokio::spawn(backup::migrate(app.clone()));
    // the live backups of the files (Round 14)
    tokio::spawn(livebackup::run(app.clone()));
    tokio::spawn(backup::ups_watch(app.clone()));
    if app.bridge.is_some() {
        refresh_proxies(&app);
        tokio::spawn(sync_users(app.clone()));
        let a = app.clone();
        tokio::spawn(async move {
            let app = a;
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(3));
            loop {
                tick.tick().await;
                refresh_proxies(&app);
            }
        });
    }
    if let Some(r) = &app.relayed {
        // as the edge listener's, the visitor's address set by the relay instead of osada-edge's token
        let relayed = Router::new().route("/_edge/links", axum::routing::get(edge::links_for_relay)).merge(router.clone());
        let _ = r.set(relayed.layer(middleware::from_fn_with_state(app.clone(), apptab::route)).with_state(app.clone()));
    }
    let stop = tokio_util::sync::CancellationToken::new();
    tokio::spawn(shutdown(stop.clone()));
    let mut servers: Vec<futures_util::future::BoxFuture<'static, std::io::Result<()>>> = Vec::new();
    let mut lan = router.clone().layer(middleware::from_fn(edge::lan));
    if let Some(bind) = &app.cfg.tls_bind {
        let https = router
            .clone()
            .layer(middleware::from_fn_with_state(app.clone(), apptab::route))
            .layer(middleware::from_fn(tls::mark))
            .layer(middleware::from_fn(edge::lan))
            .layer(middleware::from_fn(crate::accesslog::tls))
            .with_state(app.clone());
        let listener = edge::TlsListener::bind(bind, tls::config(&app.cfg.data.join("tls"))?).await?;
        tracing::info!("https on {bind}");
        servers.push(Box::pin(
            axum::serve(listener, https.into_make_service_with_connect_info::<edge::PeerAddr>())
                .with_graceful_shutdown(stop.clone().cancelled_owned())
                .into_future(),
        ));
        lan = lan.layer(middleware::from_fn_with_state(app.clone(), to_https));
    }
    let listener = tokio::net::TcpListener::bind(&app.cfg.bind).await?;
    tracing::info!("osada {} on {}, data in {}", env!("CARGO_PKG_VERSION"), app.cfg.bind, app.cfg.data.display());
    servers.push(Box::pin(
        axum::serve(listener, lan.layer(middleware::from_fn(crate::accesslog::lan)).with_state(app.clone()).into_make_service_with_connect_info::<edge::PeerAddr>())
            .with_graceful_shutdown(stop.clone().cancelled_owned())
            .into_future(),
    ));
    if let (Some(bind), Some(e)) = (&app.cfg.edge_bind, &app.edge) {
        let edge = router
            .layer(middleware::from_fn_with_state(app.clone(), apptab::route))
            .layer(middleware::from_fn_with_state(app.clone(), edge::gate))
            .with_state(app.clone());
        let listener = edge::TlsListener::bind(bind, e.tls()).await?;
        tracing::info!("edge listener (tls) on {bind}");
        servers.push(Box::pin(
            axum::serve(listener, edge.into_make_service_with_connect_info::<edge::PeerAddr>())
                .with_graceful_shutdown(stop.cancelled_owned())
                .into_future(),
        ));
    }
    futures_util::future::try_join_all(servers).await?;
    Ok(())
}

/// Inside osada 0.2: the proxy modules of the node control (apptab.rs), and the frames the pages may show.
fn refresh_proxies(app: &App) {
    if let Some(b) = &app.bridge {
        if app.proxies.set(&b.proxies()) {
            app.assets.set_frames(&app.proxies.frames());
            let names: Vec<String> = app.proxies.all().iter().map(|m| format!("{} ({})", m.name, m.host)).collect();
            tracing::info!("proxies: {}", if names.is_empty() { "none".into() } else { names.join(", ") });
        }
    }
}

/// Inside osada 0.2: its directory into the users table, now and then (new
/// accounts get their row before they first come, for the mail sync and
/// sharing; renamed and disabled ones follow).
async fn sync_users(app: Arc<App>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
    // AS numbers looked up here already (the hourly round of edge.rs keeps them fresh)
    let mut seen = std::collections::HashSet::new();
    loop {
        tick.tick().await;
        let a = app.clone();
        let _ = tokio::task::spawn_blocking(move || auth::sync_users(&a)).await;
        // AS numbers new in someone's outside networks: their networks now, not at the hourly round
        let lists: Vec<String> = app
            .db
            .with(|c| {
                let mut st = c.prepare("SELECT remote_cidrs FROM users WHERE remote_cidrs LIKE '%AS%'")?;
                let v = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>();
                v
            })
            .unwrap_or_default();
        let mut asns: Vec<u32> = lists.iter().flat_map(|l| edge::asns_in(l)).filter(|n| !seen.contains(n)).collect();
        asns.sort_unstable();
        asns.dedup();
        if !asns.is_empty() {
            // (those fetched lately are passed over)
            app.asns.refresh(&app.db, &asns, false).await;
            seen.extend(asns);
        }
    }
}
