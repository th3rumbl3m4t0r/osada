//! The osada binary. Every node runs this one; what it does is configuration
//! (§4.1).

use std::{
    collections::BTreeSet,
    net::{SocketAddr, ToSocketAddrs},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use osada::{
    cluster::Cluster,
    join::{self, Token},
    kv::{Kv, sqlite::SqliteKv},
    link::{self, JoinRequest, Link},
    node::{self, Node},
    pki::{self, Ca, NodeCert},
    placement::{NodeId, Role},
};

const USAGE: &str = "usage: osada <command> [--option value]...

the drop-in upgrade of osada 0.1 (its environment, /etc/osada/osada.env):
  serve    (no options) osada 0.1 with a node of its own in $OSADA_DATA/node
  user | backup | oidc | mail | edge | wiki ...
           osada 0.1's commands, on its data
  rescue   <user> [--admin] [--create]
           a new password for <user> (printed), enabled, with --admin the
           role admin over /, with --create a new account; the running node
           does it within seconds (as the server's account: runuser -u osada --)

a node:
  init     --data <dir> --node <name>
           start a cluster: this node, alone, with every role
  token    --data <dir> --node <name> --roles <role,...> [--hours <n>] [--dial <host:port>]
           a one-time token for a node to join (on the node with the CA); with
           --dial, this node dials the new one there (it need not reach this one)
  join     --data <dir> [--node <name>] --token <token> [--peer <name>@<host:port>]
           [--listen <host:port>] [--wait <minutes>]
           join a cluster: this node's certificate, from the node with the CA;
           --peer: dial it there; --listen: wait there (UDP) for it to dial this
           node (token --dial); both: whichever comes first; --wait: 30 min
  dial     --data <dir> [--add <name>@<host:port>] [--remove <host:port>]
           the addresses this node keeps dialing (its own list; the running node
           follows it within seconds)
  serve    --data <dir> --listen <host:port> [--peer <name>@<host:port>]... [--http <host:port>]
           [--relay <host:port> --relay-to <node> [--relay-http <host:port>]]
           run the node: its links (kept up to every --peer), joins, and with
           --http the account and admin pages; with --relay an edge: https
           (<dir>/edge-tls/fullchain.pem, privkey.pem) relayed to the node
           --relay-to over its link, --relay-http for certbot (webroot
           <dir>/acme) and a redirect to https
  cluster  --data <dir>
           what the cluster log says
  rescue   --data <dir> <user> [--admin] [--create]
           as above, for a node of its own
  version  the version and the licence
  help     this text

roles: voter idp store blob edge-http edge-mail worker modhost media
";

enum Fail {
    /// The command was called wrong: say so, and how it goes.
    Usage(String),
    /// It ran and did not work.
    Run(String),
}

impl From<String> for Fail {
    fn from(m: String) -> Self {
        Fail::Run(m)
    }
}

impl From<&str> for Fail {
    fn from(m: &str) -> Self {
        Fail::Run(m.into())
    }
}

type Result<T> = std::result::Result<T, Fail>;

fn usage(m: impl Into<String>) -> Fail {
    Fail::Usage(m.into())
}

fn run(e: impl std::fmt::Display) -> Fail {
    Fail::Run(e.to_string())
}

fn main() -> ExitCode {
    // settings from before the rename (SHELF_*, e.g. /etc/shelf/shelf.env) count as OSADA_* ones
    // unless those are set; once, before any thread starts
    for (k, v) in std::env::vars_os() {
        if let Some(rest) = k.to_str().and_then(|k| k.strip_prefix("SHELF_")) {
            let new = format!("OSADA_{rest}");
            if std::env::var_os(&new).is_none() {
                // SAFETY: single-threaded here, at the very start of main
                unsafe { std::env::set_var(new, &v) };
            }
        }
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or(&[]);
    let res = match args.first().map(String::as_str) {
        Some("version" | "--version") => {
            emit(&format!("osada {}\n{}\n{}\n", osada::VERSION, osada::COPYRIGHT, osada::LICENCE));
            Ok(())
        }
        Some("help" | "--help" | "-h") => {
            print!("{USAGE}");
            Ok(())
        }
        Some("init") => init(rest),
        Some("token") => token(rest),
        Some("join") => runtime().and_then(|rt| rt.block_on(join_cluster(rest))),
        None => dropin(),
        Some("serve") if rest.is_empty() => dropin(),
        Some(cmd @ ("user" | "backup" | "oidc" | "mail" | "edge" | "wiki")) => legacy(cmd, rest),
        Some("rescue") => rescue(rest),
        Some("serve") => runtime().and_then(|rt| rt.block_on(serve(rest))),
        Some("cluster") => show(rest),
        Some("dial") => dial_cmd(rest),
        Some("web-agent") => web_agent(rest),
        Some("web") => web_cmd(rest),
        Some(other) => Err(usage(format!("unknown command {other:?}"))),
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(Fail::Usage(m)) => {
            if !m.is_empty() {
                eprintln!("osada: {m}\n");
            }
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
        Err(Fail::Run(m)) => {
            eprintln!("osada: {m}");
            ExitCode::FAILURE
        }
    }
}

/// osada 0.1's log lines (tracing): to the journal without times of their own.
fn logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "osada=info".into());
    let log = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).with_ansi(false);
    if std::env::var_os("JOURNAL_STREAM").is_some() {
        log.without_time().init();
    } else {
        log.init();
    }
}

/// `osada serve` from osada 0.1's environment (dropin.rs).
fn dropin() -> Result<()> {
    logging();
    let cfg = osada::osada1::Config::from_env();
    osada::osada1::check_owner(&cfg.data).map_err(run)?;
    runtime()?.block_on(osada::dropin::serve(cfg)).map_err(Fail::Run)
}

/// The stream handler for the sites that run on this node (D-U27).
fn runner(node: &Arc<Node>, dir: &Path, me: NodeId) -> osada::link::StreamHandler {
    let (rn, mn) = (node.clone(), node.clone());
    let site_of: Arc<dyn Fn(&str) -> Option<osada::placement::Module> + Send + Sync> = Arc::new(move |name: &str| {
        let p = rn.cluster_state().placement;
        osada::sites::sites(&p).find(|(n, m, s)| n.as_str() == name && osada::sites::served_by(&p, m, s, &me)).map(|(_, m, _)| m.clone())
    });
    let may: Arc<dyn Fn(&NodeId) -> bool + Send + Sync> =
        Arc::new(move |peer: &NodeId| {
            let p = mn.cluster_state().placement;
            // the edges, and home for the sites at /web/<site>/ on osada's names (Round 18)
            p.nodes.get(peer).is_some_and(|n| n.has(Role::EdgeHttp)) || p.home_node().as_ref() == Some(peer)
        });
    osada::relay::web_runner(site_of, osada::websrv::Web::new(dir), may)
}

/// `osada web-agent --data <node folder>`: as root, the containers of the
/// sites that run on this node (crate::containers, D-U23).
fn web_agent(args: &[String]) -> Result<()> {
    let o = Opts::parse(args, &["--data"])?;
    let dir = PathBuf::from(o.one("--data")?);
    // started beside the node's service: its first start makes the folder
    for _ in 0..120 {
        if dir.join("store.db").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if !dir.join("store.db").exists() {
        return Err(run(format!("{}: no node here", dir.display())));
    }
    osada::containers::agent(&dir)
}

/// `osada web import <sites.json> [--data <node folder>]`: sites for the
/// running node to add to node control (node/websites.rs), as `osada rescue`.
fn web_cmd(args: &[String]) -> Result<()> {
    let (sub, rest) = args.split_first().ok_or_else(|| usage("osada web import <sites.json> [--data <node folder>]"))?;
    if sub != "import" {
        return Err(usage(format!("unknown: osada web {sub}")));
    }
    let (mut dir, mut file) = (None, None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data" => dir = Some(PathBuf::from(it.next().ok_or_else(|| usage("--data needs a value"))?)),
            f if file.is_none() && !f.starts_with('-') => file = Some(PathBuf::from(f)),
            other => return Err(usage(format!("unknown option {other}"))),
        }
    }
    let file = file.ok_or_else(|| usage("which file?"))?;
    let dir = match dir {
        Some(d) => d,
        None => osada::dropin::node_dir(&osada::osada1::Config::from_env().data),
    };
    if !dir.join("store.db").exists() {
        return Err(run(format!("{}: no node here", dir.display())));
    }
    let text = std::fs::read_to_string(&file).map_err(|e| run(format!("{}: {e}", file.display())))?;
    serde_json::from_str::<serde_json::Value>(&text).map_err(|e| run(format!("{}: not JSON: {e}", file.display())))?;
    let to = dir.join("web-import.json");
    std::fs::write(&to, &text).map_err(|e| run(format!("{}: {e}", to.display())))?;
    println!("asked the node in {} to add the sites of {}; it answers in web-import.done or .failed there", dir.display(), file.display());
    Ok(())
}

/// `osada rescue`: a request for the running node (node/rescue.rs).
fn rescue(args: &[String]) -> Result<()> {
    let (mut dir, mut name, mut admin, mut create) = (None, None, false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data" => dir = Some(PathBuf::from(it.next().ok_or_else(|| usage("--data needs a value"))?)),
            "--admin" => admin = true,
            "--create" => create = true,
            n if !n.starts_with('-') && name.is_none() => name = Some(n.to_lowercase()),
            other => return Err(usage(format!("unknown option {other}"))),
        }
    }
    let name = name.ok_or_else(|| usage("rescue needs a user name"))?;
    let dir = match dir {
        Some(d) => d,
        None => {
            let cfg = osada::osada1::Config::from_env();
            osada::osada1::check_owner(&cfg.data).map_err(run)?;
            osada::dropin::node_dir(&cfg.data)
        }
    };
    if !dir.join("store.db").exists() {
        return Err(run(format!("{}: no node here", dir.display())));
    }
    let password = osada::directory::random_hex(10);
    let hash = osada::directory::hash_password(&password)?;
    let req = osada::node::Rescue { name: name.clone(), hash, admin, create };
    let file = dir.join("rescue.json");
    {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let _ = std::fs::remove_file(dir.join("rescue.failed"));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)
            .map_err(|e| run(format!("{}: {e} (a rescue waiting already?)", file.display())))?;
        f.write_all(serde_json::to_string(&req).map_err(run)?.as_bytes()).map_err(run)?;
    }
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(200));
        if let Ok(why) = std::fs::read_to_string(dir.join("rescue.failed")) {
            return Err(run(format!("the node refused: {}", why.trim())));
        }
        if !file.exists() {
            println!("user: {name}\npassword: {password}");
            return Ok(());
        }
    }
    Err(run(format!("no running node took {} within 10 s; it is done when the node holding {name}'s credentials starts", file.display())))
}

