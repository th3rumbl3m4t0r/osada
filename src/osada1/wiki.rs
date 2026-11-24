//! The wiki: articles every user reads and writes. An article (`wiki_pages`) has
//! an address of its own (`slug`: a random id, nothing to do with its title, so
//! titles in any language and alike names are no trouble) and a text in each
//! language somebody wrote it in; every save is a revision (`wiki_revs`) and the
//! newest one of a language is its text. Pictures belong to the article, all of
//! its languages: files in `<data>/wiki/<id>/` (in the backup), listed in
//! `wiki_media`. A text's markdown shows them by name (`![](plan.png)`) and refers
//! to other articles by address (`[see](k3x9q2pa)`); a reference to an address
//! nobody wrote yet is fine: it shows as missing and offers to write it.
//! An article may hold a family tree (`wiki_trees`, every save a revision too):
//! people with their dates, a picture of the article's and the address of their
//! own article, and the unions between them with their children. Exported, it
//! comes with its people's articles, their texts as written and, instead of their
//! pictures, where those are and what the texts say they show. Where its cards go is
//! worked out once, by whoever saves a change (tree.js), and kept with the revision,
//! so a page only draws it.
//! Removing an article is for whoever started it, a language for whoever wrote it
//! first, a picture for whoever put it there; admins may remove anything.
//! Anybody may pin an article to the top of the list, for everybody (`pinned`).

use std::{
    collections::BTreeMap,
    fs,
    path::Path as FsPath,
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        Arc,
    },
};

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Response,
    routing::{get, post, put},
    Json, Router,
};
use bytes::Bytes;
use futures_util::TryStreamExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::osada1::{
    auth::User,
    db::now,
    error::{AppError, Res},
    http_util, md,
    users::LANGS,
    vfs::{local::Local, Backend, ByteStream, RelPath},
    App,
};

const TITLE_MAX: usize = 200;
const TEXT_MAX: usize = 1 << 20;
const PICTURE_MAX: u64 = 32 << 20;
const PICTURES: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "avif", "svg"];
const TREE_MAX: usize = 1 << 20;
const LAYOUT_MAX: usize = 4 << 20;
/// what new addresses are made of: no 0/o, 1/l
const ID_CHARS: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/wiki", get(list).post(create))
        .route("/api/wiki/{slug}", get(article).delete(remove))
        .route("/api/wiki/{slug}/export", get(export))
        .route("/api/wiki/{slug}/history", get(history))
        .route("/api/wiki/{slug}/pin", put(pin))
        .route("/api/wiki/{slug}/rev/{id}", get(revision))
        .route("/api/wiki/{slug}/text/{lang}", put(save).delete(remove_lang))
        .route("/api/wiki/{slug}/tree", put(save_tree))
        .route("/api/wiki/{slug}/tree/{id}", get(tree_revision))
        .route("/api/wiki/{slug}/media", post(add_picture))
        .route("/api/wiki/{slug}/media/{name}", get(picture).delete(remove_picture))
}

fn pictures(app: &App) -> Local {
    Local::new(app.cfg.data.join("wiki"), app.tmp_dir())
}

/// an address: letters a-z, digits, `-` inside; "new" is the page that writes one
pub fn valid_id(s: &str) -> bool {
    (1..=60).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s != "new"
}

fn new_id() -> String {
    use rand::Rng;
    let mut r = rand::thread_rng();
    (0..8).map(|_| ID_CHARS[r.gen_range(0..ID_CHARS.len())] as char).collect()
}

/// "Jak sdílet soubory?" -> "jak-sdilet-soubory": the address of an imported article, from its folder
pub fn slug_of(title: &str) -> String {
    let mut s = String::new();
    for c in title.to_lowercase().chars() {
        let t = match c {
            'a'..='z' | '0'..='9' => {
                s.push(c);
                continue;
            }
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' | 'ă' | 'ą' => "a",
            'č' | 'ć' | 'ç' => "c",
            'ď' | 'đ' => "d",
            'é' | 'è' | 'ê' | 'ë' | 'ě' | 'ę' | 'ē' => "e",
            'í' | 'ì' | 'î' | 'ï' => "i",
            'ľ' | 'ĺ' | 'ł' => "l",
            'ň' | 'ń' | 'ñ' => "n",
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ő' | 'ø' => "o",
            'ř' | 'ŕ' => "r",
            'š' | 'ś' | 'ş' => "s",
            'ť' | 'ţ' => "t",
            'ú' | 'ù' | 'û' | 'ü' | 'ů' | 'ű' => "u",
            'ý' | 'ÿ' => "y",
            'ž' | 'ź' | 'ż' => "z",
            'ß' => "ss",
            'æ' => "ae",
            'œ' => "oe",
            _ => "-",
        };
        s.push_str(t);
    }
    let mut out = String::new();
    for part in s.split('-').filter(|p| !p.is_empty()) {
        if out.len() + part.len() > 60 {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(part);
    }
    if out.is_empty() {
        "article".into()
    } else {
        out
    }
}

/// a picture's name as it is kept: letters, digits, `.`, `-`, `_` (not first); one of PICTURES
fn picture_name(name: &str) -> Res<String> {
    let name = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    let (stem, ext) = name.rsplit_once('.').ok_or_else(|| AppError::bad("only pictures (png, jpg, gif, webp, avif, svg)"))?;
    let ext = ext.to_ascii_lowercase();
    if !PICTURES.contains(&ext.as_str()) {
        return Err(AppError::bad("only pictures (png, jpg, gif, webp, avif, svg)"));
    }
    let stem: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' }).take(80).collect();
    let stem = stem.trim_matches(['-', '.']);
    Ok(format!("{}.{ext}", if stem.is_empty() { "picture" } else { stem }))
}

fn check_lang(lang: &str) -> Res<()> {
    if LANGS.contains(&lang) {
        Ok(())
    } else {
        Err(AppError::bad("unknown language"))
    }
}

fn check_text(title: &str, body: &str) -> Res<()> {
    if title.trim().is_empty() {
        return Err(AppError::bad("a title is needed"));
    }
    if title.len() > TITLE_MAX || body.len() > TEXT_MAX {
        return Err(AppError::bad("the article is too long"));
    }
    Ok(())
}

struct Page {
    id: i64,
    created_by: Option<i64>,
    pinned: bool,
}

fn page(c: &Connection, slug: &str) -> rusqlite::Result<Option<Page>> {
    c.query_row("SELECT id, created_by, pinned FROM wiki_pages WHERE slug = ?1", [slug], |r| {
        Ok(Page { id: r.get(0)?, created_by: r.get(1)?, pinned: r.get(2)? })
    })
    .optional()
}

fn find(app: &App, slug: &str) -> Res<Page> {
    app.db.with(|c| page(c, slug))?.ok_or_else(AppError::not_found)
}

/// every article's title in each of its languages, by address
type Titles = std::collections::HashMap<String, BTreeMap<String, String>>;

fn titles(c: &Connection) -> rusqlite::Result<Titles> {
    let mut st = c.prepare(
        "SELECT p.slug, r.lang, r.title FROM wiki_pages p JOIN wiki_revs r ON r.page_id = p.id
         WHERE r.id = (SELECT MAX(id) FROM wiki_revs m WHERE m.page_id = r.page_id AND m.lang = r.lang)",
    )?;
    let mut out = Titles::new();
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
        let (slug, lang, title) = row?;
        out.entry(slug).or_default().insert(lang, title);
    }
    Ok(out)
}

