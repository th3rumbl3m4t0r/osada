//! Chat on the node (§12.1, D-U42): rooms for a person, a room's history, writes through the
//! room's authority, and the live part: a version that goes up whenever a room this node keeps
//! changes or someone types (long polls wait on it), and who is typing, which is kept in memory
//! here and never written.
//!
//! The pages ask the node that answers for them (home, through an edge or not); that node keeps
//! every room (rooms are kept by every store node). Typing is seen by those who ask the same node:
//! carrying it to other nodes over the links comes when people are answered by more than one.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use super::{Actor, Inner, Node};
use crate::{
    chat::{self, ChatOp, RoomKind, RoomState},
    cluster::Command,
    directory::User,
    log::{CollectionId, Entry, Log},
    placement::Domain,
};

/// How long someone counts as typing after the page said so.
const TYPING: Duration = Duration::from_secs(6);
/// How long a long poll waits for a change.
pub const WAIT: Duration = Duration::from_secs(25);
/// A page of history, at most.
const PAGE: usize = 200;

pub(super) struct Live {
    version: tokio::sync::watch::Sender<u64>,
    typing: Mutex<BTreeMap<(CollectionId, String), Instant>>,
}

impl Default for Live {
    fn default() -> Live {
        Live { version: tokio::sync::watch::channel(1).0, typing: Mutex::default() }
    }
}

impl Live {
    pub(super) fn changed(&self) {
        self.version.send_modify(|v| *v += 1);
    }
}

/// A user's name and the name to show.
fn who(inner: &Inner, id: &str) -> (String, String) {
    match inner.dirs.values().find_map(|d| d.state.users.get(id)) {
        Some(u) => (u.name.clone(), if u.display.trim().is_empty() { u.name.clone() } else { u.display.clone() }),
        None => (id.to_string(), id.to_string()),
    }
}

fn op_of(e: &Entry) -> Option<ChatOp> {
    serde_json::from_slice(&e.payload).ok()
}

fn preview(text: &str) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 140 { format!("{}…", one.chars().take(140).collect::<String>()) } else { one }
}

/// What the page shows of an entry: a message, or a line about the room.
fn item(inner: &Inner, s: &RoomState, e: &Entry, op: &ChatOp) -> Value {
    let author = e.actor.strip_prefix("user:").unwrap_or(&e.actor);
    let (name, display) = who(inner, author);
    let mut v = json!({ "seq": e.seq, "time": e.time, "author": author, "author_name": name, "author_display": display });
    match op {
        ChatOp::Say { text } => {
            v["kind"] = "say".into();
            match s.edited.get(&e.seq) {
                Some((t, at)) => {
                    v["text"] = t.as_str().into();
                    v["edited"] = (*at).into();
                }
                None => v["text"] = text.as_str().into(),
            }
        }
        other => {
            v["kind"] = "event".into();
            v["event"] = other.name().into();
            let names = |ids: &[String]| ids.iter().map(|i| who(inner, i).1).collect::<Vec<_>>();
            match other {
                ChatOp::Create { name, members, .. } => {
                    v["name"] = name.as_str().into();
                    v["users"] = names(members).into();
                }
                ChatOp::Rename { name } => v["name"] = name.as_str().into(),
                ChatOp::Topic { topic } => v["topic"] = topic.as_str().into(),
                ChatOp::Add { users } => v["users"] = names(users).into(),
                ChatOp::Remove { user } => v["users"] = names(std::slice::from_ref(user)).into(),
                _ => {}
            }
        }
    }
    v
}

/// The text a message has now: edited, or as it was said.
fn text_now(s: &RoomState, log: &Log, seq: u64) -> String {
    if let Some((t, _)) = s.edited.get(&seq) {
        return t.clone();
    }
    match log.get(seq).ok().flatten().as_ref().and_then(op_of) {
        Some(ChatOp::Say { text }) => text,
        _ => String::new(),
    }
}

impl Node {
    /// The live version now.
    pub fn chat_version(&self) -> u64 {
        *self.chat.version.borrow()
    }

