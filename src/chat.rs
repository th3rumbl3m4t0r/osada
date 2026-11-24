//! Chat (§12.1, D-U42): a room is a collection of domain chat with a linear log. Its entries are
//! the room's operations (`ChatOp`): made, renamed, people added and leaving, messages said,
//! edited and deleted, how far each member has read. Membership and who may do what are checked
//! by the room's authority against the folded state (`RoomState`) before an entry is written.
//!
//! - A direct message is a room of two, kind `dm`, whose id comes from the two user ids
//!   (`dm_of`), so two people have one, however often either starts it.
//! - Rooms are local; a room's `federated` flag is for the Matrix module (§14) and refused until
//!   it exists.
//! - Typing is ephemeral: kept in memory by the node that answers, never written (node/chat.rs).
//! - The state keeps where each message is (seq, author), not its text: texts are read from the
//!   log when shown; an edited text is kept until it is deleted.

use std::collections::BTreeMap;

use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::log::{CollectionId, Entry};

/// The module name of the rooms' entries.
pub const CHAT: &str = "chat";
pub const MAX_TEXT: usize = 8000;
pub const MAX_NAME: usize = 80;
pub const MAX_TOPIC: usize = 300;
pub const MAX_MEMBERS: usize = 1000;

/// The direct-message room of two users (either order).
pub fn dm_of(a: &str, b: &str) -> CollectionId {
    let (x, y) = if a <= b { (a, b) } else { (b, a) };
    let mut h = Sha256::new();
    h.update(b"shelf.chat.dm\0");
    h.update(x.as_bytes());
    h.update([0]);
    h.update(y.as_bytes());
    CollectionId(h.finalize()[..16].try_into().expect("16 bytes"))
}

/// A new room's id.
pub fn new_room_id() -> CollectionId {
    let mut b = [0u8; 16];
    SystemRandom::new().fill(&mut b).expect("the system's randomness");
    CollectionId(b)
}

/// The actor of a user's entries.
pub fn actor_of(user_id: &str) -> String {
    format!("user:{user_id}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomKind {
    Room,
    Dm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Member,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ChatOp {
    /// The first entry: the kind, the name (none for a DM), the members besides who makes it.
    Create {
        kind: RoomKind,
        #[serde(default)]
        name: String,
        #[serde(default)]
        members: Vec<String>,
        #[serde(default)]
        federated: bool,
    },
    Rename { name: String },
    Topic { topic: String },
    Add { users: Vec<String> },
    Remove { user: String },
    Leave,
    Say { text: String },
    Edit { seq: u64, text: String },
    Delete { seq: u64 },
    /// Read up to `seq`.
    Read { seq: u64 },
}

impl ChatOp {
    pub fn name(&self) -> &'static str {
        match self {
            ChatOp::Create { .. } => "create",
            ChatOp::Rename { .. } => "rename",
            ChatOp::Topic { .. } => "topic",
            ChatOp::Add { .. } => "add",
            ChatOp::Remove { .. } => "remove",
            ChatOp::Leave => "leave",
            ChatOp::Say { .. } => "say",
            ChatOp::Edit { .. } => "edit",
            ChatOp::Delete { .. } => "delete",
            ChatOp::Read { .. } => "read",
        }
    }

    /// Whether the room's history shows it (a message or a line about the room).
    pub fn shown(&self) -> bool {
        !matches!(self, ChatOp::Edit { .. } | ChatOp::Delete { .. } | ChatOp::Read { .. })
    }
}

/// Where a message is in the log, and who said it (an index into `RoomState::people`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Msg {
    pub seq: u64,
    pub time: u64,
    author: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoomState {
    /// None until the room's first entry.
    pub kind: Option<RoomKind>,
    pub name: String,
    pub topic: String,
    pub federated: bool,
    pub created: u64,
    pub members: BTreeMap<String, Role>,
    /// How far each member has read (their own messages count as read).
    pub read: BTreeMap<String, u64>,
    /// The messages still there, in order.
    pub messages: Vec<Msg>,
    /// Edited texts, by the message's seq: the text and when.
    pub edited: BTreeMap<u64, (String, u64)>,
    /// The last entry folded.
    pub head: u64,
    people: Vec<String>,
}

fn text_ok(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("an empty message".into());
    }
    if text.chars().count() > MAX_TEXT {
        return Err(format!("a message is at most {MAX_TEXT} characters"));
    }
    Ok(())
}