/// osada 0.1's commands, as they were.
fn legacy(cmd: &str, args: &[String]) -> Result<()> {
    use osada::osada1::{self, backup, edge, mail, oidc, users, wiki};
    logging();
    let cfg = osada1::Config::from_env();
    osada1::check_owner(&cfg.data).map_err(run)?;
    let r = match cmd {
        "edge" => edge::cli(&cfg.data, args),
        "backup" => osada1::open_app(cfg).and_then(|app| backup::cli(app, args)),
        _ => osada1::open_app(cfg).and_then(|app| match cmd {
            "user" => users::cli(&app, args),
            "oidc" => oidc::cli(&app, args),
            "mail" => mail::cli(&app, args),
            _ => wiki::cli(&app, args),
        }),
    };
    r.map_err(|e| Fail::Run(format!("{e:#}")))
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(run)
}

/// `--name value` pairs, of the names a command knows.
struct Opts(Vec<(String, String)>);

impl Opts {
    fn parse(args: &[String], known: &[&str]) -> Result<Opts> {
        let mut out = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if !known.contains(&a.as_str()) {
                return Err(usage(format!("unknown option {a}")));
            }
            let v = it.next().ok_or_else(|| usage(format!("{a} needs a value")))?;
            out.push((a.clone(), v.clone()));
        }
        Ok(Opts(out))
    }

    fn all(&self, name: &str) -> Vec<&str> {
        self.0.iter().filter(|(k, _)| k == name).map(|(_, v)| v.as_str()).collect()
    }

    fn maybe(&self, name: &str) -> Result<Option<&str>> {
        match self.all(name).as_slice() {
            [] => Ok(None),
            [v] => Ok(Some(v)),
            _ => Err(usage(format!("{name} given more than once"))),
        }
    }

    fn one(&self, name: &str) -> Result<&str> {
        self.maybe(name)?.ok_or_else(|| usage(format!("{name} is needed")))
    }

    fn node(&self) -> Result<&str> {
        let node = self.one("--node")?;
        if !osada::placement::valid_node_label(node) {
            return Err(usage("a node's name is letters, digits, '-', '_' and '.', up to 63"));
        }
        Ok(node)
    }
}

