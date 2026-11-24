//! Mail a person sends through osada (Round 21): a public link to an address that is no user of
//! this instance. It goes out from the person's own mailbox, as they would send it themselves (it
//! is in their Sent folder after), through the mail server's JMAP: osada is the OpenID provider
//! the mail server trusts for the webmail, so osada signs a short access token for the person
//! (client `maildot`, the webmail's; scope `openid email`) and speaks JMAP with it.
//!
//! Where the mail server is: the mail instance's public name (`https://<host>`), or
//! `OSADA_MAIL_JMAP_URL` (the tests' mock server).

use serde_json::{json, Value};

use crate::osada1::{auth::User, oidc, App};

/// The webmail's client: the mail server takes osada's tokens for it.
pub const CLIENT: &str = "maildot";

pub struct Letter {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
}

/// The mail server's base URL, if there is one.
pub fn base(app: &App) -> Option<String> {
    if let Ok(u) = std::env::var("OSADA_MAIL_JMAP_URL") {
        return Some(u.trim_end_matches('/').to_string());
    }
    app.bridge.as_ref()?.mail_host().map(|h| format!("https://{h}"))
}

/// The address the person sends from (their mailbox), if they have one.
pub fn from_address(app: &App, user: &User) -> Option<String> {
    let (sub, mail): (Option<String>, Option<String>) =
        app.db.opt(|c| c.query_row("SELECT sub, mail FROM users WHERE id = ?1", [user.id], |r| Ok((r.get(0)?, r.get(1)?)))).ok().flatten().unwrap_or((None, None));
    sub.and_then(|s| app.bridge.as_ref().and_then(|b| b.mailbox_of(&s))).or(mail)
}

fn err_of(v: &Value) -> Option<String> {
    for r in v["methodResponses"].as_array().into_iter().flatten() {
        if r[0] == "error" {
            return Some(format!("{}: {}", r[1]["type"].as_str().unwrap_or("error"), r[1]["description"].as_str().unwrap_or("")));
        }
        for k in ["notCreated", "notUpdated", "notDestroyed"] {
            if let Some(m) = r[1][k].as_object().filter(|m| !m.is_empty()) {
                let (_, e) = m.iter().next().unwrap_or((&String::new(), &Value::Null)).clone();
                return Some(format!("{}: {}", e["type"].as_str().unwrap_or("not done"), e["description"].as_str().unwrap_or("")));
            }
        }
    }
    None
}

/// A JMAP session at the mail server as a person: a short token osada signs for the webmail's
/// client with the scope asked for, then the session's API address and accounts. (Round 24: the
/// sending above and the mail app passwords, apppw.rs, both speak through it.)
pub struct Jmap {
    http: reqwest::Client,
    api: String,
    token: String,
    pub session: Value,
}

impl Jmap {
    pub async fn open(app: &App, user: &User, scope: &str) -> Result<Jmap, String> {
        let base = base(app).ok_or("there is no mail server")?;
        let token = oidc::mint_access(app, user, CLIENT, scope, 300).map_err(|e| format!("{e:?}"))?;
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
        let r = http.get(format!("{base}/.well-known/jmap")).bearer_auth(&token).send().await.map_err(|e| format!("the mail server: {e}"))?;
        let r = r.error_for_status().map_err(|e| format!("the mail server: {e}"))?;
        let session: Value = serde_json::from_slice(&r.bytes().await.map_err(|e| e.to_string())?).map_err(|e| format!("the mail server's session: {e}"))?;
        let api = session["apiUrl"].as_str().ok_or("the mail server gave no JMAP address")?.to_string();
        Ok(Jmap { http, api, token, session })
    }

    /// The person's account for the first of `caps` the session names.
    pub fn account(&self, caps: &[&str]) -> Option<String> {
        caps.iter().find_map(|c| self.session["primaryAccounts"][*c].as_str()).map(str::to_string)
    }