fn name_ok(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > MAX_NAME || n.chars().any(char::is_control) {
        return Err(format!("a room's name: 1 to {MAX_NAME} characters, no control characters"));
    }
    Ok(())
}

impl RoomState {
    pub fn exists(&self) -> bool {
        self.kind.is_some()
    }

    pub fn is_member(&self, user: &str) -> bool {
        self.members.contains_key(user)
    }

    pub fn is_owner(&self, user: &str) -> bool {
        self.members.get(user) == Some(&Role::Owner)
    }

    /// The messages after what `user` has read.
    pub fn unread(&self, user: &str) -> usize {
        let read = self.read.get(user).copied().unwrap_or(0);
        self.messages.len() - self.messages.partition_point(|m| m.seq <= read)
    }

    pub fn last(&self) -> Option<&Msg> {
        self.messages.last()
    }

    pub fn author(&self, m: &Msg) -> &str {
        &self.people[m.author as usize]
    }

    /// The message at `seq`, if it is still there.
    pub fn message(&self, seq: u64) -> Option<&Msg> {
        self.messages.binary_search_by_key(&seq, |m| m.seq).ok().map(|i| &self.messages[i])
    }

    /// Whether `user` may write `op` to room `id` (users named in it are checked by the node).
    pub fn check(&self, id: CollectionId, user: &str, op: &ChatOp) -> Result<(), String> {
        if let ChatOp::Create { kind, name, members, federated } = op {
            if self.exists() {
                return Err("the room exists already".into());
            }
            if *federated {
                return Err("federated rooms come with the Matrix module (§14); this room would be local".into());
            }
            if members.len() > MAX_MEMBERS {
                return Err(format!("a room has at most {MAX_MEMBERS} members"));
            }
            return match kind {
                RoomKind::Room => name_ok(name),
                RoomKind::Dm => match members.as_slice() {
                    [other] if other != user && dm_of(user, other) == id => Ok(()),
                    _ => Err("a direct message is between two people, the room of the two".into()),
                },
            };
        }
        if !self.exists() {
            return Err("no such room".into());
        }
        if !self.is_member(user) {
            return Err("you are not in this room".into());
        }
        let room_only = |what: &str| if self.kind == Some(RoomKind::Dm) { Err(format!("a direct message cannot be {what}")) } else { Ok(()) };
        match op {
            ChatOp::Create { .. } => unreachable!("above"),
            ChatOp::Rename { name } => {
                room_only("renamed")?;
                name_ok(name)
            }
            ChatOp::Topic { topic } => {
                room_only("given a topic")?;
                if topic.chars().count() > MAX_TOPIC {
                    return Err(format!("a topic is at most {MAX_TOPIC} characters"));
                }
                Ok(())
            }
            ChatOp::Add { users } => {
                room_only("given more people")?;
                if users.is_empty() {
                    return Err("nobody to add".into());
                }
                if self.members.len() + users.len() > MAX_MEMBERS {
                    return Err(format!("a room has at most {MAX_MEMBERS} members"));
                }
                Ok(())
            }
            ChatOp::Remove { user: who } => {
                room_only("changed")?;
                if !self.is_owner(user) {
                    return Err("only the room's owners take people out".into());
                }
                if who == user {
                    return Err("to go, leave the room".into());
                }
                if !self.is_member(who) {
                    return Err("they are not in this room".into());
                }
                Ok(())
            }
            ChatOp::Leave => room_only("left"),
            ChatOp::Say { text } => text_ok(text),
            ChatOp::Edit { seq, text } => {
                let m = self.message(*seq).ok_or("no such message")?;
                if self.author(m) != user {
                    return Err("only its author edits a message".into());
                }
                text_ok(text)
            }
            ChatOp::Delete { seq } => {
                let m = self.message(*seq).ok_or("no such message")?;
                if self.author(m) != user && !self.is_owner(user) {
                    return Err("only its author or the room's owners delete a message".into());
                }
                Ok(())
            }
            ChatOp::Read { .. } => Ok(()),
        }
    }

