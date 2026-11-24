//! What an edge node keeps (D-U11), from osada-edge (same author; it was MIT,
//! here AGPL-3.0 with the rest): copies of file bodies (`cache`, role cache),
//! and public links' answers, pages and static files for when home is away
//! (`keep`, role public-cache).

pub mod cache;
pub mod keep;

/// Not passed on in either direction, nor kept. The x-shelf ones are the
/// edge's own words to home; a visitor does not get to say them.
const DROP: &[&str] = &[
    "connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "proxy-connection", "te", "trailer",
    "transfer-encoding", "upgrade", "host", "x-real-ip", "x-forwarded-for", "x-forwarded-proto", "x-forwarded-host",
];

pub fn dropped(name: &str) -> bool {
    DROP.contains(&name) || name.starts_with("x-shelf-")
}
