//! Node links (§6.3): one long-lived QUIC connection per pair of nodes,
//! mutually authenticated with the cluster's certificates (pki.rs), which
//! carries everything between them on streams of its own.
//!
//! - **Either side may dial.** A node at home dials the public node and keeps
//!   the link up (keep-alives every 10 s also keep the NAT's mapping); the
//!   public node then uses the same connection the other way. No port at
//!   home needs to be open.
//! - **Who is who.** The dialer checks that the other side's certificate
//!   names the node it meant to reach (rustls does, against the cluster CA).
//!   The accepting side requires a certificate from the cluster CA and that
//!   it names the node the dialer says it is in its hello.
//! - **One link per pair.** When both dial at once, both keep the connection
//!   dialed by the node whose name sorts first, and close the other.
//!
//! On the wire, each exchange is a bidirectional stream: frames of a 4-byte
//! big-endian length and JSON, the first stream of a connection being the
//! hellos. A stream of a kind (`Link::open_stream`, the edge's HTTP relay)
//! starts with one frame each way, its head, and carries raw bytes after.

use std::{
    collections::BTreeMap,
    fmt,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use quinn::{
    Connection, Endpoint, RecvStream, SendStream,
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
};
use rustls::{
    RootCertStore,
    pki_types::{CertificateDer, ServerName},
    server::WebPkiClientVerifier,
};
use serde::{Deserialize, Serialize};

use crate::{pki::NodeTls, placement::NodeId};

pub const ALPN: &[u8] = b"shelf-node/1";
const MAX_FRAME: usize = 16 << 20;
// a link that is gone is known within IDLE (a peer that stopped says so at once: close_and_wait)
const KEEP_ALIVE: Duration = Duration::from_secs(5);
const IDLE: Duration = Duration::from_secs(15);
/// How long an invitation's handshake may take ([`Link::invite`]).
const CONNECT: Duration = Duration::from_secs(10);
/// Closing codes.
const REFUSED: u32 = 1;
const DUPLICATE: u32 = 2;

#[derive(Debug)]
pub struct LinkError(pub String);

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node link: {}", self.0)
    }
}

impl std::error::Error for LinkError {}