    fn person(&mut self, user: &str) -> u32 {
        match self.people.iter().position(|p| p == user) {
            Some(i) => i as u32,
            None => {
                self.people.push(user.to_string());
                (self.people.len() - 1) as u32
            }
        }
    }

    /// Folds one entry in (checked when it was written).
    pub fn apply(&mut self, e: &Entry) -> Result<(), String> {
        self.head = e.seq;
        let op: ChatOp = serde_json::from_slice(&e.payload).map_err(|e| e.to_string())?;
        let user = e.actor.strip_prefix("user:").unwrap_or(&e.actor).to_string();
        match op {
            ChatOp::Create { kind, name, members, federated } => {
                self.kind = Some(kind);
                self.name = name.trim().to_string();
                self.federated = federated;
                self.created = e.time;
                let role = if kind == RoomKind::Dm { Role::Member } else { Role::Owner };
                self.members.insert(user, role);
                for m in members {
                    self.members.entry(m).or_insert(Role::Member);
                }
            }
            ChatOp::Rename { name } => self.name = name.trim().to_string(),
            ChatOp::Topic { topic } => self.topic = topic.trim().to_string(),
            ChatOp::Add { users } => {
                for u in users {
                    self.members.entry(u).or_insert(Role::Member);
                }
            }
            ChatOp::Remove { user: who } => self.drop_member(&who),
            ChatOp::Leave => self.drop_member(&user),
            ChatOp::Say { .. } => {
                let author = self.person(&user);
                self.messages.push(Msg { seq: e.seq, time: e.time, author });
                self.read.insert(user, e.seq);
            }
            ChatOp::Edit { seq, text } => {
                self.edited.insert(seq, (text, e.time));
            }
            ChatOp::Delete { seq } => {
                if let Ok(i) = self.messages.binary_search_by_key(&seq, |m| m.seq) {
                    self.messages.remove(i);
                }
                self.edited.remove(&seq);
            }
            ChatOp::Read { seq } => {
                let r = self.read.entry(user).or_insert(0);
                *r = (*r).max(seq.min(e.seq));
            }
        }
        Ok(())
    }