fn address(s: &str) -> Result<SocketAddr> {
    s.to_socket_addrs().map_err(|e| usage(format!("{s}: {e}")))?.next().ok_or_else(|| usage(format!("{s}: no address")))
}

/// `[<name or id>@]<host:port>`: a node at its address, and what it is
/// called if that is known (D-U19: ids are GUIDs; any node of the cluster
/// that answers there will do when the name is not known yet).
fn peer(s: &str) -> Result<(Option<String>, SocketAddr)> {
    match s.split_once('@') {
        Some((name, addr)) if !name.trim().is_empty() => Ok((Some(name.trim().to_string()), address(addr)?)),
        Some((_, addr)) => Ok((None, address(addr)?)),
        None => Ok((None, address(s)?)),
    }
}

fn store(dir: &Path) -> Result<Arc<dyn Kv>> {
    Ok(Arc::new(SqliteKv::open(&dir.join("store.db")).map_err(run)?))
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn fresh_dir(dir: &Path) -> Result<()> {
    if dir.join("tls").exists() || dir.join("store.db").exists() {
        return Err(run(format!("{} already holds a node", dir.display())));
    }
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| run(format!("{}: {e}", dir.display())))
}

fn init(args: &[String]) -> Result<()> {
    let o = Opts::parse(args, &["--data", "--node"])?;
    let dir = PathBuf::from(o.one("--data")?);
    let node = o.node()?;
    fresh_dir(&dir)?;
    // the node's id is a GUID; `--node` is its name, which admins can change (D-U19)
    let id = NodeId::fresh();
    let c = Cluster::init_named(store(&dir)?, id.clone(), Some(node.into()), now_ms()).map_err(run)?;
    let ca = Ca::new().map_err(run)?;
    ca.save(&dir).map_err(run)?;
    ca.issue(&id.0).and_then(|cert| cert.save(&dir)).map_err(run)?;
    println!("cluster started in {}: node {node} ({id}), cluster log at entry {}", dir.display(), c.log().head().seq);
    println!("cluster CA {}", ca.fingerprint().map_err(run)?);
    Ok(())
}