fn render(slug: &str, body: &str, known: &Titles) -> String {
    md::mark_refs(&md::render(body, Some(&format!("/api/wiki/{slug}/media/")), Some("/wiki/")), |id| known.contains_key(id))
}

/// references to articles in some other markdown (a file's, the editors' preview): marked like the wiki's own
pub fn mark(app: &App, html: String) -> String {
    if !html.contains("href=\"/wiki/") {
        return html;
    }
    match app.db.with(|c| titles(c)) {
        Ok(known) => md::mark_refs(&html, |id| known.contains_key(id)),
        Err(_) => html,
    }
}

/// the newest revision of every language of a page: lang -> (rev, title, body, author, created, first author id, started)
type Texts = BTreeMap<String, (i64, String, String, String, i64, Option<i64>, i64)>;

fn texts(c: &Connection, page_id: i64) -> rusqlite::Result<Texts> {
    let mut st = c.prepare(
        "SELECT r.lang, r.id, r.title, r.body, r.author, r.created,
                (SELECT author_id FROM wiki_revs f WHERE f.page_id = r.page_id AND f.lang = r.lang ORDER BY f.id LIMIT 1),
                (SELECT MIN(created) FROM wiki_revs f WHERE f.page_id = r.page_id AND f.lang = r.lang)
         FROM wiki_revs r
         WHERE r.page_id = ?1 AND r.id = (SELECT MAX(id) FROM wiki_revs m WHERE m.page_id = r.page_id AND m.lang = r.lang)",
    )?;
    let rows = st.query_map([page_id], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))))?;
    rows.collect()
}

#[derive(Deserialize)]
struct ListQ {
    q: Option<String>,
}

/// every article with the title and time of each language, and whether it is pinned; `q`: only those with it in a title or text
async fn list(State(app): State<Arc<App>>, _user: User, Query(lq): Query<ListQ>) -> Res<Json<Value>> {
    let q = lq.q.map(|q| q.trim().to_lowercase()).filter(|q| !q.is_empty());
    let pages = app.db.with(|c| {
        let mut st = c.prepare(
            "SELECT p.id, p.slug, p.created, p.pinned, r.lang, r.title, r.body, r.author, r.created
             FROM wiki_pages p JOIN wiki_revs r ON r.page_id = p.id
             WHERE r.id = (SELECT MAX(id) FROM wiki_revs m WHERE m.page_id = r.page_id AND m.lang = r.lang)
             ORDER BY p.id",
        )?;
        let mut out: Vec<(i64, String, i64, bool, Vec<(String, String, String, String, i64)>)> = Vec::new();
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, bool>(3)?,
                (r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?),
            ))
        })?;
        for row in rows {
            let (id, slug, created, pinned, t) = row?;
            match out.last_mut() {
                Some(p) if p.0 == id => p.4.push(t),
                _ => out.push((id, slug, created, pinned, vec![t])),
            }
        }
        Ok(out)
    })?;
    // and the people of family trees
    let in_trees: std::collections::HashSet<i64> = match &q {
        Some(q) => app.db.with(|c| {
            let mut st = c.prepare(
                "SELECT page_id, data FROM wiki_trees t WHERE id = (SELECT MAX(id) FROM wiki_trees m WHERE m.page_id = t.page_id)",
            )?;
            let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            let mut hit = std::collections::HashSet::new();
            for row in rows {
                let (id, data) = row?;
                let v: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
                let found = v["people"].as_object().into_iter().flatten().any(|(_, p)| {
                    ["name", "desc"].iter().any(|k| p[k].as_str().is_some_and(|s| s.to_lowercase().contains(q.as_str())))
                });
                if found {
                    hit.insert(id);
                }
            }
            Ok(hit)
        })?,
        None => Default::default(),
    };
    let list: Vec<Value> = pages
        .into_iter()
        .filter(|(id, _, _, _, ts)| {
            q.as_ref().is_none_or(|q| in_trees.contains(id) || ts.iter().any(|t| t.1.to_lowercase().contains(q) || t.2.to_lowercase().contains(q)))
        })
        .map(|(_, slug, created, pinned, ts)| {
            let texts: serde_json::Map<String, Value> =
                ts.into_iter().map(|(lang, title, _, author, at)| (lang, json!({ "title": title, "author": author, "updated": at }))).collect();
            json!({ "slug": slug, "created": created, "pinned": pinned, "texts": texts })
        })
        .collect();
    Ok(Json(Value::Array(list)))
}

