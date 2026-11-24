//! TOTP (RFC 6238; §10.2): six digits from HMAC-SHA1 over 30-second steps,
//! as every authenticator app does it. A code is taken from one step before
//! to one after (clocks differ), and never twice: the step it was for has to
//! be later than the last one used.

use ring::hmac;

pub const STEP: u64 = 30;
pub const DIGITS: u32 = 6;
pub const SECRET_LEN: usize = 20;

/// The code for `step` (seconds / 30).
pub fn code(secret: &[u8], step: u64) -> u32 {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let mac = hmac::sign(&key, &step.to_be_bytes());
    let h = mac.as_ref();
    let at = (h[h.len() - 1] & 0x0f) as usize;
    let n = u32::from_be_bytes([h[at] & 0x7f, h[at + 1], h[at + 2], h[at + 3]]);
    n % 10u32.pow(DIGITS)
}

/// The step a typed code is good for at `now` (seconds since 1970), if it
/// is one and comes after `last` (the last step used).
pub fn check(secret: &[u8], typed: &str, now: u64, last: Option<u64>) -> Option<u64> {
    let typed: String = typed.chars().filter(|c| !c.is_whitespace()).collect();
    if typed.len() != DIGITS as usize || !typed.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let want: u32 = typed.parse().ok()?;
    let now = now / STEP;
    [now.saturating_sub(1), now, now + 1]
        .into_iter()
        .filter(|s| last.is_none_or(|l| *s > l))
        .find(|s| code(secret, *s) == want)
}

/// RFC 4648 base32 without padding: how apps take a secret typed by hand.
pub fn base32(bytes: &[u8]) -> String {
    const ABC: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0);
    for &b in bytes {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            out.push(ABC[((buf >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(ABC[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn uri_escape(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// What an authenticator app scans: `otpauth://totp/…`.
pub fn uri(issuer: &str, account: &str, secret: &[u8]) -> String {
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits={DIGITS}&period={STEP}",
        uri_escape(issuer),
        uri_escape(account),
        base32(secret),
        uri_escape(issuer)
    )
}

/// The URI as a QR code, SVG.
pub fn qr_svg(text: &str) -> Option<String> {
    let qr = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium).ok()?;
    let n = qr.size();
    let border = 3;
    let mut path = String::new();
    for y in 0..n {
        for x in 0..n {
            if qr.get_module(x, y) {
                path.push_str(&format!("M{},{}h1v1h-1z", x + border, y + border));
            }
        }
    }
    let side = n + 2 * border;
    Some(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {side} {side}\" shape-rendering=\"crispEdges\">\
         <rect width=\"100%\" height=\"100%\" fill=\"#fff\"/><path d=\"{path}\" fill=\"#000\"/></svg>"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238, appendix B, SHA1 (the last six of the eight digits).
    #[test]
    fn rfc_6238_vectors() {
        let secret = b"12345678901234567890";
        for (t, want) in [(59u64, 94_287_082u32), (1_111_111_109, 7_081_804), (1_234_567_890, 89_005_924), (2_000_000_000, 69_279_037)] {
            assert_eq!(code(secret, t / STEP), want % 1_000_000, "T={t}");
        }
    }

    #[test]
    fn a_code_counts_once_and_only_near_its_time() {
        let secret = b"12345678901234567890";
        let now = 1_234_567_890;
        let typed = format!("{:06}", code(secret, now / STEP));
        let step = check(secret, &typed, now, None).unwrap();
        assert_eq!(check(secret, &typed, now, Some(step)), None);
        assert_eq!(check(secret, &typed, now + 31, None), Some(step));
        assert_eq!(check(secret, &typed, now + 95, None), None);
        assert_eq!(check(secret, "12345", now, None), None);
        assert_eq!(check(secret, &format!("{} {}", &typed[..3], &typed[3..]), now, None), Some(step));
    }

    #[test]
    fn base32_and_uri() {
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32(b"12345678901234567890"), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        let u = uri("osada", "anna@home", b"12345678901234567890");
        assert!(u.starts_with("otpauth://totp/osada:anna%40home?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=osada"));
        assert!(qr_svg(&u).unwrap().starts_with("<svg"));
    }
}
