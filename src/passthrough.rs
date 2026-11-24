//! TLS passed on as it comes (D-U16): a connection to an edge's https port
//! whose ClientHello names a host that a mail instance of this node serves
//! (Stalwart, for now) goes to that server untouched, after a PROXY protocol
//! v2 header with the visitor's address. The server keeps its own
//! certificate (its ACME TLS-ALPN-01 challenges pass through too) and sees
//! who connects, so its own checks (allowed networks per account, bans)
//! keep working. Everything else is osada's and goes on to rustls.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use tokio::{io::AsyncWriteExt, net::TcpStream};

/// The upstream (`host:port`) for a host name, if this node passes it on.
pub type Pass = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// What the bytes so far say about the server name.
#[derive(Debug, PartialEq)]
pub enum Hello {
    /// A complete ClientHello, with the name it asks for (lower case).
    Name(Option<String>),
    /// Not TLS, or not a ClientHello: rustls says no.
    NotTls,
    /// More bytes are needed.
    More,
}

/// The largest ClientHello looked at (a record is at most 16 KiB; with
/// post-quantum key shares a hello is a few KiB).
const MAX_HELLO: usize = 32 * 1024;

/// The server name of the ClientHello at the start of `buf`, the handshake
/// put together from as many records as it takes.
pub fn server_name(buf: &[u8]) -> Hello {
    let mut hs = Vec::new();
    let mut at = 0;
    loop {
        if buf.len() < at + 5 {
            return Hello::More;
        }
        if buf[at] != 0x16 || buf[at + 1] != 3 {
            return Hello::NotTls;
        }
        let len = u16::from_be_bytes([buf[at + 3], buf[at + 4]]) as usize;
        if len == 0 || len > 1 << 14 {
            return Hello::NotTls;
        }
        if buf.len() < at + 5 + len {
            return Hello::More;
        }
        hs.extend_from_slice(&buf[at + 5..at + 5 + len]);
        at += 5 + len;
        if hs.len() >= 4 {
            if hs[0] != 1 {
                return Hello::NotTls;
            }
            let need = 4 + (u32::from_be_bytes([0, hs[1], hs[2], hs[3]]) as usize);
            if need > MAX_HELLO {
                return Hello::NotTls;
            }
            if hs.len() >= need {
                return match sni(&hs[4..need]) {
                    Some(n) => Hello::Name(n),
                    None => Hello::NotTls,
                };
            }
        }
    }
}

/// The host_name of the server_name extension of a ClientHello body; None
/// when the body does not parse.
fn sni(b: &[u8]) -> Option<Option<String>> {
    let mut r = Reader(b);
    r.take(2 + 32)?; // version, random
    let n = r.u8()? as usize;
    r.take(n)?; // session id
    let n = r.u16()? as usize;
    r.take(n)?; // cipher suites
    let n = r.u8()? as usize;
    r.take(n)?; // compression
    if r.0.is_empty() {
        return Some(None); // no extensions
    }
    let n = r.u16()? as usize;
    let mut ext = Reader(r.take(n)?);
    while !ext.0.is_empty() {
        let (kind, n) = (ext.u16()?, ext.u16()? as usize);
        let body = ext.take(n)?;
        if kind != 0 {
            continue;
        }
        let mut l = Reader(body);
        let n = l.u16()? as usize;
        let mut names = Reader(l.take(n)?);
        while !names.0.is_empty() {
            let (t, n) = (names.u8()?, names.u16()? as usize);
            let name = names.take(n)?;
            if t == 0 {
                let s = std::str::from_utf8(name).ok()?;
                return Some(Some(s.trim_end_matches('.').to_ascii_lowercase()));
            }
        }
        return Some(None);
    }
    Some(None)
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
}

/// A PROXY protocol v2 header for a TCP connection from `src` to `dst`
/// (both as IPv4 when both are, IPv4-mapped addresses taken as IPv4).
pub fn proxy_v2(src: SocketAddr, dst: SocketAddr) -> Vec<u8> {
    let mut h = b"\r\n\r\n\0\r\nQUIT\n".to_vec();
    h.push(0x21); // version 2, PROXY
    let (s, d) = (src.ip().to_canonical(), dst.ip().to_canonical());
    match (s, d) {
        (std::net::IpAddr::V4(s), std::net::IpAddr::V4(d)) => {
            h.push(0x11);
            h.extend_from_slice(&12u16.to_be_bytes());
            h.extend_from_slice(&s.octets());
            h.extend_from_slice(&d.octets());
        }
        _ => {
            let v6 = |a: std::net::IpAddr| match a {
                std::net::IpAddr::V4(a) => a.to_ipv6_mapped(),
                std::net::IpAddr::V6(a) => a,
            };
            h.push(0x21);
            h.extend_from_slice(&36u16.to_be_bytes());
            h.extend_from_slice(&v6(s).octets());
            h.extend_from_slice(&v6(d).octets());
        }
    }
    h.extend_from_slice(&src.port().to_be_bytes());
    h.extend_from_slice(&dst.port().to_be_bytes());
    h
}

/// The server name a new connection asks for, read without taking the
/// bytes (they stay for rustls or the upstream). None: not TLS, or nothing
/// complete within 5 s.
pub async fn peek_name(s: &TcpStream) -> Option<String> {
    let mut buf = vec![0u8; MAX_HELLO + 5 * 3];
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        let mut last = 0;
        loop {
            let n = s.peek(&mut buf).await.ok()?;
            if n == 0 {
                return None;
            }
            match server_name(&buf[..n]) {
                Hello::Name(name) => return name,
                Hello::NotTls => return None,
                Hello::More if n == buf.len() => return None,
                Hello::More => {}
            }
            // peek returns at once while anything is waiting: wait for more
            if n == last {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            last = n;
        }
    })
    .await;
    got.ok().flatten()
}