    /// Waits until the version is not `seen` (at most `WAIT`); the version then.
    pub async fn chat_wait(&self, seen: u64) -> u64 {
        let mut rx = self.chat.version.subscribe();
        if *rx.borrow_and_update() != seen {
            return *rx.borrow();
        }
        let _ = tokio::time::timeout(WAIT, rx.changed()).await;
        *rx.borrow()
    }

    fn typing_in(&self, room: CollectionId, except: &str) -> Vec<String> {
        let t = self.chat.typing.lock().unwrap_or_else(|e| e.into_inner());
        t.iter().filter(|((r, u), at)| *r == room && u != except && at.elapsed() < TYPING).map(|((_, u), _)| u.clone()).collect()
    }

    /// The rooms `user` is in, the latest first: unread messages, the last one, who types.
    pub fn chat_rooms(&self, user: &User) -> Value {
        let inner = self.lock();
        let mut rooms = Vec::new();
        for (id, f) in &inner.rooms {
            let s = &f.state;
            if !s.is_member(&user.id) {
                continue;
            }
            let Some(r) = inner.replicas.get(id) else { continue };
            let last = s.last().map(|m| {
                let a = s.author(m);
                json!({ "seq": m.seq, "time": m.time, "author": a, "author_display": who(&inner, a).1, "text": preview(&text_now(s, r.log(), m.seq)) })
            });
            let (name, with) = match s.kind {
                Some(RoomKind::Dm) => {
                    let other = s.members.keys().find(|m| **m != user.id).cloned().unwrap_or_default();
                    let (n, d) = who(&inner, &other);
                    (d, Some(n))
                }
                _ => (s.name.clone(), None),
            };
            let typing: Vec<String> = self.typing_in(*id, &user.id).iter().map(|u| who(&inner, u).1).collect();
            rooms.push(json!({
                "id": id.to_string(), "kind": s.kind, "name": name, "with": with, "topic": s.topic,
                "members": s.members.len(), "unread": s.unread(&user.id), "last": last, "created": s.created,
                "typing": typing,
            }));
        }
        let at = |v: &Value| v["last"]["time"].as_u64().unwrap_or(v["created"].as_u64().unwrap_or(0));
        rooms.sort_by_key(|v| std::cmp::Reverse(at(v)));
        json!({ "rooms": rooms, "v": self.chat_version(), "me": user.id })
    }