#[derive(Deserialize)]
struct NewReq {
    title: String,
    lang: String,
    body: String,
    /// the address a reference already points at; none: a new one
    id: Option<String>,
}

async fn create(State(app): State<Arc<App>>, user: User, Json(req): Json<NewReq>) -> Res<Json<Value>> {
    check_lang(&req.lang)?;
    check_text(&req.title, &req.body)?;
    let title = req.title.trim();
    let want = req.id.filter(|i| !i.is_empty());
    if want.as_deref().is_some_and(|i| !valid_id(i)) {
        return Err(AppError::bad("not a usable address"));
    }
    let slug = app.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        let slug = match &want {
            Some(i) if page(&tx, i)?.is_some() => return Ok(None),
            Some(i) => i.clone(),
            None => loop {
                let i = new_id();
                if page(&tx, &i)?.is_none() {
                    break i;
                }
            },
        };
        let t = now();
        tx.execute("INSERT INTO wiki_pages (slug, created_by, created) VALUES (?1, ?2, ?3)", params![slug, user.id, t])?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO wiki_revs (page_id, lang, title, body, author_id, author, created, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '')",
            params![id, req.lang, title, req.body, user.id, user.username, t],
        )?;
        tx.commit()?;
        Ok(Some(slug))
    })?;
    // somebody wrote it meanwhile: the reference leads there now
    let slug = slug.ok_or_else(|| AppError::conflict("there is an article at this address already"))?;
    Ok(Json(json!({ "slug": slug })))
}