fn err(e: impl fmt::Display) -> LinkError {
    LinkError(e.to_string())
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Frame {
    Hello { node: String, version: String },
    Ping { n: u64 },
    Pong { n: u64 },
    Join { node: String, token: String, public_key: String },
    /// The CA's node dialing a node that waits to join (`Link::invite`, `wait_join`): who it is
    /// and the cluster's CA, which must be the token's before the waiting node says more.
    Invite { by: String, ca_pem: String },
    Joined {
        cert_pem: String,
        ca_pem: String,
        #[serde(default)]
        node: String,
        #[serde(default)]
        by: String,
    },
    Call { body: serde_json::Value },
    Reply { body: serde_json::Value },
    Stream { kind: String, head: serde_json::Value },
}

/// What a node does with a stream of a kind another opens ([`Link::open_stream`]): the other node,
/// the kind, the head, and the stream; it answers with [`write_head`], then bytes.
pub type StreamHandler =
    Arc<dyn Fn(NodeId, String, serde_json::Value, SendStream, RecvStream) -> futures_util::future::BoxFuture<'static, ()> + Send + Sync>;

/// The head of the answer on a stream of a kind.
pub async fn write_head(send: &mut SendStream, head: serde_json::Value) -> Result<(), LinkError> {
    write_frame(send, &Frame::Reply { body: head }).await
}

/// Reads the head of the answer on a stream of a kind.
pub async fn read_head(recv: &mut RecvStream) -> Result<serde_json::Value, LinkError> {
    match read_frame(recv).await? {
        Frame::Reply { body } => Ok(body),
        other => Err(LinkError(format!("expected a head, got {other:?}"))),
    }
}

/// What a node answers when another calls it ([`Link::call`]).
pub type CallHandler = Arc<dyn Fn(&NodeId, serde_json::Value) -> serde_json::Value + Send + Sync>;

/// Where messages from other nodes go ([`Link::send`]): who sent it, and the bytes.
pub type Inbox = tokio::sync::mpsc::UnboundedSender<(NodeId, Vec<u8>)>;

/// The largest message ([`Link::send`]).
pub const MAX_MESSAGE: usize = 64 << 20;
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// A node asking to join (§6.2): its name, the token's secret, and its
/// public key (P-256, raw, hex).
#[derive(Clone, Debug)]
pub struct JoinRequest {
    pub node: String,
    pub token: String,
    pub public_key: String,
}

/// Its certificate and the cluster CA's, PEM.
#[derive(Clone, Debug)]
pub struct JoinGrant {
    pub cert_pem: String,
    pub ca_pem: String,
    /// The id the node has (its certificate's name).
    pub node: String,
    /// The node that let it in (the voter it learns the cluster from).
    pub by: String,
}

/// What the CA's node does with a join request; `Err` says why not, to the
/// joining node.
pub type JoinHandler = Arc<dyn Fn(JoinRequest) -> Result<JoinGrant, String> + Send + Sync>;

struct Live {
    conn: Connection,
    /// The node that dialed it.
    dialer: NodeId,
}

pub struct Link {
    me: NodeId,
    endpoint: Endpoint,
    /// The cluster CA (PEM), shown to a node waiting to join ([`Link::invite`]).
    ca_pem: String,
    /// For [`Link::invite`]: this node's certificate, any server certificate (a node waiting to
    /// join has none from the CA yet; it is trusted for nothing but answering the invitation).
    invite_client: quinn::ClientConfig,
    links: Mutex<BTreeMap<NodeId, Live>>,
    pings: AtomicU64,
    joins: Mutex<Option<JoinHandler>>,
    calls: Mutex<Option<CallHandler>>,
    inbox: Mutex<Option<Inbox>>,
    streams: Mutex<Option<StreamHandler>>,
}

impl Link {
    /// A node's end of its links, listening on `addr` (UDP).
    pub fn bind(tls: &NodeTls, addr: SocketAddr) -> Result<Arc<Link>, LinkError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = RootCertStore::empty();
        roots.add(tls.ca.clone()).map_err(err)?;
        let roots = Arc::new(roots);

        // A dialer without a certificate gets through the handshake only to
        // be refused at its hello: this way a joining node (later) can reach
        // the CA's node on the same port.
        let verifier = WebPkiClientVerifier::builder_with_provider(roots.clone(), provider.clone())
            .allow_unauthenticated()
            .build()
            .map_err(err)?;
        let mut server = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(err)?
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![tls.cert.clone(), tls.ca.clone()], tls.key.clone_key())
            .map_err(err)?;
        server.alpn_protocols = vec![ALPN.to_vec()];

        // any node of the cluster: which one, its hello says and its certificate must name (D-U19)
        let mut client = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(err)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(ClusterCa { ca: tls.ca.clone(), provider: provider.clone() }))
            .with_client_auth_cert(vec![tls.cert.clone()], tls.key.clone_key())
            .map_err(err)?;
        client.alpn_protocols = vec![ALPN.to_vec()];

        let mut invite = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(err)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCert { provider: provider.clone() }))
            .with_client_auth_cert(vec![tls.cert.clone()], tls.key.clone_key())
            .map_err(err)?;
        invite.alpn_protocols = vec![ALPN.to_vec()];

        let mut transport = quinn::TransportConfig::default();
        transport.keep_alive_interval(Some(KEEP_ALIVE));
        transport.max_idle_timeout(Some(IDLE.try_into().map_err(err)?));
        // BBR: a bulk transfer over a slow line (home's uplink) does not fill the router's queue,
        // so the small answers beside it stay quick (load test 2026-11-17: Cubic, 66 → 306 ms)
        transport.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
        let transport = Arc::new(transport);
        let mut server = quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server).map_err(err)?));
        server.transport_config(transport.clone());
        let mut client = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(client).map_err(err)?));
        client.transport_config(transport.clone());
        let mut invite_client = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(invite).map_err(err)?));
        invite_client.transport_config(transport);

        let mut endpoint = Endpoint::server(server, addr).map_err(err)?;
        endpoint.set_default_client_config(client);
        Ok(Arc::new(Link {
            me: NodeId(tls.node.clone()),
            endpoint,
            ca_pem: pem_of(&tls.ca),
            invite_client,
            links: Mutex::new(BTreeMap::new()),
            pings: AtomicU64::new(0),
            joins: Mutex::new(None),
            calls: Mutex::new(None),
            inbox: Mutex::new(None),
            streams: Mutex::new(None),
        }))
    }

    /// Whether this node takes joins (it holds the CA: [`Link::take_joins`]).
    pub fn takes_joins(&self) -> bool {
        self.joins.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Dials a node waiting to join at `addr` (`osada join --listen`, [`wait_join`]): for a node
    /// that cannot reach this one (this one at home, behind NAT; that one an edge in the cloud).
    /// Shows it the CA; it answers with its join request, which is granted or refused as one that
    /// came in ([`Link::take_joins`]). The id of the node that joined.
    pub async fn invite(&self, addr: SocketAddr) -> Result<String, LinkError> {
        let handler = self.joins.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let handler = handler.ok_or_else(|| LinkError("this node takes no joins (it does not hold the CA)".into()))?;
        let connecting = self.endpoint.connect_with(self.invite_client.clone(), addr, "shelf-join").map_err(err)?;
        let conn = tokio::time::timeout(CONNECT, connecting).await.map_err(|_| LinkError("no answer".into()))?.map_err(err)?;
        let refuse = |why: String| {
            conn.close(REFUSED.into(), why.as_bytes());
            LinkError(why)
        };
        let (mut send, mut recv) = conn.open_bi().await.map_err(err)?;
        write_frame(&mut send, &Frame::Invite { by: self.me.0.clone(), ca_pem: self.ca_pem.clone() }).await?;
        let req = match tokio::time::timeout(IDLE, read_frame(&mut recv)).await {
            Ok(Ok(Frame::Join { node, token, public_key })) => JoinRequest { node, token, public_key },
            Ok(Ok(other)) => return Err(refuse(format!("expected a join request, got {other:?}"))),
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(refuse("nothing said".into())),
        };
        let grant = handler(req).map_err(&refuse)?;
        let node = grant.node.clone();
        write_frame(&mut send, &Frame::Joined { cert_pem: grant.cert_pem, ca_pem: grant.ca_pem, node: grant.node, by: self.me.0.clone() }).await?;
        send.finish().map_err(err)?;
        // the new node closes the connection once it has the answer
        let _ = tokio::time::timeout(IDLE, conn.closed()).await;
        Ok(node)
    }

    /// From now on, nodes without a certificate may ask this one for one.
    pub fn take_joins(&self, handler: JoinHandler) {
        *self.joins.lock().unwrap_or_else(|e| e.into_inner()) = Some(handler);
    }

    /// From now on, calls from other nodes go to `handler`.
    pub fn take_calls(&self, handler: CallHandler) {
        *self.calls.lock().unwrap_or_else(|e| e.into_inner()) = Some(handler);
    }

    /// From now on, streams of a kind other nodes open go to `handler`.
    pub fn take_streams(&self, handler: StreamHandler) {
        *self.streams.lock().unwrap_or_else(|e| e.into_inner()) = Some(handler);
    }

    /// Opens a stream of `kind` to `peer` with `head`: the caller writes the bytes after it, reads the
    /// answer's head ([`read_head`]) and the bytes after that.
    pub async fn open_stream(&self, peer: &NodeId, kind: &str, head: serde_json::Value) -> Result<(SendStream, RecvStream), LinkError> {
        let conn = self.connection(peer).ok_or_else(|| LinkError(format!("no link to {peer}")))?;
        let (mut send, recv) = conn.open_bi().await.map_err(err)?;
        write_frame(&mut send, &Frame::Stream { kind: kind.into(), head }).await?;
        Ok((send, recv))
    }

    /// From now on, messages from other nodes go to `inbox`.
    pub fn take_messages(&self, inbox: Inbox) {
        *self.inbox.lock().unwrap_or_else(|e| e.into_inner()) = Some(inbox);
    }

    /// Sends `bytes` to `peer` on a stream of its own, when there is a link
    /// (false when not). Whether it arrives, and in what order, is for the
    /// protocol above (replication sends again).
    pub fn send(&self, peer: &NodeId, bytes: Vec<u8>) -> bool {
        let Some(conn) = self.connection(peer) else { return false };
        tokio::spawn(async move {
            if let Ok(mut s) = conn.open_uni().await {
                if s.write_all(&bytes).await.is_ok() {
                    let _ = s.finish();
                }
            }
        });
        true
    }

    /// A request to `peer`, and its answer.
    pub async fn call(&self, peer: &NodeId, body: serde_json::Value) -> Result<serde_json::Value, LinkError> {
        let conn = self.connection(peer).ok_or_else(|| LinkError(format!("no link to {peer}")))?;
        let (mut send, mut recv) = conn.open_bi().await.map_err(err)?;
        write_frame(&mut send, &Frame::Call { body }).await?;
        send.finish().map_err(err)?;
        match tokio::time::timeout(CALL_TIMEOUT, read_frame(&mut recv)).await {
            Ok(Ok(Frame::Reply { body })) => Ok(body),
            Ok(Ok(other)) => Err(LinkError(format!("expected a reply, got {other:?}"))),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(LinkError(format!("{peer} did not answer"))),
        }
    }

    pub fn me(&self) -> &NodeId {
        &self.me
    }

    pub fn local_addr(&self) -> Result<SocketAddr, LinkError> {
        self.endpoint.local_addr().map_err(err)
    }

    /// Takes connections until the endpoint closes.
    pub fn spawn_accept(self: &Arc<Self>) {
        let link = self.clone();
        tokio::spawn(async move {
            while let Some(incoming) = link.endpoint.accept().await {
                let link = link.clone();
                tokio::spawn(async move {
                    if let Err(e) = link.clone().accepted(incoming).await {
                        eprintln!("{}: refused a link: {e}", link.me);
                    }
                });
            }
        });
    }

    async fn accepted(self: Arc<Self>, incoming: quinn::Incoming) -> Result<(), LinkError> {
        let conn = incoming.await.map_err(err)?;
        let refuse = |why: String| {
            conn.close(REFUSED.into(), why.as_bytes());
            LinkError(why)
        };
        let (mut send, mut recv) = tokio::time::timeout(IDLE, conn.accept_bi())
            .await
            .map_err(|_| refuse("nothing said".into()))?
            .map_err(err)?;
        let first = read_frame(&mut recv).await.map_err(|e| refuse(e.0))?;
        let node = match (peer_cert(&conn), first) {
            (Some(cert), Frame::Hello { node, .. }) => {
                if let Err(e) = names(&cert, &node) {
                    return Err(refuse(format!("says it is {node:?}, its certificate does not: {e}")));
                }
                if node == self.me.0 {
                    return Err(refuse("a link to myself".into()));
                }
                node
            }
            (None, Frame::Join { node, token, public_key }) => {
                let handler = self.joins.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let handler = handler.ok_or_else(|| refuse("this node takes no joins".into()))?;
                let grant = handler(JoinRequest { node: node.clone(), token, public_key }).map_err(&refuse)?;
                write_frame(&mut send, &Frame::Joined { cert_pem: grant.cert_pem, ca_pem: grant.ca_pem, node: grant.node, by: self.me.0.clone() }).await?;
                send.finish().map_err(err)?;
                // The new node closes the connection once it has the answer.
                let _ = tokio::time::timeout(IDLE, conn.closed()).await;
                return Ok(());
            }
            (None, _) => return Err(refuse("no certificate from the cluster".into())),
            (Some(_), _) => return Err(refuse("the first frame was no hello".into())),
        };
        write_frame(&mut send, &Frame::Hello { node: self.me.0.clone(), version: crate::VERSION.into() }).await?;
        send.finish().map_err(err)?;
        let peer = NodeId(node);
        if self.register(peer.clone(), conn.clone(), peer.clone()) {
            self.serve(peer, conn).await;
        }
        Ok(())
    }

    /// Dials `peer` at `addr`, unless a link to it is up already.
    pub async fn dial(self: &Arc<Self>, peer: &NodeId, addr: SocketAddr) -> Result<(), LinkError> {
        if self.connection(peer).is_some() {
            return Ok(());
        }
        self.dial_at(Some(peer), addr).await.map(|_| ())
    }

    /// Dials whichever node of the cluster is at `addr` (D-U19: ids are
    /// GUIDs a dialer does not always know); the node it is.
    pub async fn dial_any(self: &Arc<Self>, addr: SocketAddr) -> Result<NodeId, LinkError> {
        self.dial_at(None, addr).await
    }

    async fn dial_at(self: &Arc<Self>, expect: Option<&NodeId>, addr: SocketAddr) -> Result<NodeId, LinkError> {
        let sni = expect.map_or("shelf-node", |p| p.0.as_str());
        let conn = self.endpoint.connect(addr, sni).map_err(err)?.await.map_err(err)?;
        let refuse = |why: String| {
            conn.close(REFUSED.into(), why.as_bytes());
            LinkError(why)
        };
        let (mut send, mut recv) = conn.open_bi().await.map_err(err)?;
        write_frame(&mut send, &Frame::Hello { node: self.me.0.clone(), version: crate::VERSION.into() }).await?;
        send.finish().map_err(err)?;
        let node = match read_frame(&mut recv).await? {
            Frame::Hello { node, .. } => node,
            other => return Err(refuse(format!("expected a hello, got {other:?}"))),
        };
        // the CA signed its certificate (ClusterCa); it must name the node the hello says
        match peer_cert(&conn) {
            Some(cert) => names(&cert, &node).map_err(|e| refuse(format!("says it is {node:?}, its certificate does not: {e}")))?,
            None => return Err(refuse("no certificate".into())),
        }
        if let Some(p) = expect.filter(|p| p.0 != node) {
            return Err(refuse(format!("expected {p}, {node} answered")));
        }
        if node == self.me.0 {
            return Err(refuse("a link to myself".into()));
        }
        let peer = NodeId(node);
        if self.register(peer.clone(), conn.clone(), self.me.clone()) {
            let link = self.clone();
            let peer = peer.clone();
            tokio::spawn(async move { link.serve(peer, conn).await });
        }
        Ok(peer)
    }

    /// Dials `peer` whenever there is no link to it, for as long as the
    /// node runs: what the node at home does towards the public one.
    pub fn spawn_keep_dialing(self: &Arc<Self>, peer: NodeId, addr: SocketAddr) {
        self.spawn_keep_dialing_at(Some(peer), addr)
    }

    /// The same; without `peer`, whichever node of the cluster answers at
    /// `addr` (its id known from the first link on).
    pub fn spawn_keep_dialing_at(self: &Arc<Self>, peer: Option<NodeId>, addr: SocketAddr) {
        let link = self.clone();
        tokio::spawn(async move {
            let mut wait = Duration::from_millis(500);
            let mut known = peer;
            loop {
                let r = match &known {
                    Some(p) => link.dial(p, addr).await,
                    None => link.dial_any(addr).await.map(|p| known = Some(p)),
                };
                match r {
                    Ok(()) => wait = Duration::from_millis(500),
                    Err(e) => {
                        let who = known.as_ref().map_or_else(|| "a node".to_string(), |p| p.to_string());
                        eprintln!("{}: dialing {who} at {addr}: {e}", link.me);
                        wait = (wait * 2).min(Duration::from_secs(30));
                    }
                }
                tokio::time::sleep(wait).await;
            }
        });
    }

    /// Keeps `conn` as the link to `peer`, or closes it when the one there
    /// is to stay. Both ends decide alike: the connection dialed by the node
    /// whose name sorts first stays; between two dialed by the same node, the
    /// newer.
    fn register(&self, peer: NodeId, conn: Connection, dialer: NodeId) -> bool {
        let mut links = self.links.lock().unwrap_or_else(|e| e.into_inner());
        let first = (&self.me).min(&peer).clone();
        if let Some(old) = links.get(&peer) {
            if old.conn.close_reason().is_none() {
                if old.dialer == first && dialer != first {
                    conn.close(DUPLICATE.into(), b"duplicate link");
                    return false;
                }
                old.conn.close(DUPLICATE.into(), b"duplicate link");
            }
        }
        links.insert(peer, Live { conn, dialer });
        true
    }

    fn connection(&self, peer: &NodeId) -> Option<Connection> {
        let links = self.links.lock().unwrap_or_else(|e| e.into_inner());
        links.get(peer).filter(|l| l.conn.close_reason().is_none()).map(|l| l.conn.clone())
    }

    /// Each live link as QUIC counts it (D-U34): the bytes sent and received on its connection, and
    /// its path's round-trip time.
    pub fn counters(&self) -> Vec<(NodeId, u64, u64, Duration)> {
        let links = self.links.lock().unwrap_or_else(|e| e.into_inner());
        links
            .iter()
            .filter(|(_, l)| l.conn.close_reason().is_none())
            .map(|(p, l)| {
                let s = l.conn.stats();
                (p.clone(), s.udp_tx.bytes, s.udp_rx.bytes, s.path.rtt)
            })
            .collect()
    }

    /// Where a peer's link comes from, as this node sees it (behind NAT: its router's address).
    pub fn remote_of(&self, peer: &NodeId) -> Option<std::net::SocketAddr> {
        self.connection(peer).map(|c| c.remote_address())
    }

    /// The nodes there is a live link to.
    pub fn peers(&self) -> Vec<NodeId> {
        let links = self.links.lock().unwrap_or_else(|e| e.into_inner());
        links.iter().filter(|(_, l)| l.conn.close_reason().is_none()).map(|(p, _)| p.clone()).collect()
    }

    /// Answers what the other side opens on `conn`, until it closes.
    async fn serve(self: Arc<Self>, peer: NodeId, conn: Connection) {
        loop {
            tokio::select! {
                bi = conn.accept_bi() => {
                    let Ok((send, recv)) = bi else { break };
                    let (link, peer) = (self.clone(), peer.clone());
                    tokio::spawn(async move { link.answer(&peer, send, recv).await });
                }
                uni = conn.accept_uni() => {
                    let Ok(mut recv) = uni else { break };
                    let inbox = self.inbox.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let peer = peer.clone();
                    tokio::spawn(async move {
                        if let (Some(inbox), Ok(bytes)) = (inbox, recv.read_to_end(MAX_MESSAGE).await) {
                            let _ = inbox.send((peer, bytes));
                        }
                    });
                }
            }
        }
        let mut links = self.links.lock().unwrap_or_else(|e| e.into_inner());
        if links.get(&peer).is_some_and(|l| l.conn.stable_id() == conn.stable_id()) {
            links.remove(&peer);
        }
    }

    async fn answer(&self, peer: &NodeId, mut send: SendStream, mut recv: RecvStream) {
        let reply = match read_frame(&mut recv).await {
            Ok(Frame::Stream { kind, head }) => {
                let handler = self.streams.lock().unwrap_or_else(|e| e.into_inner()).clone();
                match handler {
                    Some(h) => h(peer.clone(), kind, head, send, recv).await,
                    None => {
                        let _ = send.reset(REFUSED.into());
                    }
                }
                return;
            }
            Ok(Frame::Ping { n }) => Frame::Pong { n },
            Ok(Frame::Call { body }) => {
                let handler = self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone();
                match handler {
                    Some(h) => Frame::Reply { body: h(peer, body) },
                    None => return,
                }
            }
            _ => return,
        };
        if write_frame(&mut send, &reply).await.is_ok() {
            let _ = send.finish();
        }
    }

    /// A round trip to `peer` over its link.
    pub async fn ping(&self, peer: &NodeId) -> Result<Duration, LinkError> {
        let conn = self.connection(peer).ok_or_else(|| LinkError(format!("no link to {peer}")))?;
        let n = self.pings.fetch_add(1, Ordering::Relaxed);
        let start = Instant::now();
        let (mut send, mut recv) = conn.open_bi().await.map_err(err)?;
        write_frame(&mut send, &Frame::Ping { n }).await?;
        send.finish().map_err(err)?;
        match read_frame(&mut recv).await? {
            Frame::Pong { n: m } if m == n => Ok(start.elapsed()),
            other => Err(LinkError(format!("expected pong {n}, got {other:?}"))),
        }
    }

    /// Closes every link and stops listening.
    pub fn close(&self) {
        self.endpoint.close(0u32.into(), b"node stopping");
    }

    /// The same, and waits (up to a second) until the peers were told: they know at once that
    /// this node is gone, instead of when the link times out (an edge then answers from its copies).
    pub async fn close_and_wait(&self) {
        self.close();
        let _ = tokio::time::timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
    }
}