    /// Someone goes; when the last owner went, the longest-standing member (first by id) owns it.
    fn drop_member(&mut self, user: &str) {
        self.members.remove(user);
        self.read.remove(user);
        if !self.members.values().any(|r| *r == Role::Owner) {
            if let Some(r) = self.members.values_mut().next() {
                *r = Role::Owner;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Hash;

    fn entry(seq: u64, user: &str, op: &ChatOp) -> Entry {
        Entry {
            collection: CollectionId([1; 16]),
            epoch: 1,
            seq,
            time: 1000 + seq,
            actor: actor_of(user),
            module: CHAT.into(),
            op: op.name().into(),
            payload: serde_json::to_vec(op).unwrap(),
            prev: Hash::default(),
        }
    }

    /// Checks then folds, as the authority does.
    fn run(s: &mut RoomState, id: CollectionId, user: &str, op: ChatOp) -> Result<u64, String> {
        s.check(id, user, &op)?;
        let seq = s.head + 1;
        s.apply(&entry(seq, user, &op))?;
        Ok(seq)
    }

    #[test]
    fn a_room_its_members_messages_and_unread() {
        let id = new_room_id();
        let mut s = RoomState::default();
        assert!(run(&mut s, id, "a", ChatOp::Say { text: "hi".into() }).unwrap_err().contains("no such room"));
        let bad = ChatOp::Create { kind: RoomKind::Room, name: " ".into(), members: vec![], federated: false };
        assert!(run(&mut s, id, "a", bad).is_err());
        let fed = ChatOp::Create { kind: RoomKind::Room, name: "x".into(), members: vec![], federated: true };
        assert!(run(&mut s, id, "a", fed).unwrap_err().contains("Matrix"));
        run(&mut s, id, "a", ChatOp::Create { kind: RoomKind::Room, name: " kitchen ".into(), members: vec!["b".into()], federated: false }).unwrap();
        assert_eq!(s.name, "kitchen");
        assert!(s.is_owner("a") && s.is_member("b") && !s.is_member("c"));
        assert!(run(&mut s, id, "c", ChatOp::Say { text: "hi".into() }).unwrap_err().contains("not in this room"));
        assert!(run(&mut s, id, "a", ChatOp::Say { text: "  ".into() }).is_err());
        let m1 = run(&mut s, id, "a", ChatOp::Say { text: "hello".into() }).unwrap();
        let m2 = run(&mut s, id, "a", ChatOp::Say { text: "anyone?".into() }).unwrap();
        assert_eq!((s.unread("a"), s.unread("b")), (0, 2));
        run(&mut s, id, "b", ChatOp::Read { seq: m1 }).unwrap();
        assert_eq!(s.unread("b"), 1);
        // reading never goes back, nor past the entry
        run(&mut s, id, "b", ChatOp::Read { seq: 0 }).unwrap();
        assert_eq!(s.unread("b"), 1);
        let m3 = run(&mut s, id, "b", ChatOp::Say { text: "yes".into() }).unwrap();
        assert_eq!((s.unread("a"), s.unread("b")), (1, 0));
        // edits by the author only; deletions by the author or an owner
        assert!(run(&mut s, id, "b", ChatOp::Edit { seq: m1, text: "x".into() }).is_err());
        run(&mut s, id, "a", ChatOp::Edit { seq: m1, text: "hello all".into() }).unwrap();
        assert_eq!(s.edited[&m1].0, "hello all");
        assert!(run(&mut s, id, "b", ChatOp::Delete { seq: m2 }).is_err());
        run(&mut s, id, "a", ChatOp::Delete { seq: m3 }).unwrap();
        assert_eq!(s.messages.iter().map(|m| m.seq).collect::<Vec<_>>(), vec![m1, m2]);
        assert!(run(&mut s, id, "a", ChatOp::Edit { seq: m3, text: "x".into() }).is_err());
        // people: added by members, taken out by owners; the last owner going hands it on
        run(&mut s, id, "b", ChatOp::Add { users: vec!["c".into()] }).unwrap();
        assert!(run(&mut s, id, "b", ChatOp::Remove { user: "c".into() }).unwrap_err().contains("owners"));
        run(&mut s, id, "a", ChatOp::Remove { user: "c".into() }).unwrap();
        assert!(!s.is_member("c"));
        run(&mut s, id, "a", ChatOp::Leave).unwrap();
        assert!(s.is_owner("b"));
        assert!(run(&mut s, id, "a", ChatOp::Create { kind: RoomKind::Room, name: "again".into(), members: vec![], federated: false }).is_err());
    }

    #[test]
    fn a_direct_message_is_the_room_of_the_two() {
        assert_eq!(dm_of("a", "b"), dm_of("b", "a"));
        assert_ne!(dm_of("a", "b"), dm_of("a", "c"));
        let id = dm_of("a", "b");
        let mut s = RoomState::default();
        let other = ChatOp::Create { kind: RoomKind::Dm, name: String::new(), members: vec!["c".into()], federated: false };
        assert!(run(&mut s, id, "a", other).is_err());
        let me = ChatOp::Create { kind: RoomKind::Dm, name: String::new(), members: vec!["a".into()], federated: false };
        assert!(run(&mut s, id, "a", me).is_err());
        run(&mut s, id, "b", ChatOp::Create { kind: RoomKind::Dm, name: String::new(), members: vec!["a".into()], federated: false }).unwrap();
        assert!(s.is_member("a") && s.is_member("b") && !s.is_owner("a") && !s.is_owner("b"));
        for op in [ChatOp::Rename { name: "x".into() }, ChatOp::Add { users: vec!["c".into()] }, ChatOp::Leave] {
            assert!(run(&mut s, id, "a", op).unwrap_err().contains("direct message"));
        }
        run(&mut s, id, "a", ChatOp::Say { text: "hi".into() }).unwrap();
        assert_eq!(s.unread("b"), 1);
    }
}