    /// A room for `user`: who is in it, and its history: the `limit` latest before `before`, or
    /// what came after `after` (then also the messages edited and deleted since).
    pub fn chat_room(&self, user: &User, id: CollectionId, before: Option<u64>, after: Option<u64>, limit: usize) -> Result<Value, String> {
        let inner = self.lock();
        let f = inner.rooms.get(&id).filter(|f| f.state.exists()).ok_or("no such room")?;
        let s = &f.state;
        if !s.is_member(&user.id) {
            return Err("you are not in this room".into());
        }
        let log = inner.replicas.get(&id).ok_or("this node does not keep that room")?.log();
        let start = log.start().seq;
        let head = log.head().seq;
        let limit = limit.clamp(1, PAGE);
        let mut items = Vec::new();
        let (mut changed, mut deleted, mut more) = (Vec::new(), Vec::new(), false);
        let shown = |e: &Entry, op: &ChatOp| op.shown() && (!matches!(op, ChatOp::Say { .. }) || s.message(e.seq).is_some());
        match after {
            Some(after) => {
                let entries = log.read(after.max(start), PAGE * 2).map_err(|e| e.to_string())?;
                more = entries.len() == PAGE * 2;
                for e in &entries {
                    let Some(op) = op_of(e) else { continue };
                    match &op {
                        ChatOp::Edit { seq, .. } if s.message(*seq).is_some() => {
                            changed.push(json!({ "seq": seq, "text": text_now(s, log, *seq), "edited": s.edited.get(seq).map(|x| x.1) }));
                        }
                        ChatOp::Delete { seq } => deleted.push(*seq),
                        _ if shown(e, &op) => items.push(item(&inner, s, e, &op)),
                        _ => {}
                    }
                }
            }
            None => {
                // back from `before`, a window of entries at a time
                let mut end = before.unwrap_or(head + 1).min(head + 1);
                while end > start + 1 && items.len() < limit {
                    let from = end.saturating_sub(256).max(start);
                    let entries = log.read(from, (end - from - 1) as usize).map_err(|e| e.to_string())?;
                    let mut chunk: Vec<Value> = entries.iter().filter_map(|e| op_of(e).filter(|op| shown(e, op)).map(|op| item(&inner, s, e, &op))).collect();
                    let room_for = limit - items.len();
                    if chunk.len() > room_for {
                        chunk.drain(..chunk.len() - room_for);
                        more = true;
                    }
                    chunk.append(&mut items);
                    items = chunk;
                    end = from + 1;
                }
                more = more || end > start + 1;
            }
        }
        let mut members: Vec<Value> = s
            .members
            .iter()
            .map(|(m, role)| {
                let (n, d) = who(&inner, m);
                json!({ "id": m, "name": n, "display": d, "role": role, "read": s.read.get(m) })
            })
            .collect();
        members.sort_by(|a, b| a["display"].as_str().unwrap_or("").to_lowercase().cmp(&b["display"].as_str().unwrap_or("").to_lowercase()));
        let (name, with) = match s.kind {
            Some(RoomKind::Dm) => {
                let other = s.members.keys().find(|m| **m != user.id).cloned().unwrap_or_default();
                let (n, d) = who(&inner, &other);
                (d, Some(n))
            }
            _ => (s.name.clone(), None),
        };
        let typing: Vec<String> = self.typing_in(id, &user.id).iter().map(|u| who(&inner, u).1).collect();
        Ok(json!({
            "id": id.to_string(), "kind": s.kind, "name": name, "with": with, "topic": s.topic, "federated": s.federated,
            "owner": s.is_owner(&user.id), "members": members, "items": items, "changed": changed, "deleted": deleted,
            "more": more, "head": head, "read": s.read.get(&user.id).copied().unwrap_or(0), "unread": s.unread(&user.id),
            "typing": typing, "v": self.chat_version(),
        }))
    }

    /// Writes `op` to room `id` as `user`, at its authority; a room just made may take a moment
    /// to reach it, so that is tried for a few seconds.
    async fn chat_write(self: &Arc<Self>, id: CollectionId, user: &str, op: &ChatOp) -> Result<u64, String> {
        let value = serde_json::to_value(op).map_err(|e| e.to_string())?;
        let not_yet = |e: &str| e.contains("does not exist yet") || e.contains("not a member of that collection") || e == "not the authority";
        let mut tries = 0;
        loop {
            match self.submit(id, chat::CHAT, value.clone(), &chat::actor_of(user)).await {
                Err(e) if not_yet(&e) && tries < 50 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                other => return other,
            }
        }
    }

    /// Users by name (or id), enabled; the ids.
    pub fn chat_user_ids(&self, names: &[String]) -> Result<Vec<String>, String> {
        let inner = self.lock();
        names
            .iter()
            .map(|n| {
                let n = n.trim();
                inner
                    .dirs
                    .values()
                    .find_map(|d| d.state.by_name(n).or_else(|| d.state.users.get(n)))
                    .filter(|u| !u.disabled)
                    .map(|u| u.id.clone())
                    .ok_or_else(|| format!("no user {n}"))
            })
            .collect()
    }

    /// A new room made by `actor`, with `members` (user names): its id.
    pub async fn chat_create(self: &Arc<Self>, actor: &Actor, name: &str, members: &[String]) -> Result<String, String> {
        let mut ids = self.chat_user_ids(members)?;
        ids.retain(|i| *i != actor.user.id);
        ids.sort();
        ids.dedup();
        let op = ChatOp::Create { kind: RoomKind::Room, name: name.trim().to_string(), members: ids, federated: false };
        let id = chat::new_room_id();
        RoomState::default().check(id, &actor.user.id, &op)?;
        self.commit_anywhere(&actor.label(), Command::Create { id, domain: Domain::Chat, ou: actor.user.ou }).await?;
        self.chat_write(id, &actor.user.id, &op).await?;
        Ok(id.to_string())
    }