fn token(args: &[String]) -> Result<()> {
    let o = Opts::parse(args, &["--data", "--node", "--roles", "--hours", "--dial"])?;
    let dir = PathBuf::from(o.one("--data")?);
    let node = o.node()?;
    let roles = o
        .one("--roles")?
        .split(',')
        .map(|r| {
            serde_json::from_value::<Role>(serde_json::Value::String(r.trim().into()))
                .map_err(|_| usage(format!("unknown role {r:?}")))
        })
        .collect::<Result<BTreeSet<Role>>>()?;
    let hours: u64 = match o.maybe("--hours")? {
        None => 1,
        Some(h) => h.parse().ok().filter(|h| (1..=168).contains(h)).ok_or_else(|| usage("--hours is 1 to 168"))?,
    };
    if !dir.join("tls/ca.key").exists() {
        return Err("this node does not hold the cluster CA (tls/ca.key)".into());
    }
    let ca = Ca::load(&dir).map_err(run)?;
    let me = NodeCert::load(&dir).map_err(run)?.node;
    let dial = o.maybe("--dial")?;
    if let Some(a) = dial.filter(|a| !node::dial::valid_addr(a)) {
        return Err(usage(format!("--dial {a}: host:port (the new node's address and link port)")));
    }
    let kv = store(&dir)?;
    let (t, id) = join::make_token(&*kv, &ca, node, roles, now_ms() + hours * 3_600_000)?;
    if let Some(a) = dial {
        node::dial::add(&*kv, a, node, now_ms())?;
    }
    println!("{t}");
    eprintln!("for the node {node} (its id will be {id}), once, within {hours} h, while this node ({me}) serves:");
    match dial {
        Some(a) => eprintln!("  osada join --data <dir> --token <the token> --listen <host:port>: this node dials {a} and lets it in"),
        None => eprintln!("  osada join --data <dir> --token <the token> --peer <host:port of this node>"),
    }
    Ok(())
}