/// The connection to `upstream`, the PROXY header first, then both ways
/// until either side closes; the bytes from the visitor and to it.
pub async fn splice(mut s: TcpStream, peer: SocketAddr, upstream: &str) -> (u64, u64) {
    let local = s.local_addr().unwrap_or(peer);
    let up = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(upstream)).await;
    let mut up = match up {
        Ok(Ok(u)) => u,
        Ok(Err(e)) => {
            tracing::warn!("passthrough {peer} → {upstream}: {e}");
            return (0, 0);
        }
        Err(_) => {
            tracing::warn!("passthrough {peer} → {upstream}: no answer in 5 s");
            return (0, 0);
        }
    };
    let _ = up.set_nodelay(true);
    if up.write_all(&proxy_v2(peer, local)).await.is_err() {
        return (0, 0);
    }
    tokio::io::copy_bidirectional(&mut s, &mut up).await.unwrap_or((0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ClientHello naming `name`, cut into records of at most `cut` bytes.
    fn hello(name: Option<&str>, cut: usize) -> Vec<u8> {
        let mut body = vec![3, 3];
        body.extend([7u8; 32]);
        body.push(0); // session id
        body.extend([0, 2, 0x13, 0x01]); // one suite
        body.extend([1, 0]); // null compression
        let mut ext = Vec::new();
        ext.extend([0x00, 0x0b, 0, 2, 1, 0]); // ec_point_formats, before the name
        if let Some(n) = name {
            let n = n.as_bytes();
            let mut list = vec![0u8];
            list.extend((n.len() as u16).to_be_bytes());
            list.extend(n);
            let mut e = (list.len() as u16).to_be_bytes().to_vec();
            e.extend(list);
            ext.extend([0, 0]);
            ext.extend((e.len() as u16).to_be_bytes());
            ext.extend(e);
        }
        ext.extend([0x00, 0x10, 0, 5, 0, 3, 2, b'h', b'2']); // alpn
        body.extend((ext.len() as u16).to_be_bytes());
        body.extend(ext);
        let mut hs = vec![1];
        hs.extend(&(body.len() as u32).to_be_bytes()[1..]);
        hs.extend(body);
        let mut out = Vec::new();
        for c in hs.chunks(cut) {
            out.extend([0x16, 3, 1]);
            out.extend((c.len() as u16).to_be_bytes());
            out.extend(c);
        }
        out
    }

    #[test]
    fn names_from_hellos() {
        let h = hello(Some("Mail.Example.org"), 1 << 14);
        assert_eq!(server_name(&h), Hello::Name(Some("mail.example.org".into())));
        assert_eq!(server_name(&hello(None, 1 << 14)), Hello::Name(None));
        // in pieces: more until the whole of it is there
        for n in [0, 3, 5, 20, h.len() - 1] {
            assert_eq!(server_name(&h[..n]), Hello::More, "{n}");
        }
        // a hello over several records
        let h = hello(Some("osada.example.org"), 20);
        assert!(h.len() > 4 * 25, "five records");
        assert_eq!(server_name(&h), Hello::Name(Some("osada.example.org".into())));
        assert_eq!(server_name(&h[..h.len() - 1]), Hello::More);
        // not TLS
        assert_eq!(server_name(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"), Hello::NotTls);
        assert_eq!(server_name(&[0x16, 3, 1, 0, 4, 2, 0, 0, 0]), Hello::NotTls);
    }

    #[test]
    fn proxy_headers() {
        let h = proxy_v2("192.0.2.7:51000".parse().unwrap(), "[::ffff:198.51.100.1]:443".parse().unwrap());
        assert_eq!(&h[..12], b"\r\n\r\n\0\r\nQUIT\n");
        assert_eq!(&h[12..16], &[0x21, 0x11, 0, 12]);
        assert_eq!(&h[16..24], &[192, 0, 2, 7, 198, 51, 100, 1]);
        assert_eq!(&h[24..28], &[0xc7, 0x38, 0x01, 0xbb]);
        let h = proxy_v2("[2001:db8::7]:51000".parse().unwrap(), "[2001:db8::1]:443".parse().unwrap());
        assert_eq!(&h[12..16], &[0x21, 0x21, 0, 36]);
        assert_eq!(h.len(), 16 + 36);
    }

    #[tokio::test]
    async fn passes_on_untouched() {
        use tokio::io::AsyncReadExt;
        let up = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up_addr = up.local_addr().unwrap().to_string();
        let front = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let front_addr = front.local_addr().unwrap();
        tokio::spawn(async move {
            let (s, peer) = front.accept().await.unwrap();
            assert_eq!(peek_name(&s).await.as_deref(), Some("mail.example.org"));
            splice(s, peer, &up_addr).await;
        });
        let h = hello(Some("mail.example.org"), 50);
        let mut c = TcpStream::connect(front_addr).await.unwrap();
        // the hello in two writes, apart
        c.write_all(&h[..30]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        c.write_all(&h[30..]).await.unwrap();
        let (mut u, _) = up.accept().await.unwrap();
        let mut got = vec![0u8; 28 + h.len()];
        u.read_exact(&mut got).await.unwrap();
        assert_eq!(&got[..16], &[b'\r', b'\n', b'\r', b'\n', 0, b'\r', b'\n', b'Q', b'U', b'I', b'T', b'\n', 0x21, 0x11, 0, 12]);
        assert_eq!(&got[16..20], &[127, 0, 0, 1]);
        assert_eq!(&got[28..], &h[..]);
        // and back
        u.write_all(b"server hello").await.unwrap();
        let mut back = [0u8; 12];
        c.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"server hello");
    }
}