/// As a node without a certificate: asks `peer` at `addr` for one (§6.2),
/// trusting it only if its certificate comes from the CA whose fingerprint
/// the token carries.
pub async fn request_join(
    addr: SocketAddr,
    ca_fingerprint: &str,
    req: JoinRequest,
) -> Result<JoinGrant, LinkError> {
    let fingerprint: [u8; 32] = hex::decode(ca_fingerprint)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| LinkError("not a CA fingerprint".into()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut client = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(err)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedCa { fingerprint, provider }))
        .with_no_client_auth();
    client.alpn_protocols = vec![ALPN.to_vec()];
    let client = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(client).map_err(err)?));
    let local: SocketAddr = if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }.parse().expect("an address");
    let endpoint = Endpoint::client(local).map_err(err)?;
    let conn = endpoint.connect_with(client, addr, "shelf-node").map_err(err)?.await.map_err(err)?;
    let (mut send, mut recv) = conn.open_bi().await.map_err(err)?;
    write_frame(&mut send, &Frame::Join { node: req.node, token: req.token, public_key: req.public_key }).await?;
    send.finish().map_err(err)?;
    let answer = read_frame(&mut recv).await;
    let why = match conn.close_reason() {
        Some(quinn::ConnectionError::ApplicationClosed(c)) => String::from_utf8_lossy(&c.reason).into_owned(),
        _ => String::new(),
    };
    conn.close(0u32.into(), b"thanks");
    endpoint.wait_idle().await;
    match answer {
        Ok(Frame::Joined { cert_pem, ca_pem, node, by }) => {
            let got = crate::pki::fingerprint(&ca_pem).map_err(err)?;
            if got != ca_fingerprint {
                return Err(LinkError("the CA sent is not the token's".into()));
            }
            Ok(JoinGrant { cert_pem, ca_pem, node, by })
        }
        Ok(other) => Err(LinkError(format!("unexpected answer {other:?}"))),
        Err(e) if !why.is_empty() => Err(LinkError(format!("refused: {why} ({e})"))),
        Err(e) => Err(e),
    }
}