/// an article in all its languages, rendered, with its pictures; `mine`: may be removed by this user
async fn article(State(app): State<Arc<App>>, user: User, Path(slug): Path<String>) -> Res<Json<Value>> {
    let p = find(&app, &slug)?;
    let (ts, media, by, known, tree) = app.db.with(|c| {
        let ts = texts(c, p.id)?;
        let known = titles(c)?;
        let tree = current_tree(c, p.id)?;
        let mut st = c.prepare("SELECT name, size, author_id, author, created FROM wiki_media WHERE page_id = ?1 ORDER BY created, name")?;
        let media = st
            .query_map([p.id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let by: Option<String> = match p.created_by {
            Some(id) => c.query_row("SELECT username FROM users WHERE id = ?1", [id], |r| r.get(0)).optional()?,
            None => None,
        };
        Ok((ts, media, by, known, tree))
    })?;
    let owner = user.admin || p.created_by == Some(user.id);
    let texts: serde_json::Map<String, Value> = ts
        .into_iter()
        .map(|(lang, (rev, title, body, author, at, first, started))| {
            let html = render(&slug, &body, &known);
            let v = json!({ "rev": rev, "title": title, "body": body, "html": html, "author": author, "updated": at,
                "started": started, "mine": owner || first == Some(user.id) });
            (lang, v)
        })
        .collect();
    let media: Vec<Value> = media
        .into_iter()
        .map(|(name, size, aid, author, at)| json!({ "name": name, "size": size, "author": author, "created": at, "mine": owner || aid == Some(user.id) }))
        .collect();
    // the tree's people lead to their articles: the titles of those there are, null for those yet to be written
    // a removed tree has its revision still: the next one is made from it
    let (tree, refs) = match tree {
        Some((rev, None, author, at, _)) => (json!({ "rev": rev, "data": null, "author": author, "updated": at }), json!({})),
        Some((rev, Some(data), author, at, layout)) => {
            let refs: serde_json::Map<String, Value> = data["people"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(_, p)| p["ref"].as_str())
                .map(|r| (r.to_string(), known.get(r).map_or(Value::Null, |t| json!(t))))
                .collect();
            (json!({ "rev": rev, "data": data, "author": author, "updated": at, "layout": layout }), Value::Object(refs))
        }
        None => (Value::Null, json!({})),
    };
    Ok(Json(json!({ "slug": slug, "by": by, "mine": owner, "pinned": p.pinned, "texts": texts, "media": media, "tree": tree, "refs": refs })))
}

/// the article, its family tree and the articles of the tree's people, to keep elsewhere
async fn export(State(app): State<Arc<App>>, _user: User, Path(slug): Path<String>) -> Res<Json<Value>> {
    let p = find(&app, &slug)?;
    let base = app.cfg.public_url.as_deref().unwrap_or("");
    let v = app.db.with(|c| {
        let article = described(c, base, &slug, p.id)?;
        let mut articles = serde_json::Map::new();
        let tree = match current_tree(c, p.id)? {
            Some((rev, data, author, at, _)) => {
                let refs = data.iter().flat_map(|d| d["people"].as_object()).flatten().filter_map(|(_, p)| p["ref"].as_str());
                for r in refs {
                    if r == slug || articles.contains_key(r) {
                        continue;
                    }
                    if let Some(q) = page(c, r)? {
                        articles.insert(r.to_string(), described(c, base, r, q.id)?);
                    }
                }
                json!({ "rev": rev, "data": data, "author": author, "updated": at })
            }
            None => Value::Null,
        };
        Ok(json!({ "exported": now(), "article": article, "tree": tree, "articles": articles }))
    })?;
    Ok(Json(v))
}

/// an article's texts as written, and its pictures: where each is and the words the texts show it with
/// (a picture's alt text, the text of a link to it), not the pictures themselves
fn described(c: &Connection, base: &str, slug: &str, page_id: i64) -> rusqlite::Result<Value> {
    let ts = texts(c, page_id)?;
    let mut st = c.prepare("SELECT name, size, author, created FROM wiki_media WHERE page_id = ?1 ORDER BY created, name")?;
    let media = st
        .query_map([page_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let at = format!("/api/wiki/{slug}/media/");
    let said: Vec<(&String, Vec<(bool, String, String)>)> = ts.iter().map(|(lang, t)| (lang, md::targets(&t.2))).collect();
    let pictures: Vec<Value> = media
        .into_iter()
        .map(|(name, size, author, created)| {
            let shows: serde_json::Map<String, Value> = said
                .iter()
                .filter_map(|(lang, ts)| {
                    let mut words: Vec<&str> = Vec::new();
                    for (_, url, w) in ts {
                        let w = w.trim();
                        if (*url == name || url.strip_prefix(at.as_str()) == Some(name.as_str())) && !w.is_empty() && !words.contains(&w) {
                            words.push(w);
                        }
                    }
                    (!words.is_empty()).then(|| (lang.to_string(), json!(words)))
                })
                .collect();
            json!({ "name": name, "url": format!("{base}{at}{name}"), "size": size, "author": author, "added": created, "shows": shows })
        })
        .collect();
    let texts: serde_json::Map<String, Value> = ts
        .into_iter()
        .map(|(lang, (rev, title, body, author, at, _, _))| (lang, json!({ "rev": rev, "title": title, "body": body, "author": author, "updated": at })))
        .collect();
    Ok(json!({ "slug": slug, "url": format!("{base}/wiki/{slug}"), "texts": texts, "pictures": pictures }))
}

/// the newest tree of a page: (rev, data, None when it was removed, author, created, where its cards go: null
/// when it was saved without)
type Tree = (i64, Option<Value>, String, i64, Value);

fn current_tree(c: &Connection, page_id: i64) -> rusqlite::Result<Option<Tree>> {
    let row: Option<(i64, String, String, i64, Option<String>)> = c
        .query_row(
            "SELECT id, data, author, created, layout FROM wiki_trees WHERE page_id = ?1 ORDER BY id DESC LIMIT 1",
            [page_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    Ok(row.map(|(rev, data, author, at, layout)| {
        let v: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
        let layout = layout.and_then(|l| serde_json::from_str(&l).ok()).unwrap_or(Value::Null);
        (rev, (!v.is_null()).then_some(v), author, at, layout)
    }))
}

/// people {id: {name, desc, born, died, pic, ref}} and unions [{a, b, kids}]; null: no tree
fn check_tree(v: &Value) -> Res<()> {
    if v.is_null() {
        return Ok(());
    }
    let bad = || AppError::bad("not a family tree");
    let people = v.get("people").and_then(Value::as_object).ok_or_else(bad)?;
    let unions = v.get("unions").and_then(Value::as_array).ok_or_else(bad)?;
    if people.len() > 5000 || unions.len() > 5000 {
        return Err(AppError::bad("the family tree is too big"));
    }
    for (id, p) in people {
        let p = p.as_object().ok_or_else(bad)?;
        let fields_ok = p.values().all(|f| f.is_null() || f.as_str().is_some_and(|s| s.len() <= 2000));
        let ref_ok = p.get("ref").and_then(Value::as_str).is_none_or(valid_id);
        if id.is_empty() || id.len() > 40 || !p.get("name").is_some_and(|n| n.as_str().is_some_and(|n| !n.trim().is_empty())) || !fields_ok || !ref_ok {
            return Err(bad());
        }
    }
    let person = |v: &Value| v.as_str().is_some_and(|i| people.contains_key(i));
    for u in unions {
        let kids_ok = u.get("kids").and_then(Value::as_array).is_some_and(|k| k.iter().all(person));
        if !u.get("a").is_some_and(person) || !u.get("b").is_none_or(|b| b.is_null() || person(b)) || !kids_ok {
            return Err(bad());
        }
    }
    Ok(())
}

/// where the cards of `data` go, as tree.js laid it out for the save: {pos: {person: {x, y}}, w, h,
/// paths: [{cls, d}]}, with a place for exactly the tree's people; anything else is not kept, and the
/// pages lay that tree out themselves
fn check_layout(data: &Value, l: &Value) -> Option<String> {
    let people = data.get("people")?.as_object()?;
    let pos = l.get("pos")?.as_object()?;
    let num = |v: &Value| v.as_f64().is_some_and(|f| f.is_finite() && f.abs() < 1e7);
    let place = |p: &Value| p.get("x").is_some_and(num) && p.get("y").is_some_and(num);
    let path = |p: &Value| {
        p.get("cls").and_then(Value::as_str).is_some_and(|c| (1..=20).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'))
            && p.get("d").and_then(Value::as_str).is_some_and(|d| d.bytes().all(|b| b"MVHA0123456789 .-".contains(&b)))
    };
    let ok = pos.len() == people.len()
        && people.keys().all(|k| pos.get(k).is_some_and(place))
        && l.get("w").is_some_and(num)
        && l.get("h").is_some_and(num)
        && l.get("paths")?.as_array()?.iter().all(path);
    let s = l.to_string();
    (ok && s.len() <= LAYOUT_MAX).then_some(s)
}

#[derive(Deserialize)]
struct TreeReq {
    data: Value,
    /// the tree revision this one was made from; 0: there was none
    base: i64,
    #[serde(default)]
    note: String,
    /// where its cards go (check_layout); none: the pages work it out
    #[serde(default)]
    layout: Value,
}

async fn save_tree(State(app): State<Arc<App>>, user: User, Path(slug): Path<String>, Json(req): Json<TreeReq>) -> Res<Json<Value>> {
    check_tree(&req.data)?;
    let data = req.data.to_string();
    if data.len() > TREE_MAX {
        return Err(AppError::bad("the family tree is too big"));
    }
    let p = find(&app, &slug)?;
    let note: String = req.note.trim().chars().take(200).collect();
    let layout = check_layout(&req.data, &req.layout);
    let r = app.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        let cur: Option<(i64, String)> = tx
            .query_row("SELECT id, author FROM wiki_trees WHERE page_id = ?1 ORDER BY id DESC LIMIT 1", [p.id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        if cur.as_ref().map_or(0, |c| c.0) != req.base {
            return Ok(Err(cur.map(|c| c.1).unwrap_or_default()));
        }
        tx.execute(
            "INSERT INTO wiki_trees (page_id, data, author_id, author, created, note, layout) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![p.id, data, user.id, user.username, now(), note, layout],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Ok(id))
    })?;
    match r {
        Ok(rev) => Ok(Json(json!({ "rev": rev }))),
        Err(by) => Err(AppError::conflict(format!("changed by {by} since it was opened"))),
    }
}

async fn tree_revision(State(app): State<Arc<App>>, _user: User, Path((slug, id)): Path<(String, i64)>) -> Res<Json<Value>> {
    let p = find(&app, &slug)?;
    let r = app.db.with(|c| {
        c.query_row(
            "SELECT data, author, created, note FROM wiki_trees WHERE id = ?1 AND page_id = ?2",
            params![id, p.id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?)),
        )
        .optional()
    })?;
    let (data, author, created, note) = r.ok_or_else(AppError::not_found)?;
    let data: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
    Ok(Json(json!({ "rev": id, "data": data, "author": author, "created": created, "note": note })))
}

#[derive(Deserialize)]
struct SaveReq {
    title: String,
    body: String,
    /// the revision the text was opened at; 0 for a language the article does not have yet
    base: i64,
    #[serde(default)]
    note: String,
}

async fn save(State(app): State<Arc<App>>, user: User, Path((slug, lang)): Path<(String, String)>, Json(req): Json<SaveReq>) -> Res<Json<Value>> {
    check_lang(&lang)?;
    check_text(&req.title, &req.body)?;
    let p = find(&app, &slug)?;
    let note: String = req.note.trim().chars().take(200).collect();
    let r = app.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        let cur: Option<(i64, String, String, String)> = tx
            .query_row(
                "SELECT id, title, body, author FROM wiki_revs WHERE page_id = ?1 AND lang = ?2 ORDER BY id DESC LIMIT 1",
                params![p.id, lang],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let at = cur.as_ref().map_or(0, |c| c.0);
        if at != req.base {
            return Ok(Err(cur.map(|c| c.3).unwrap_or_default()));
        }
        if let Some((id, title, body, _)) = &cur {
            if title == req.title.trim() && *body == req.body {
                return Ok(Ok(*id));
            }
        }
        tx.execute(
            "INSERT INTO wiki_revs (page_id, lang, title, body, author_id, author, created, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![p.id, lang, req.title.trim(), req.body, user.id, user.username, now(), note],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Ok(id))
    })?;
    match r {
        Ok(rev) => Ok(Json(json!({ "rev": rev }))),
        Err(by) => Err(AppError::conflict(format!("changed by {by} since it was opened"))),
    }
}

#[derive(Deserialize)]
struct HistQ {
    lang: Option<String>,
}

async fn history(State(app): State<Arc<App>>, _user: User, Path(slug): Path<String>, Query(hq): Query<HistQ>) -> Res<Json<Value>> {
    let p = find(&app, &slug)?;
    let lang = hq.lang.unwrap_or_default();
    let revs = app.db.with(|c| {
        let mut st = c.prepare(
            "SELECT id, lang, title, author, created, note, length(CAST(body AS BLOB)) FROM wiki_revs
             WHERE page_id = ?1 AND (?2 = '' OR lang = ?2) ORDER BY id DESC LIMIT 1000",
        )?;
        let mut v = st
            .query_map(params![p.id, lang], |r| {
                Ok(json!({ "rev": r.get::<_, i64>(0)?, "lang": r.get::<_, String>(1)?, "title": r.get::<_, String>(2)?,
                    "author": r.get::<_, String>(3)?, "created": r.get::<_, i64>(4)?, "note": r.get::<_, String>(5)?, "size": r.get::<_, i64>(6)? }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // the family tree's saves too, as the language "tree"
        if lang.is_empty() || lang == "tree" {
            let mut st = c.prepare(
                "SELECT id, author, created, note, length(CAST(data AS BLOB)), data = 'null' FROM wiki_trees WHERE page_id = ?1 ORDER BY id DESC LIMIT 1000",
            )?;
            let t = st.query_map([p.id], |r| {
                Ok(json!({ "rev": r.get::<_, i64>(0)?, "lang": "tree", "title": "", "author": r.get::<_, String>(1)?, "created": r.get::<_, i64>(2)?,
                    "note": r.get::<_, String>(3)?, "size": r.get::<_, i64>(4)?, "removed": r.get::<_, bool>(5)? }))
            })?;
            for x in t {
                v.push(x?);
            }
            if lang.is_empty() {
                v.sort_by_key(|x| std::cmp::Reverse(x["created"].as_i64().unwrap_or(0)));
            }
        }
        Ok(v)
    })?;
    Ok(Json(Value::Array(revs)))
}

async fn revision(State(app): State<Arc<App>>, _user: User, Path((slug, id)): Path<(String, i64)>) -> Res<Json<Value>> {
    let p = find(&app, &slug)?;
    let r = app.db.with(|c| {
        c.query_row(
            "SELECT lang, title, body, author, created, note FROM wiki_revs WHERE id = ?1 AND page_id = ?2",
            params![id, p.id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?, r.get::<_, String>(5)?)),
        )
        .optional()
    })?;
    let (lang, title, body, author, created, note) = r.ok_or_else(AppError::not_found)?;
    let known = app.db.with(|c| titles(c))?;
    let html = render(&slug, &body, &known);
    Ok(Json(json!({ "rev": id, "lang": lang, "title": title, "body": body, "html": html, "author": author, "created": created, "note": note })))
}

#[derive(Deserialize)]
struct PinReq {
    pinned: bool,
}

/// to the top of the list, for everybody, or back among the others
async fn pin(State(app): State<Arc<App>>, _user: User, Path(slug): Path<String>, Json(req): Json<PinReq>) -> Res<Json<Value>> {
    let p = find(&app, &slug)?;
    app.db.with(|c| c.execute("UPDATE wiki_pages SET pinned = ?1 WHERE id = ?2", params![req.pinned, p.id]))?;
    Ok(Json(json!({ "pinned": req.pinned })))
}

fn may_remove() -> AppError {
    AppError::new(axum::http::StatusCode::FORBIDDEN, "only whoever started it, or an admin, can remove it")
}

async fn remove(State(app): State<Arc<App>>, user: User, Path(slug): Path<String>) -> Res<Json<Value>> {
    let p = find(&app, &slug)?;
    if !(user.admin || p.created_by == Some(user.id)) {
        return Err(may_remove());
    }
    app.db.with(|c| c.execute("DELETE FROM wiki_pages WHERE id = ?1", [p.id]))?;
    let _ = tokio::fs::remove_dir_all(app.cfg.data.join("wiki").join(p.id.to_string())).await;
    Ok(Json(json!({ "ok": true })))
}

/// one language of an article; the last one takes the article with it
async fn remove_lang(State(app): State<Arc<App>>, user: User, Path((slug, lang)): Path<(String, String)>) -> Res<Json<Value>> {
    let p = find(&app, &slug)?;
    let first: Option<Option<i64>> = app.db.with(|c| {
        c.query_row("SELECT author_id FROM wiki_revs WHERE page_id = ?1 AND lang = ?2 ORDER BY id LIMIT 1", params![p.id, lang], |r| r.get(0)).optional()
    })?;
    let Some(first) = first else { return Err(AppError::not_found()) };
    if !(user.admin || p.created_by == Some(user.id) || first == Some(user.id)) {
        return Err(may_remove());
    }
    let left = app.db.with(|c| {
        c.execute("DELETE FROM wiki_revs WHERE page_id = ?1 AND lang = ?2", params![p.id, lang])?;
        c.query_row("SELECT COUNT(*) FROM wiki_revs WHERE page_id = ?1", [p.id], |r| r.get::<_, i64>(0))
    })?;
    if left == 0 {
        app.db.with(|c| c.execute("DELETE FROM wiki_pages WHERE id = ?1", [p.id]))?;
        let _ = tokio::fs::remove_dir_all(app.cfg.data.join("wiki").join(p.id.to_string())).await;
    }
    Ok(Json(json!({ "ok": true, "gone": left == 0 })))
}

#[derive(Deserialize)]
struct PicQ {
    name: String,
}

/// a picture for an article; a name already there gets -2, -3 … The name it got comes back.
async fn add_picture(State(app): State<Arc<App>>, user: User, Path(slug): Path<String>, Query(pq): Query<PicQ>, body: Body) -> Res<Json<Value>> {
    let p = find(&app, &slug)?;
    let want = picture_name(&pq.name)?;
    let (stem, ext) = want.rsplit_once('.').expect("picture_name has an extension");
    let b = pictures(&app);
    let dir = RelPath::parse(&p.id.to_string())?;
    if b.stat(&dir).await.is_err() {
        b.mkdir(&dir).await?;
    }
    let mut name = want.clone();
    for n in 2.. {
        let taken = app.db.with(|c| c.query_row("SELECT 1 FROM wiki_media WHERE page_id = ?1 AND name = ?2", params![p.id, name], |_| Ok(())).optional())?;
        if taken.is_none() && b.stat(&dir.join(&RelPath::parse(&name)?)).await.is_err() {
            break;
        }
        name = format!("{stem}-{n}.{ext}");
    }
    let big = Arc::new(AtomicBool::new(false));
    let mut seen = 0u64;
    let over = big.clone();
    let data: ByteStream = Box::pin(body.into_data_stream().map_err(std::io::Error::other).and_then(move |chunk: Bytes| {
        seen += chunk.len() as u64;
        let too = seen > PICTURE_MAX;
        if too {
            over.store(true, Relaxed);
        }
        async move { if too { Err(std::io::Error::other("too big")) } else { Ok(chunk) } }
    }));
    let e = b.write(&dir.join(&RelPath::parse(&name)?), data).await.map_err(|e| {
        if big.load(Relaxed) {
            AppError::bad("the picture is too big (over 32 MB)")
        } else {
            AppError::from(e)
        }
    })?;
    app.db.with(|c| {
        c.execute(
            "INSERT INTO wiki_media (page_id, name, size, author_id, author, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![p.id, name, e.size as i64, user.id, user.username, now()],
        )
    })?;
    Ok(Json(json!({ "name": name, "size": e.size })))
}

async fn picture(State(app): State<Arc<App>>, _user: User, Path((slug, name)): Path<(String, String)>, headers: HeaderMap) -> Res<Response> {
    let p = find(&app, &slug)?;
    if picture_name(&name)? != name {
        return Err(AppError::not_found());
    }
    let (e, r) = pictures(&app).open(&RelPath::parse(&format!("{}/{name}", p.id))?).await?;
    Ok(http_util::serve(e, r, &headers, &name, false, false, None).await)
}

async fn remove_picture(State(app): State<Arc<App>>, user: User, Path((slug, name)): Path<(String, String)>) -> Res<Json<Value>> {
    let p = find(&app, &slug)?;
    let author: Option<Option<i64>> = app.db.with(|c| {
        c.query_row("SELECT author_id FROM wiki_media WHERE page_id = ?1 AND name = ?2", params![p.id, name], |r| r.get(0)).optional()
    })?;
    let Some(author) = author else { return Err(AppError::not_found()) };
    if !(user.admin || p.created_by == Some(user.id) || author == Some(user.id)) {
        return Err(may_remove());
    }
    app.db.with(|c| c.execute("DELETE FROM wiki_media WHERE page_id = ?1 AND name = ?2", params![p.id, name]))?;
    let _ = pictures(&app).remove(&RelPath::parse(&format!("{}/{name}", p.id))?).await;
    Ok(Json(json!({ "ok": true })))
}

fn db_err(e: AppError) -> anyhow::Error {
    anyhow::anyhow!(e.msg)
}

/// `osada wiki import <folder>`: an article from `<lang>.md` files (the first
/// line `# title`, then the text) and the pictures next to them; the folder's
/// name is the slug. Written as "osada"; again, it adds what changed.
/// The guide "osada: what it is and how to use it" as this version ships it
/// (docs/wiki/osada, T9): the page `osada`, imported at start when it is not
/// the one last imported (so a new version brings its guide, and edits made
/// in the wiki meanwhile stay until then).
const GUIDE_TEXTS: &[(&str, &str)] = &[
    ("cs", include_str!("../../docs/wiki/osada/cs.md")),
    ("de", include_str!("../../docs/wiki/osada/de.md")),
    ("en", include_str!("../../docs/wiki/osada/en.md")),
    ("fr", include_str!("../../docs/wiki/osada/fr.md")),
    ("hu", include_str!("../../docs/wiki/osada/hu.md")),
    ("sk", include_str!("../../docs/wiki/osada/sk.md")),
];
const GUIDE_PICS: &[(&str, &[u8])] = &[
    ("edit.cs.png", include_bytes!("../../docs/wiki/osada/edit.cs.png")),
    ("edit.de.png", include_bytes!("../../docs/wiki/osada/edit.de.png")),
    ("edit.en.png", include_bytes!("../../docs/wiki/osada/edit.en.png")),
    ("edit.fr.png", include_bytes!("../../docs/wiki/osada/edit.fr.png")),
    ("edit.hu.png", include_bytes!("../../docs/wiki/osada/edit.hu.png")),
    ("edit.sk.png", include_bytes!("../../docs/wiki/osada/edit.sk.png")),
    ("files.cs.png", include_bytes!("../../docs/wiki/osada/files.cs.png")),
    ("files.de.png", include_bytes!("../../docs/wiki/osada/files.de.png")),
    ("files.en.png", include_bytes!("../../docs/wiki/osada/files.en.png")),
    ("files.fr.png", include_bytes!("../../docs/wiki/osada/files.fr.png")),
    ("files.hu.png", include_bytes!("../../docs/wiki/osada/files.hu.png")),
    ("files.sk.png", include_bytes!("../../docs/wiki/osada/files.sk.png")),
    ("offline.cs.png", include_bytes!("../../docs/wiki/osada/offline.cs.png")),
    ("offline.de.png", include_bytes!("../../docs/wiki/osada/offline.de.png")),
    ("offline.en.png", include_bytes!("../../docs/wiki/osada/offline.en.png")),
    ("offline.fr.png", include_bytes!("../../docs/wiki/osada/offline.fr.png")),
    ("offline.hu.png", include_bytes!("../../docs/wiki/osada/offline.hu.png")),
    ("offline.sk.png", include_bytes!("../../docs/wiki/osada/offline.sk.png")),
    ("share.cs.png", include_bytes!("../../docs/wiki/osada/share.cs.png")),
    ("share.de.png", include_bytes!("../../docs/wiki/osada/share.de.png")),
    ("share.en.png", include_bytes!("../../docs/wiki/osada/share.en.png")),
    ("share.fr.png", include_bytes!("../../docs/wiki/osada/share.fr.png")),
    ("share.hu.png", include_bytes!("../../docs/wiki/osada/share.hu.png")),
    ("share.sk.png", include_bytes!("../../docs/wiki/osada/share.sk.png")),
    ("tree.cs.png", include_bytes!("../../docs/wiki/osada/tree.cs.png")),
    ("tree.de.png", include_bytes!("../../docs/wiki/osada/tree.de.png")),
    ("tree.en.png", include_bytes!("../../docs/wiki/osada/tree.en.png")),
    ("tree.fr.png", include_bytes!("../../docs/wiki/osada/tree.fr.png")),
    ("tree.hu.png", include_bytes!("../../docs/wiki/osada/tree.hu.png")),
    ("tree.sk.png", include_bytes!("../../docs/wiki/osada/tree.sk.png")),
    ("view.cs.png", include_bytes!("../../docs/wiki/osada/view.cs.png")),
    ("view.de.png", include_bytes!("../../docs/wiki/osada/view.de.png")),
    ("view.en.png", include_bytes!("../../docs/wiki/osada/view.en.png")),
    ("view.fr.png", include_bytes!("../../docs/wiki/osada/view.fr.png")),
    ("view.hu.png", include_bytes!("../../docs/wiki/osada/view.hu.png")),
    ("view.sk.png", include_bytes!("../../docs/wiki/osada/view.sk.png")),
];

pub fn guide(app: &App) -> anyhow::Result<()> {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    for (l, t) in GUIDE_TEXTS {
        h.update(l.as_bytes());
        h.update(t.as_bytes());
    }
    for (n, b) in GUIDE_PICS {
        h.update(n.as_bytes());
        h.update(b);
    }
    let hash = hex::encode(h.finalize());
    if crate::osada1::db::get_setting(&app.db, "wiki_guide").ok().flatten().as_deref() == Some(hash.as_str()) {
        return Ok(());
    }
    let mut texts = Vec::new();
    for (lang, s) in GUIDE_TEXTS {
        texts.push(text_of(&format!("{lang}.md"), lang, s)?);
    }
    let pics = GUIDE_PICS.iter().map(|(n, b)| (n.to_string(), b.to_vec())).collect();
    // the guide imported before the rename (page `shelf`) is this page now
    let _ = app.db.with(|c| c.execute("UPDATE wiki_pages SET slug = 'osada' WHERE slug = 'shelf' AND NOT EXISTS (SELECT 1 FROM wiki_pages WHERE slug = 'osada')", []));
    import(app, "osada", texts, pics)?;
    crate::osada1::db::set_setting(&app.db, "wiki_guide", &hash).map_err(|e| anyhow::anyhow!("{}", e.msg))?;
    Ok(())
}

/// `# title` and the body of a page's text.
fn text_of(name: &str, lang: &str, s: &str) -> anyhow::Result<(String, String, String)> {
    let (first, rest) = s.split_once('\n').unwrap_or((s, ""));
    let title = first.strip_prefix("# ").ok_or_else(|| anyhow::anyhow!("{name}: the first line is not '# title'"))?.trim().to_string();
    let body = rest.trim_start_matches('\n').to_string();
    check_text(&title, &body).map_err(|e| anyhow::anyhow!("{name}: {}", e.msg))?;
    Ok((lang.to_string(), title, body))
}

pub fn cli(app: &App, args: &[String]) -> anyhow::Result<()> {
    let (Some("import"), Some(dir)) = (args.first().map(String::as_str), args.get(1)) else {
        anyhow::bail!("usage: osada wiki import <folder>");
    };
    let dir = FsPath::new(dir);
    let slug = dir.file_name().and_then(|n| n.to_str()).map(slug_of).ok_or_else(|| anyhow::anyhow!("no folder name"))?;
    let mut texts = Vec::new();
    let mut pics = Vec::new();
    for e in fs::read_dir(dir)? {
        let p = e?.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        if let Some(lang) = name.strip_suffix(".md").filter(|l| LANGS.contains(l)) {
            texts.push(text_of(&name, lang, &fs::read_to_string(&p)?)?);
        } else if picture_name(&name).is_ok_and(|n| n == name) {
            pics.push((name, fs::read(&p)?));
        }
    }
    anyhow::ensure!(!texts.is_empty(), "no <lang>.md in {}", dir.display());
    import(app, &slug, texts, pics)
}

/// Texts `(lang, title, body)` and pictures as the page `slug`'s: a revision
/// for each text that is not its language's last, pictures that changed.
fn import(app: &App, slug: &str, mut texts: Vec<(String, String, String)>, pics: Vec<(String, Vec<u8>)>) -> anyhow::Result<()> {
    texts.sort();
    let t = now();
    let id = app.db.with(|c| {
        if let Some(p) = page(c, &slug)? {
            return Ok(p.id);
        }
        c.execute("INSERT INTO wiki_pages (slug, created_by, created) VALUES (?1, NULL, ?2)", params![slug, t])?;
        Ok(c.last_insert_rowid())
    }).map_err(db_err)?;
    let mut n = 0;
    for (lang, title, body) in &texts {
        let same: Option<i64> = app.db.with(|c| {
            c.query_row(
                "SELECT 1 FROM wiki_revs WHERE id = (SELECT MAX(id) FROM wiki_revs WHERE page_id = ?1 AND lang = ?2) AND title = ?3 AND body = ?4",
                params![id, lang, title, body],
                |r| r.get(0),
            )
            .optional()
        }).map_err(db_err)?;
        if same.is_none() {
            app.db.with(|c| {
                c.execute(
                    "INSERT INTO wiki_revs (page_id, lang, title, body, author_id, author, created, note) VALUES (?1, ?2, ?3, ?4, NULL, 'osada', ?5, 'imported')",
                    params![id, lang, title, body, t],
                )
            }).map_err(db_err)?;
            n += 1;
        }
    }
    let to = app.cfg.data.join("wiki").join(id.to_string());
    fs::create_dir_all(&to)?;
    let mut m = 0;
    for (name, data) in &pics {
        let dest = to.join(name);
        if fs::read(&dest).ok().as_deref() == Some(data.as_slice()) {
            continue;
        }
        let tmp = app.tmp_dir().join(format!("wiki-{id}-{name}"));
        fs::write(&tmp, data)?;
        fs::rename(&tmp, &dest)?;
        app.db.with(|c| {
            c.execute(
                "INSERT INTO wiki_media (page_id, name, size, author_id, author, created) VALUES (?1, ?2, ?3, NULL, 'osada', ?4)
                 ON CONFLICT(page_id, name) DO UPDATE SET size = excluded.size, created = excluded.created",
                params![id, name, data.len() as i64, t],
            )
        }).map_err(db_err)?;
        m += 1;
    }
    println!("wiki/{slug}: {n} of {} texts and {m} of {} pictures new or changed", texts.len(), pics.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        let d = json!({ "people": { "p1": { "name": "A" }, "p2": { "name": "B" } }, "unions": [] });
        let good = json!({ "pos": { "p1": { "x": 40, "y": 40 }, "p2": { "x": 308.5, "y": 40 } }, "w": 580, "h": 154,
            "paths": [{ "cls": "couple", "d": "M272 77H308.5" }] });
        assert!(check_layout(&d, &good).is_some());
        let mut short = good.clone();
        short["pos"].as_object_mut().unwrap().remove("p2");
        assert!(check_layout(&d, &short).is_none());
        let mut odd = good.clone();
        odd["paths"][0]["d"] = json!("M0 0\"/><script>");
        assert!(check_layout(&d, &odd).is_none());
        assert!(check_layout(&d, &Value::Null).is_none() && check_layout(&Value::Null, &good).is_none());
    }

    #[test]
    fn slugs_and_names() {
        assert_eq!(slug_of("Jak sdílet soubory?"), "jak-sdilet-soubory");
        assert_eq!(slug_of("  osada: how to use it  "), "osada-how-to-use-it");
        assert_eq!(slug_of("Árvíztűrő tükörfúrógép"), "arvizturo-tukorfurogep");
        assert_eq!(slug_of("???"), "article");
        assert!(slug_of(&"word ".repeat(40)).len() <= 60);
        assert_eq!(picture_name("My Plan (2).PNG").unwrap(), "My-Plan--2.png");
        assert_eq!(picture_name("../../etc/x.jpg").unwrap(), "x.jpg");
        assert_eq!(picture_name("files.en.png").unwrap(), "files.en.png");
        assert_eq!(picture_name("..x.png").unwrap(), "x.png");
        assert!(picture_name("notes.txt").is_err() && picture_name("noext").is_err());
    }
}