async fn join_cluster(args: &[String]) -> Result<()> {
    let o = Opts::parse(args, &["--data", "--node", "--token", "--peer", "--listen", "--wait"])?;
    let dir = PathBuf::from(o.one("--data")?);
    // the name the token is for (optional: the token knows it; D-U19)
    let node = match o.maybe("--node")? {
        Some(_) => o.node()?.to_string(),
        None => String::new(),
    };
    let token = Token::parse(o.one("--token")?).ok_or_else(|| usage("that is not a join token"))?;
    let dial = o.maybe("--peer")?.map(peer).transpose()?.map(|(_, a)| a);
    let listen = o.maybe("--listen")?.map(address).transpose()?;
    if dial.is_none() && listen.is_none() {
        return Err(usage("join needs --peer (dial the cluster's node), --listen (wait for it to dial this one), or both"));
    }
    let minutes: u64 = match o.maybe("--wait")? {
        None => 30,
        Some(m) => m.parse().ok().filter(|m| (1..=1440).contains(m)).ok_or_else(|| usage("--wait is 1 to 1440 minutes"))?,
    };
    if dir.join("tls").exists() || dir.join("store.db").exists() {
        return Err(run(format!("{} already holds a node", dir.display())));
    }
    let key = rcgen::KeyPair::generate().map_err(run)?;
    let req = JoinRequest { node: node.clone(), token: token.secret.clone(), public_key: hex::encode(key.public_key_raw()) };
    let fp = token.ca_fingerprint.clone();
    // dialing: once without --listen (as ever); while also waiting, again every 10 s until a
    // refusal, which only the token's cluster can give
    let dialing = async {
        let Some(a) = dial else { return std::future::pending().await };
        loop {
            match link::request_join(a, &fp, req.clone()).await {
                Ok(g) => return Ok(g),
                Err(e) if listen.is_none() || e.0.starts_with("refused") => return Err(e),
                Err(e) => {
                    eprintln!("join: dialing {a}: {e}; again in 10 s");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        }
    };
    let waiting = async {
        let Some(l) = listen else { return std::future::pending().await };
        println!("waiting up to {minutes} min at {l} (udp) for the cluster's node to dial this one (on it: osada token … --dial <this node's address>)");
        link::wait_join(l, &fp, req.clone()).await
    };
    let grant = tokio::time::timeout(Duration::from_secs(minutes * 60), async {
        tokio::select! {
            g = dialing => g,
            g = waiting => g,
        }
    })
    .await
    .map_err(|_| run(format!("no node of the cluster came within {minutes} min")))?
    .map_err(run)?;

    if grant.by.is_empty() || grant.node.is_empty() {
        return Err(run("the node answering is older than this one: it said no ids"));
    }
    fresh_dir(&dir)?;
    let (id, by) = (grant.node.clone(), NodeId(grant.by.clone()));
    let cert = NodeCert { node: id.clone(), cert_pem: grant.cert_pem, key_pem: key.serialize_pem(), ca_pem: grant.ca_pem };
    cert.save(&dir).map_err(run)?;
    node::remember_voter(&*store(&dir)?, &by)?;
    println!("joined the cluster of {by} as {} ({id}); its certificate is in {}", if node.is_empty() { "the token's node" } else { &node }, dir.join("tls").display());
    println!("next: osada serve --data {} --listen <host:port> (the cluster's node dials it, or --peer: it dials that)", dir.display());
    Ok(())
}

async fn serve(args: &[String]) -> Result<()> {
    let o = Opts::parse(args, &["--data", "--listen", "--peer", "--http", "--relay", "--relay-to", "--relay-http", "--fallback-http", "--fallback-tls"])?;
    let dir = PathBuf::from(o.one("--data")?);
    let listen = address(o.one("--listen")?)?;
    let peers = o.all("--peer").into_iter().map(peer).collect::<Result<Vec<_>>>()?;
    let tls = NodeCert::load(&dir).and_then(|c| c.tls()).map_err(run)?;
    let kv = store(&dir)?;
    let link = Link::bind(&tls, listen).map_err(run)?;
    let ca = if dir.join("tls/ca.key").exists() { Some(Arc::new(Ca::load(&dir).map_err(run)?)) } else { None };
    let node = Node::start(kv, link.clone(), ca, Some(dir.join("first-admin")))?;
    node.set_data(dir.clone());
    node.watch_web_import(dir.join("web-import.json"));
    node.spawn_certs(None);
    node.spawn_ship();
    node.spawn_logs();
    // where this node keeps copies of files (D-U41): its files folder once the root agent made it ours
    let me_id = NodeId(tls.node.clone());
    let files_dir_at_start = node.cluster_state().placement.nodes.get(&me_id).and_then(|n| n.files_dir.clone());
    let (files, ask) = osada::filesdir::base(&dir, files_dir_at_start.as_deref(), &me_id);
    if ask {
        let target = files_dir_at_start.as_deref().map(|d| osada::filesdir::folder(d, &me_id));
        if osada::filesdir::request(&dir, target.as_deref()) {
            println!("{me_id}: files folder {}: asked the web agent (it restarts this node)", files_dir_at_start.as_deref().unwrap_or(""));
        }
    }
    if files != dir {
        // the copies left in the data folder are not used any more
        for sub in ["cache", "keep"] {
            let _ = std::fs::remove_dir_all(dir.join(sub));
        }
        println!("{me_id}: copies of files in {}", files.display());
    }
    // the sites that run here, asked for by an edge (D-U27)
    // the links' rates and this node's uplink (D-U40): the weekly tests, the answers to other nodes
    osada::linkstats::init(Some(&dir));
    osada::linkstats::spawn_tests(link.clone());
    osada::linkstats::spawn_rates(link.clone());
    // egress (Round 19): every node an exit for the containers and jobs that say so, and the way
    // out for its own (from the root agent's local socket)
    let egress_placement: osada::egress::PlacementFn = { let n = node.clone(); Arc::new(move || n.cluster_state().placement) };
    osada::egress::spawn_local(&dir, link.clone(), egress_placement.clone());
    link.take_streams(osada::relay::dispatch(vec![
        (osada::egress::KIND, osada::egress::server(egress_placement, NodeId(tls.node.clone()))),
        (osada::relay::WEB_KIND, runner(&node, &dir, NodeId(tls.node.clone()))),
        (osada::relay::CACHE_KIND, osada::relay::cache_server()),
        (osada::linkstats::BW_KIND, osada::linkstats::server()),
        // a live backup kept here (Round 14): in this node's files folder
        (osada::livetarget::KIND, osada::livetarget::server({ let f = files.clone(); Arc::new(move || f.clone()) })),
        // a site's copy from the authoritative web node (Round 16), in this node's web/
        (osada::livetarget::WEB_MIRROR, osada::livetarget::web_mirror_server(dir.clone())),
        // the nightly backup's parts from this node (Round 17), for home
        (osada::nodebackup::KIND, osada::nodebackup::server(dir.clone(), { let n = node.clone(); Arc::new(move || n.cluster_state().placement.home_node()) })),
    ]));
    node.watch_rescue(dir.join("rescue.json"));
    link.spawn_accept();
    if let Some(http) = o.maybe("--http")? {
        let addr = address(http)?;
        let web = node.clone();
        tokio::spawn(async move {
            if let Err(e) = osada::web::serve(web, addr).await {
                eprintln!("web: {e}");
            }
        });
        println!("{} pages on http://{addr}", tls.node);
    }
    if let Some(relay) = o.maybe("--relay")? {
        let bind = address(relay)?;
        // home: the node named (id or name), else the authority for files; looked up again every few
        // seconds, so a node renamed or a link not yet there does not matter (D-U19)
        let want = o.maybe("--relay-to")?.map(str::to_string);
        let to = want.clone().unwrap_or_else(|| "home (the authority for files)".into());
        let (hn, hs) = (node.clone(), std::sync::Mutex::new((std::time::Instant::now() - Duration::from_secs(60), None::<NodeId>)));
        let home: osada::relay::Home = Arc::new(move || {
            let mut s = hs.lock().unwrap_or_else(|e| e.into_inner());
            if s.0.elapsed() > Duration::from_secs(5) || s.1.is_none() {
                let p = hn.cluster_state().placement;
                s.1 = match &want {
                    Some(w) => p.resolve_node(w),
                    None => p.home_node().filter(|h| h.0 != hn.me().0),
                };
                s.0 = std::time::Instant::now();
            }
            s.1.clone()
        });
        let plain = o.maybe("--relay-http")?.map(address).transpose()?;
        let (l, d) = (link.clone(), dir.clone());
        // its roles in node control (cache, public-cache) and cache size; looked at every few seconds
        let (n, me) = (node.clone(), NodeId(tls.node.clone()));
        let seen = std::sync::Mutex::new((std::time::Instant::now() - Duration::from_secs(60), (false, false, 20u32)));
        let roles: osada::relay::Roles = Arc::new(move || {
            let mut s = seen.lock().unwrap_or_else(|e| e.into_inner());
            if s.0.elapsed() > Duration::from_secs(5) {
                let p = n.cluster_state().placement;
                s.1 = p.nodes.get(&me).map_or((false, false, 20), |x| (x.has(Role::Cache), x.has(Role::PublicCache), x.cache_gb.unwrap_or(20)));
                s.0 = std::time::Instant::now();
            }
            s.1
        });
        // TLS for the host names of this node's mail instances, passed on to their servers (D-U16)
        let (n, me) = (node.clone(), NodeId(tls.node.clone()));
        let hosts = std::sync::Mutex::new((std::time::Instant::now() - Duration::from_secs(60), Vec::<(String, String)>::new()));
        let pass: osada::passthrough::Pass = Arc::new(move |name: &str| {
            let mut h = hosts.lock().unwrap_or_else(|e| e.into_inner());
            if h.0.elapsed() > Duration::from_secs(5) {
                h.1 = n.cluster_state().placement.modules.values()
                    .filter(|m| m.kind == osada::placement::ModuleKind::Data(osada::placement::Domain::Mail) && m.node == me)
                    .filter_map(|m| Some((m.host.clone()?, m.upstream.as_deref()?.strip_prefix("tls://")?.to_string())))
                    .collect();
                h.0 = std::time::Instant::now();
            }
            h.1.iter().find(|(host, _)| host == name).map(|(_, up)| up.clone())
        });
        // the sites exposed here (D-U21); names without a site or a certificate of this edge go
        // to the server that had them (nginx): http proxied with its Host, TLS passed on (D-U25)
        let store = osada::acme::Store::new(&dir);
        let st = store.clone();
        let ours: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(move |name: &str| st.covers(name));
        let (sn, sme) = (node.clone(), NodeId(tls.node.clone()));
        let sp = std::sync::Mutex::new((std::time::Instant::now() - Duration::from_secs(60), osada::placement::Placement::default()));
        let site_of: osada::relay::SiteOf = Arc::new(move |host: &str| {
            let mut s = sp.lock().unwrap_or_else(|e| e.into_inner());
            if s.0.elapsed() > Duration::from_secs(5) {
                s.1 = sn.cluster_state().placement;
                s.0 = std::time::Instant::now();
            }
            osada::sites::exposed(&s.1, &sme, host).map(|(n, m, _)| (n.clone(), m.clone()))
        });
        let fallback_tls = o.maybe("--fallback-tls")?.map(str::to_string);
        let (mail, so, ou) = (pass, site_of.clone(), ours.clone());
        let pass: osada::passthrough::Pass = Arc::new(move |name: &str| {
            mail(name).or_else(|| fallback_tls.clone().filter(|_| !ou(name) && so(name).is_none()))
        });
        let sites = Arc::new(osada::relay::WebEdge {
            me: NodeId(tls.node.clone()),
            site_of,
            web: osada::websrv::Web::new(&dir),
            cf: osada::websrv::cloudflare(&dir),
            fallback_http: o.maybe("--fallback-http")?.map(str::to_string),
            ours,
            link: link.clone(),
            web_authority: { let wn = node.clone(); Arc::new(move || wn.cluster_state().placement.web.authority.clone()) },
            acme: {
                let an = node.clone();
                Arc::new(move |host: String, token: String| {
                    let an = an.clone();
                    Box::pin(async move { an.acme_for(&host, &token).await }) as futures_util::future::BoxFuture<'static, Option<String>>
                })
            },
        });
        // while home is away, sign-in and other programs' sign-in here when this node keeps secrets (D-U45)
        let opts = osada::web::Options { oidc: Some("https"), ..osada::web::Options::standalone(node.me()) };
        let kn = node.clone();
        let local = osada::relay::Local { router: osada::web::router::<()>(node.clone(), opts), active: Arc::new(move || kn.keeps_any_secrets()) };
        tokio::spawn(async move {
            if let Err(e) = osada::relay::serve_edge(l, home, d, files, bind, plain, roles, Some(pass), Some(sites), store, Some(local)).await {
                eprintln!("edge: {e}");
            }
        });
        println!("{} relays https on {bind} to {to}", tls.node);
    }
    // the files folder, asked of the root agent again when node control changes it (D-U41)
    {
        let (fnode, fdir, fme, mut asked) = (node.clone(), dir.clone(), NodeId(tls.node.clone()), files_dir_at_start.clone());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let now = fnode.cluster_state().placement.nodes.get(&fme).and_then(|n| n.files_dir.clone());
                if now != asked {
                    let target = now.as_deref().map(|d| osada::filesdir::folder(d, &fme));
                    if osada::filesdir::request(&fdir, target.as_deref()) {
                        println!("{fme}: files folder {}: asked the web agent (it restarts this node)", now.as_deref().unwrap_or("(the data folder)"));
                    }
                    asked = now;
                }
            }
        });
    }
    // the containers of the sites that run here, for the root agent (D-U23)
    let (wn, wd, wme) = (node.clone(), dir.clone(), NodeId(tls.node.clone()));
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            let w = osada::containers::wanted(&wn.cluster_state().placement, &wme, None);
            if !w.is_empty() || wd.join("web/containers.json").exists() {
                if let Err(e) = osada::containers::write_wanted(&wd, &w) {
                    eprintln!("{wme}: web/containers.json: {e}");
                }
            }
            let jw = osada::containers::jobs_wanted(&wn.cluster_state().placement, &wme);
            if !jw.is_empty() || wd.join("web/jobs.json").exists() {
                if let Err(e) = osada::containers::write_jobs(&wd, &jw) {
                    eprintln!("{wme}: web/jobs.json: {e}");
                }
            }
        }
    });
    let placement = node.cluster_state().placement;
    for (name, a) in &peers {
        link.spawn_keep_dialing_at(name.as_deref().and_then(|n| placement.resolve_node(n)), *a);
    }
    println!("{} serving on {} (udp)", tls.node, link.local_addr().map_err(run)?);
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(run)?;
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    tick.tick().await;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = term.recv() => break,
            _ = tick.tick() => {
                let mut seen = Vec::new();
                for p in link.peers() {
                    match link.ping(&p).await {
                        Ok(d) => seen.push(format!("{p} {:.2} ms", d.as_secs_f64() * 1000.0)),
                        Err(e) => seen.push(format!("{p}: {e}")),
                    }
                }
                let links = if seen.is_empty() { "none".into() } else { seen.join(", ") };
                let state = node.cluster_state();
                println!("links: {links}; cluster log at {}, {} nodes, {} collections", node.cluster_applied(), state.placement.nodes.len(), state.collections.len());
            }
        }
    }
    link.close_and_wait().await;
    println!("{} stopped", tls.node);
    Ok(())
}