/// As a node without a certificate: waits at `listen` (UDP) for the cluster's CA node to dial
/// ([`Link::invite`]; it was told this node's address with the token), and asks it for a
/// certificate there: the join the other way round, for a node the CA's node can reach but not
/// the reverse (an edge in the cloud, home behind NAT). The dialer is trusted once it shows the
/// CA whose fingerprint the token carries and a certificate that CA signed for it; only then is
/// the token's secret sent. Dialers that are not are refused, and the wait goes on.
pub async fn wait_join(listen: SocketAddr, ca_fingerprint: &str, req: JoinRequest) -> Result<JoinGrant, LinkError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let me = rcgen::generate_simple_self_signed(vec!["shelf-join".to_string()]).map_err(err)?;
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(me.key_pair.serialize_der().into());
    let mut server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(err)?
        .with_client_cert_verifier(Arc::new(AnyCert { provider: provider.clone() }))
        .with_single_cert(vec![me.cert.der().clone()], key)
        .map_err(err)?;
    server.alpn_protocols = vec![ALPN.to_vec()];
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(IDLE.try_into().map_err(err)?));
    let mut server = quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server).map_err(err)?));
    server.transport_config(Arc::new(transport));
    let endpoint = Endpoint::server(server, listen).map_err(|e| LinkError(format!("listening on {listen}: {e}")))?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<JoinGrant, LinkError>>();
    let fp = ca_fingerprint.to_string();
    let result = loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break Err(LinkError("stopped listening".into())) };
                let (tx, fp, req, provider) = (tx.clone(), fp.clone(), req.clone(), provider.clone());
                tokio::spawn(async move {
                    let from = incoming.remote_address();
                    match answer_invite(incoming, &fp, req, &provider).await {
                        Ok(grant) => { let _ = tx.send(Ok(grant)); }
                        // the cluster itself said no (a used or expired token): no use waiting on
                        Err(Invite::Refused(why)) => { let _ = tx.send(Err(LinkError(format!("refused by the cluster: {why}")))); }
                        Err(Invite::Stranger(why)) => eprintln!("join: {from} dialed, not the token's cluster: {why}"),
                        // the cluster's node dialing as to a node (before it invites), scanners
                        Err(Invite::Handshake) => {}
                    }
                });
            }
            Some(r) = rx.recv() => break r,
        }
    };
    // the answer's connection closes cleanly; a stranger's still open does not hold this up
    let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.wait_idle()).await;
    endpoint.close(0u32.into(), b"joined");
    result
}