    /// The direct messages of `actor` and `other` (a user name): made the first time.
    pub async fn chat_dm(self: &Arc<Self>, actor: &Actor, other: &str) -> Result<String, String> {
        let other = self.chat_user_ids(&[other.to_string()])?.remove(0);
        if other == actor.user.id {
            return Err("a direct message is with someone else".into());
        }
        let id = chat::dm_of(&actor.user.id, &other);
        if self.lock().rooms.get(&id).is_some_and(|f| f.state.exists()) {
            return Ok(id.to_string());
        }
        if !self.cluster_state().collections.contains_key(&id) {
            if let Err(e) = self.commit_anywhere(&actor.label(), Command::Create { id, domain: Domain::Chat, ou: actor.user.ou }).await {
                // the other one may have started it just now (made at the voter, not here yet)
                if !e.contains("Exists") && !self.cluster_state().collections.contains_key(&id) {
                    return Err(e);
                }
            }
        }
        let op = ChatOp::Create { kind: RoomKind::Dm, name: String::new(), members: vec![other], federated: false };
        match self.chat_write(id, &actor.user.id, &op).await {
            Ok(_) => Ok(id.to_string()),
            Err(e) if e.contains("exists already") => Ok(id.to_string()),
            Err(e) => Err(e),
        }
    }

    /// Anything else a member does in a room: say, edit, delete, rename, add people, …
    pub async fn chat_do(self: &Arc<Self>, actor: &Actor, id: CollectionId, op: ChatOp) -> Result<u64, String> {
        if matches!(op, ChatOp::Create { .. }) {
            return Err("a room is made once".into());
        }
        if let ChatOp::Say { .. } = op {
            self.chat.typing.lock().unwrap_or_else(|e| e.into_inner()).remove(&(id, actor.user.id.clone()));
        }
        self.chat_write(id, &actor.user.id, &op).await
    }

    /// `user` has read room `id` up to `seq`: written only when that is further than before.
    pub async fn chat_read(self: &Arc<Self>, actor: &Actor, id: CollectionId, seq: u64) -> Result<(), String> {
        let (have, last) = {
            let inner = self.lock();
            let s = &inner.rooms.get(&id).ok_or("no such room")?.state;
            (s.read.get(&actor.user.id).copied().unwrap_or(0), s.last().map_or(0, |m| m.seq))
        };
        let seq = seq.min(last);
        if seq <= have {
            return Ok(());
        }
        self.chat_write(id, &actor.user.id, &ChatOp::Read { seq }).await.map(|_| ())
    }

    /// `user` is typing in room `id` (for the next few seconds).
    pub fn chat_typing(&self, user: &User, id: CollectionId) -> Result<(), String> {
        if !self.lock().rooms.get(&id).is_some_and(|f| f.state.is_member(&user.id)) {
            return Err("you are not in this room".into());
        }
        {
            let mut t = self.chat.typing.lock().unwrap_or_else(|e| e.into_inner());
            t.retain(|_, at| at.elapsed() < TYPING);
            t.insert((id, user.id.clone()), Instant::now());
        }
        self.chat.changed();
        Ok(())
    }

    /// People to write to or add: at most 15 enabled users whose name or display name has `q`.
    pub fn chat_people(&self, q: &str) -> Value {
        let q = q.trim().to_lowercase();
        let inner = self.lock();
        let mut found: Vec<&User> = inner
            .dirs
            .values()
            .flat_map(|d| d.state.users.values())
            .filter(|u| !u.disabled && (q.is_empty() || u.name.to_lowercase().contains(&q) || u.display.to_lowercase().contains(&q)))
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found.truncate(15);
        json!(found.iter().map(|u| json!({ "id": u.id, "name": u.name, "display": u.display })).collect::<Vec<_>>())
    }
}