/// `osada dial`: the addresses this node keeps dialing (node/dial.rs).
fn dial_cmd(args: &[String]) -> Result<()> {
    let o = Opts::parse(args, &["--data", "--add", "--remove"])?;
    let dir = PathBuf::from(o.one("--data")?);
    if !dir.join("store.db").exists() {
        return Err(run(format!("{}: no node here", dir.display())));
    }
    let kv = store(&dir)?;
    for a in o.all("--add") {
        let (name, addr) = match a.split_once('@') {
            Some((n, a)) => (n.trim(), a.trim()),
            None => ("", a.trim()),
        };
        if !node::dial::valid_addr(addr) {
            return Err(usage(format!("--add {a}: [<name>@]<host:port>")));
        }
        node::dial::add(&*kv, addr, name, now_ms())?;
    }
    for a in o.all("--remove") {
        if !node::dial::remove(&*kv, a)? {
            eprintln!("{a} was not on the list");
        }
    }
    let list = node::dial::list(&*kv);
    if list.is_empty() {
        println!("this node dials no addresses of its own (OSADA_PEERS and --peer aside)");
    }
    for (addr, d) in list {
        let name = if d.name.is_empty() { "-" } else { &d.name };
        println!("{addr}  {name}  {}", d.node.as_deref().unwrap_or("(no node has answered there yet)"));
    }
    Ok(())
}