enum Invite {
    /// The token's cluster, which refused the join.
    Refused(String),
    /// Not the token's cluster, or no proper invitation.
    Stranger(String),
    /// No TLS handshake.
    Handshake,
}

async fn answer_invite(incoming: quinn::Incoming, fp: &str, req: JoinRequest, provider: &rustls::crypto::CryptoProvider) -> Result<JoinGrant, Invite> {
    use rustls::pki_types::pem::PemObject;
    let stranger = |e: String| Invite::Stranger(e);
    let conn = incoming.await.map_err(|_| Invite::Handshake)?;
    let refuse = |why: String| {
        conn.close(REFUSED.into(), why.as_bytes());
        Invite::Stranger(why)
    };
    let (mut send, mut recv) = match tokio::time::timeout(IDLE, conn.accept_bi()).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(stranger(e.to_string())),
        Err(_) => return Err(refuse("nothing said".into())),
    };
    let (by, ca_pem) = match tokio::time::timeout(IDLE, read_frame(&mut recv)).await {
        Ok(Ok(Frame::Invite { by, ca_pem })) => (by, ca_pem),
        Ok(Ok(_)) => return Err(refuse("this node waits to join a cluster; it takes no links".into())),
        Ok(Err(e)) => return Err(refuse(e.0)),
        Err(_) => return Err(refuse("nothing said".into())),
    };
    // the token's CA, and a certificate of the dialer's that it signed
    if crate::pki::fingerprint(&ca_pem).ok().as_deref() != Some(fp) {
        return Err(refuse("the CA shown is not the token's".into()));
    }
    let ca = CertificateDer::from_pem_slice(ca_pem.as_bytes()).map_err(|e| refuse(e.to_string()))?;
    let cert = peer_cert(&conn).ok_or_else(|| refuse("no certificate".into()))?;
    let now = rustls::pki_types::UnixTime::now();
    chain_to(&ca, &cert, provider, now, webpki::KeyUsage::client_auth()).map_err(|e| refuse(e.to_string()))?;
    names(&cert, &by).map_err(|e| refuse(format!("says it is {by:?}, its certificate does not: {e}")))?;
    write_frame(&mut send, &Frame::Join { node: req.node, token: req.token, public_key: req.public_key })
        .await
        .map_err(|e| stranger(e.0))?;
    let _ = send.finish();
    let answer = tokio::time::timeout(IDLE, read_frame(&mut recv)).await;
    let why = match conn.close_reason() {
        Some(quinn::ConnectionError::ApplicationClosed(c)) => String::from_utf8_lossy(&c.reason).into_owned(),
        _ => String::new(),
    };
    conn.close(0u32.into(), b"thanks");
    match answer {
        Ok(Ok(Frame::Joined { cert_pem, ca_pem, node, by })) => {
            if crate::pki::fingerprint(&ca_pem).ok().as_deref() != Some(fp) {
                return Err(Invite::Stranger("the CA sent is not the token's".into()));
            }
            Ok(JoinGrant { cert_pem, ca_pem, node, by })
        }
        Ok(Ok(other)) => Err(Invite::Stranger(format!("unexpected answer {other:?}"))),
        Ok(Err(e)) if !why.is_empty() => Err(Invite::Refused(format!("{why} ({e})"))),
        Ok(Err(e)) => Err(Invite::Stranger(e.0)),
        Err(_) => Err(Invite::Stranger("no answer".into())),
    }
}