    /// One request; a method's error, or something not created, updated or destroyed, in words.
    pub async fn call(&self, using: &[&str], calls: Value) -> Result<Value, String> {
        let body = json!({ "using": using, "methodCalls": calls });
        let r = self.http.post(&self.api).bearer_auth(&self.token).header("content-type", "application/json").body(body.to_string()).send().await.map_err(|e| e.to_string())?;
        let r = r.error_for_status().map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_slice(&r.bytes().await.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        match err_of(&v) {
            Some(e) => Err(e),
            None => Ok(v),
        }
    }
}

/// Sends `l` from `user`'s mailbox; an error says why not (in words for the page).
pub async fn send_as(app: &App, user: &User, display: &str, l: &Letter) -> Result<(), String> {
    base(app).ok_or("there is no mail server to send through")?;
    let from = from_address(app, user).ok_or("you have no mailbox to send it from")?;
    let j = Jmap::open(app, user, "openid email").await?;
    let account = j.account(&["urn:ietf:params:jmap:submission", "urn:ietf:params:jmap:mail"]).ok_or("no mail account at the mail server")?;
    const USING: &[&str] = &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", "urn:ietf:params:jmap:submission"];
    let v = j
        .call(USING, json!([
            ["Identity/get", { "accountId": account }, "i"],
            ["Mailbox/get", { "accountId": account, "properties": ["role", "name"] }, "m"],
        ]))
        .await?;
    let ids = v["methodResponses"][0][1]["list"].as_array().cloned().unwrap_or_default();
    let identity = ids
        .iter()
        .find(|i| i["email"].as_str().is_some_and(|e| e.eq_ignore_ascii_case(&from)))
        .or(ids.first())
        .and_then(|i| i["id"].as_str())
        .ok_or("no identity to send as at the mail server")?
        .to_string();
    let boxes = v["methodResponses"][1][1]["list"].as_array().cloned().unwrap_or_default();
    let role = |r: &str| boxes.iter().find(|b| b["role"] == r).and_then(|b| b["id"].as_str()).map(str::to_string);
    let drafts = role("drafts").ok_or("no Drafts folder at the mail server")?;
    let sent = role("sent");
    let mut after = json!({ "keywords/$draft": null, format!("mailboxIds/{drafts}"): null });
    if let Some(s) = &sent {
        after[format!("mailboxIds/{s}")] = json!(true);
    }
    j.call(USING, json!([
        ["Email/set", { "accountId": account, "create": { "e": {
            "mailboxIds": { drafts.clone(): true }, "keywords": { "$draft": true, "$seen": true },
            "from": [{ "name": display, "email": from }], "to": [{ "email": l.to }], "subject": l.subject,
            "bodyValues": { "t": { "value": l.text }, "h": { "value": l.html } },
            "textBody": [{ "partId": "t", "type": "text/plain" }], "htmlBody": [{ "partId": "h", "type": "text/html" }],
        } } }, "0"],
        ["EmailSubmission/set", { "accountId": account, "create": { "s": { "identityId": identity, "emailId": "#e" } },
            "onSuccessUpdateEmail": { "#s": after } }, "1"],
    ]))
    .await?;
    Ok(())
}

/// A time (seconds since 1970, UTC) as YYYY-MM-DD (Howard Hinnant's days-to-civil).
fn ymd(t: i64) -> String {
    let z = t.div_euclid(86400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The letter for a public link: who shares what, the link, until when.
pub fn link_letter(to: &str, who: &str, item: &str, url: &str, expires: Option<i64>, site: &str) -> Letter {
    let until = expires.map(|e| format!(" until {}", ymd(e)));
    let until = until.unwrap_or_default();
    Letter {
        to: to.to_string(),
        subject: format!("{who} shared \u{201c}{item}\u{201d} with you"),
        text: format!("{who} shared \u{201c}{item}\u{201d} with you{until}:\n\n{url}\n\nAnyone with the link can open it (view only, no account needed).\n\n-- \nsent with osada ({site})\n"),
        html: format!(
            "<p>{w} shared <b>{i}</b> with you{u}:</p><p><a href=\"{l}\">{l}</a></p><p style=\"color:#777\">Anyone with the link can open it (view only, no account needed).</p><p style=\"color:#777;font-size:smaller\">sent with osada ({s})</p>",
            w = esc(who), i = esc(item), u = esc(&until), l = esc(url), s = esc(site)
        ),
    }
}

/// An address as typed: `local@domain.tld`, lower case; None when it is not one.
pub fn address(s: &str) -> Option<String> {
    let s = s.trim().trim_start_matches('<').trim_end_matches('>').to_lowercase();
    let (local, domain) = s.split_once('@')?;
    let ok_local = !local.is_empty() && local.len() <= 64 && local.chars().all(|c| c.is_ascii_alphanumeric() || "._%+-'".contains(c)) && !local.starts_with('.') && !local.ends_with('.');
    let labels: Vec<&str> = domain.split('.').collect();
    let ok_domain = labels.len() >= 2
        && domain.len() <= 253
        && labels.iter().all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_alphanumeric() || c == '-'))
        && labels.last().is_some_and(|t| t.len() >= 2 && !t.chars().all(|c| c.is_ascii_digit()));
    (ok_local && ok_domain).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_and_letters() {
        assert_eq!(address(" Anna@Example.org ").as_deref(), Some("anna@example.org"));
        assert_eq!(address("<a.b+c@sub.example.co.uk>").as_deref(), Some("a.b+c@sub.example.co.uk"));
        for bad in ["anna", "anna@", "@example.org", "anna@example", "a b@example.org", "anna@-x.org", "anna@1.2.3.4", ".a@example.org", "a@@example.org"] {
            assert_eq!(address(bad), None, "{bad}");
        }
        let l = link_letter("x@example.org", "Vladimír", "photos <2026>", "https://osada.example.org/s/abc", Some(1_800_000_000), "osada.example.org");
        assert!(l.subject.contains("Vladimír shared") && l.text.contains("https://osada.example.org/s/abc") && l.text.contains("until 2027-01-15"));
        assert!(l.html.contains("photos &lt;2026&gt;") && !l.html.contains("<2026>"));
        assert_eq!((ymd(0), ymd(951_782_400), ymd(1_800_000_000)), ("1970-01-01".into(), "2000-02-29".into(), "2027-01-15".into()));
    }
}