fn show(args: &[String]) -> Result<()> {
    let o = Opts::parse(args, &["--data"])?;
    let dir = PathBuf::from(o.one("--data")?);
    if !dir.join("store.db").exists() {
        return Err(run(format!("{}: no store.db; osada init first", dir.display())));
    }
    let c = Cluster::open(store(&dir)?).map_err(run)?;
    let mut out = String::new();
    let head = c.log().head();
    out += &format!("cluster log: {} entries, epoch {}, head {}\n", head.seq, head.epoch, &hex::encode(head.hash)[..16]);
    if let Ok(ca) = std::fs::read_to_string(dir.join("tls/ca.crt")) {
        out += &format!("cluster CA {}\n", pki::fingerprint(&ca).map_err(run)?);
    }
    let state = c.state();
    out += &format!("node control:\n{}\n", serde_json::to_string_pretty(&state.placement).map_err(run)?);
    out += &format!("collections: {}\n", state.collections.len());
    for (id, col) in &state.collections {
        out += &format!("  {id} {} ou {} epoch {} authority {}\n", col.domain, col.ou.0, col.term.epoch, col.term.authority);
    }
    for (id, at, want) in state.misplaced() {
        out += &format!("  to hand over: {id} from {at} to {want}\n");
    }
    emit(&out);
    Ok(())
}

/// Writes to stdout, and lets a reader that went away (`| head`) go.
fn emit(text: &str) {
    use std::io::Write;
    let _ = std::io::stdout().lock().write_all(text.as_bytes());
}