/// A certificate as PEM.
fn pem_of(der: &CertificateDer<'_>) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(der.as_ref());
    let mut out = String::from("-----BEGIN CERTIFICATE-----\n");
    for line in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out
}

/// Takes any certificate whose key signs the handshake, for the invitation only: the dialer of a
/// waiting node (whose certificate is self-signed), and as that node, the dialer's (checked
/// against the token's CA after the handshake, in [`answer_invite`]).
#[derive(Debug)]
struct AnyCert {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

impl rustls::server::danger::ClientCertVerifier for AnyCert {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        Ok(rustls::server::danger::ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Trusts a server whose certificate the cluster CA `ca` signed, whatever
/// node it names: which node it is, its hello says and `names` checks after
/// the handshake, as for nodes that dial in (D-U19).
#[derive(Debug)]
struct ClusterCa {
    ca: CertificateDer<'static>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

/// Whether the CA `ca` signed `ee` for `usage` (a server, a client), now.
fn chain_to(ca: &CertificateDer<'_>, ee: &CertificateDer<'_>, provider: &rustls::crypto::CryptoProvider, now: rustls::pki_types::UnixTime, usage: webpki::KeyUsage) -> Result<(), rustls::Error> {
    let anchor = webpki::anchor_from_trusted_cert(ca).map_err(|e| rustls::Error::General(format!("the CA: {e}")))?;
    let cert = webpki::EndEntityCert::try_from(ee).map_err(|e| rustls::Error::General(format!("the certificate: {e}")))?;
    cert.verify_for_usage(provider.signature_verification_algorithms.all, &[anchor], &[], now, usage, None, None)
        .map(|_| ())
        .map_err(|e| rustls::Error::General(format!("not a node of this cluster: {e}")))
}

impl rustls::client::danger::ServerCertVerifier for ClusterCa {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        chain_to(&self.ca, end_entity, &self.provider, now, webpki::KeyUsage::server_auth()).map(|()| rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Trusts a server whose certificate chain holds the CA with this
/// fingerprint, and whose certificate that CA signed (for any node: a
/// joining node does not know the GUIDs, D-U19).
#[derive(Debug)]
struct PinnedCa {
    fingerprint: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedCa {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        use sha2::{Digest, Sha256};
        let ca = intermediates
            .iter()
            .find(|c| Sha256::digest(c.as_ref()).as_slice() == self.fingerprint)
            .ok_or_else(|| rustls::Error::General("the node's CA is not the token's".into()))?;
        let _ = (server_name, ocsp_response);
        chain_to(ca, end_entity, &self.provider, now, webpki::KeyUsage::server_auth()).map(|()| rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// The certificate the other side presented, which rustls has checked
/// against the cluster CA.
fn peer_cert(conn: &Connection) -> Option<CertificateDer<'static>> {
    let certs = conn.peer_identity()?.downcast::<Vec<CertificateDer<'static>>>().ok()?;
    certs.into_iter().next()
}

/// Whether `cert` names the node `node`.
fn names(cert: &CertificateDer<'_>, node: &str) -> Result<(), LinkError> {
    let ee = webpki::EndEntityCert::try_from(cert).map_err(err)?;
    let name = ServerName::try_from(node).map_err(err)?;
    ee.verify_is_valid_for_subject_name(&name).map_err(err)
}

async fn write_frame(send: &mut SendStream, frame: &Frame) -> Result<(), LinkError> {
    let body = serde_json::to_vec(frame).map_err(err)?;
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&(body.len() as u32).to_be_bytes());
    buf.extend_from_slice(&body);
    send.write_all(&buf).await.map_err(err)
}

async fn read_frame(recv: &mut RecvStream) -> Result<Frame, LinkError> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await.map_err(err)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(LinkError(format!("a frame of {len} bytes")));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.map_err(err)?;
    serde_json::from_slice(&body).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pki::{Ca, NodeCert, P256PublicKey};

    fn n(s: &str) -> NodeId {
        NodeId(s.into())
    }

    fn local() -> SocketAddr {
        "127.0.0.1:0".parse().unwrap()
    }

    fn node(ca: &Ca, name: &str) -> Arc<Link> {
        let link = Link::bind(&ca.issue(name).unwrap().tls().unwrap(), local()).unwrap();
        link.spawn_accept();
        link
    }

    async fn until(what: &str, f: impl Fn() -> bool) {
        for _ in 0..200 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}");
    }

    /// home dials edge; edge, which dialed nobody, reaches home over the
    /// same connection.
    #[tokio::test]
    async fn either_side_uses_the_link() {
        let ca = Ca::new().unwrap();
        let (home, edge) = (node(&ca, "home"), node(&ca, "edge"));
        home.dial(&n("edge"), edge.local_addr().unwrap()).await.unwrap();
        until("edge to see the link", || edge.peers() == [n("home")]).await;
        edge.ping(&n("home")).await.unwrap();
        home.ping(&n("edge")).await.unwrap();
        // Dialing again while the link is up changes nothing.
        home.dial(&n("edge"), edge.local_addr().unwrap()).await.unwrap();
        assert_eq!((home.peers(), edge.peers()), (vec![n("edge")], vec![n("home")]));
    }

    #[tokio::test]
    async fn strangers_are_refused() {
        let ca = Ca::new().unwrap();
        let edge = node(&ca, "edge");
        let addr = edge.local_addr().unwrap();

        // Another cluster's node: its certificate is not from our CA.
        let stranger = node(&Ca::new().unwrap(), "home");
        assert!(stranger.dial(&n("edge"), addr).await.is_err());

        // The right cluster, the wrong name: edge's certificate does not
        // name "mail".
        let home = node(&ca, "home");
        assert!(home.dial(&n("mail"), addr).await.is_err());

        // A node of the cluster saying it is another one.
        let mut liar = ca.issue("eve").unwrap().tls().unwrap();
        liar.node = "home".into();
        let liar = Link::bind(&liar, local()).unwrap();
        assert!(liar.dial(&n("edge"), addr).await.is_err());

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(edge.peers().is_empty());
        home.dial(&n("edge"), addr).await.unwrap();
        home.ping(&n("edge")).await.unwrap();
    }

    #[tokio::test]
    async fn both_dialing_at_once_leaves_one_link() {
        let ca = Ca::new().unwrap();
        let (a, b) = (node(&ca, "home"), node(&ca, "edge"));
        let (to_b, to_a) = (n("edge"), n("home"));
        let (ra, rb) = tokio::join!(a.dial(&to_b, b.local_addr().unwrap()), b.dial(&to_a, a.local_addr().unwrap()));
        // One of the two may lose its dial to the other's; never both.
        assert!(ra.is_ok() || rb.is_ok(), "{ra:?} {rb:?}");
        until("one link each way", || a.peers() == [n("edge")] && b.peers() == [n("home")]).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        a.ping(&n("edge")).await.unwrap();
        b.ping(&n("home")).await.unwrap();
    }

    /// D-U19: a node dialed by its address alone; its hello and certificate say which it is.
    #[tokio::test]
    async fn any_node_of_the_cluster_at_an_address() {
        let ca = Ca::new().unwrap();
        let id = crate::placement::NodeId::fresh();
        let a = node(&ca, &id.0);
        let addr = a.local_addr().unwrap();
        let b = node(&ca, "b");
        assert_eq!(b.dial_any(addr).await.unwrap(), id);
        b.ping(&id).await.unwrap();
        until("a to see b", || a.peers() == [n("b")]).await;
        // the address, but another node expected there
        let c = node(&ca, "c");
        assert!(c.dial(&n("someone-else"), addr).await.unwrap_err().0.contains("answered"));
        // a node of another cluster is not taken, either way
        let stranger = node(&Ca::new().unwrap(), "s");
        assert!(stranger.dial_any(addr).await.is_err());
        assert!(c.dial_any(stranger.local_addr().unwrap()).await.is_err());
        // keep dialing an address: one link, its id learned
        let d = node(&ca, "d");
        d.spawn_keep_dialing_at(None, addr);
        until("d linked to a", || d.peers() == [id.clone()]).await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(d.peers(), [id.clone()]);
    }

    #[tokio::test]
    async fn a_node_joins_with_a_token() {
        let ca = Arc::new(Ca::new().unwrap());
        let home = node(&ca, "home");
        let issuer = ca.clone();
        home.take_joins(Arc::new(move |req: JoinRequest| {
            if req.token != "s3cret" {
                return Err("unknown or used token".into());
            }
            let key = P256PublicKey::from_hex(&req.public_key).map_err(|e| e.to_string())?;
            let cert_pem = issuer.issue_for(&req.node, &key).map_err(|e| e.to_string())?;
            Ok(JoinGrant { cert_pem, ca_pem: issuer.cert_pem.clone(), node: req.node.clone(), by: String::new() })
        }));
        let addr = home.local_addr().unwrap();
        let fp = ca.fingerprint().unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let req = |token: &str| JoinRequest { node: "edge".into(), token: token.into(), public_key: hex::encode(key.public_key_raw()) };

        let e = request_join(addr, &fp, req("guess")).await.unwrap_err();
        assert!(e.0.contains("unknown or used token"), "{e}");
        // A node that is not of the token's cluster is not trusted.
        let other = Ca::new().unwrap().fingerprint().unwrap();
        assert!(request_join(addr, &other, req("s3cret")).await.is_err());

        // The right token: a certificate for the key the new node kept, and
        // with it a link.
        let grant = request_join(addr, &fp, req("s3cret")).await.unwrap();
        assert_eq!(grant.by, "home");
        let cert = NodeCert { node: grant.node, cert_pem: grant.cert_pem, key_pem: key.serialize_pem(), ca_pem: grant.ca_pem };
        let edge = Link::bind(&cert.tls().unwrap(), local()).unwrap();
        edge.spawn_accept();
        edge.dial(&n("home"), addr).await.unwrap();
        edge.ping(&n("home")).await.unwrap();
        until("home to see edge", || home.peers() == [n("edge")]).await;
        home.ping(&n("edge")).await.unwrap();
    }

    /// The join the other way round: the new node waits, the CA's node dials it.
    #[tokio::test]
    async fn a_waiting_node_is_invited() {
        let ca = Arc::new(Ca::new().unwrap());
        let home = node(&ca, "home");
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let (issuer, s) = (ca.clone(), seen.clone());
        home.take_joins(Arc::new(move |req: JoinRequest| {
            s.lock().unwrap().push(req.token.clone());
            if req.token != "s3cret" {
                return Err("unknown or used token".into());
            }
            let key = P256PublicKey::from_hex(&req.public_key).map_err(|e| e.to_string())?;
            let cert_pem = issuer.issue_for(&req.node, &key).map_err(|e| e.to_string())?;
            Ok(JoinGrant { cert_pem, ca_pem: issuer.cert_pem.clone(), node: req.node.clone(), by: String::new() })
        }));
        // another cluster's CA node, which takes joins as well
        let other_ca = Arc::new(Ca::new().unwrap());
        let other = node(&other_ca, "stranger");
        let (oi, os) = (other_ca.clone(), seen.clone());
        other.take_joins(Arc::new(move |req: JoinRequest| {
            os.lock().unwrap().push(format!("stranger got {}", req.token));
            let key = P256PublicKey::from_hex(&req.public_key).map_err(|e| e.to_string())?;
            Ok(JoinGrant { cert_pem: oi.issue_for(&req.node, &key).map_err(|e| e.to_string())?, ca_pem: oi.cert_pem.clone(), node: req.node.clone(), by: String::new() })
        }));
        let fp = ca.fingerprint().unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let req = |token: &str| JoinRequest { node: "edge".into(), token: token.into(), public_key: hex::encode(key.public_key_raw()) };
        let free = || {
            let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            s.local_addr().unwrap()
        };

        // a node of another cluster is refused before the token is said; the wait goes on, and a
        // refusal by the token's cluster ends it
        let at = free();
        let waiting = tokio::spawn({
            let (fp, r) = (fp.clone(), req("guess"));
            async move { wait_join(at, &fp, r).await }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(other.invite(at).await.is_err());
        let e = home.invite(at).await.unwrap_err();
        assert!(e.0.contains("unknown or used token"), "{e}");
        let e = waiting.await.unwrap().unwrap_err();
        assert!(e.0.contains("refused by the cluster") && e.0.contains("unknown or used token"), "{e}");
        assert_eq!(*seen.lock().unwrap(), ["guess"], "the stranger never heard a token");

        // the right token: a certificate, and with it a link either way
        let at = free();
        let waiting = tokio::spawn({
            let (fp, r) = (fp.clone(), req("s3cret"));
            async move { wait_join(at, &fp, r).await }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(home.invite(at).await.unwrap(), "edge");
        let grant = waiting.await.unwrap().unwrap();
        assert_eq!(grant.by, "home");
        let cert = NodeCert { node: grant.node, cert_pem: grant.cert_pem, key_pem: key.serialize_pem(), ca_pem: grant.ca_pem };
        let edge = Link::bind(&cert.tls().unwrap(), at).unwrap();
        edge.spawn_accept();
        home.dial(&n("edge"), at).await.unwrap();
        edge.ping(&n("home")).await.unwrap();
        home.ping(&n("edge")).await.unwrap();
    }
}
