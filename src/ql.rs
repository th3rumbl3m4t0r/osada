//! The logs' query language (D-U31): sift's pipes (apps1 git `sift`, app/sift/ql.py), compiled to
//! one SQLite query over the logs authority's tables (`ds_<dataset>`, logstore.rs).
//!
//! ```text
//! dataset = http | filter status >= 400 and client insubnet "10.0.0.0/8" | stats count() as n by client | sort n desc | limit 20
//! ```
//!
//! Stages (the first may be left out: every dataset):
//!   dataset = NAME | dataset in (A, B) | dataset = *
//!   filter EXPR (where) · search "text" (any case, in _raw) · fields A, B · alter NAME = EXPR, … (eval)
//!   stats AGG [as NAME], … [by EXPR [as NAME], …] (comp) · timechart [span=5m] AGG … [by FIELD]
//!   top [N] FIELD [by FIELD, …] · dedup FIELD, … · sort FIELD [asc|desc], … · limit N (head)
//! Expressions: = != < <= > >=, and or not, + - * /, contains, startswith, endswith, ~= (regex; also ~,
//!   !~=, !~), in (…), insubnet "10.0.0.0/8, fd00::/8" (incidr; a bare address is /32 or /128), "not"
//!   before contains/startswith/endswith/in/insubnet; strings, numbers, true/false, null, durations 5m.
//!   `_time` compares with "2026-11-17T06:00:00Z" too; ago(1h), bin(_time, 5m).
//! Times are ms since 1970 (UTC); the time range comes from the page and is in every table scan.
use regex::Regex;
use rusqlite::functions::{Aggregate, Context, FunctionFlags};
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::Connection;
use serde_json::Value;
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::OnceLock;

pub type Res<T> = Result<T, String>;

// ---- the datasets' columns ----------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColType {
    Int,
    Real,
    Text,
    /// ms since 1970
    Time,
}

impl ColType {
    pub fn sql(self) -> &'static str {
        match self {
            ColType::Int | ColType::Time => "INTEGER",
            ColType::Real => "REAL",
            ColType::Text => "TEXT",
        }
    }
}

/// Every dataset: its columns, the common ones (`_time`, `_node`, `_ingested`, `_raw`) first.
pub type Schemas = BTreeMap<String, Vec<(String, ColType)>>;

pub const COMMON: [&str; 4] = ["_time", "_node", "_ingested", "_raw"];

// ---- reading ------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Str(String),
    /// ms
    Dur(i64),
    Num(String),
    Op(String),
    Id(String),
}

fn token_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r#"^(?:(?P<ws>\s+)|(?P<str>"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')|(?P<dur>\d+(?:s|m|h|d|w)\b)|(?P<num>\d+(?:\.\d+)?)|(?P<op>!~=|~=|!=|<=|>=|==|!~|[=<>~(),|*+\-/.])|(?P<id>[A-Za-z_][A-Za-z0-9_]*))"#,
        )
        .unwrap()
    })
}

fn dur_ms(s: &str) -> Option<i64> {
    let (n, u) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = n.parse().ok()?;
    let unit = match u {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    n.checked_mul(unit * 1000)
}

fn tokenize(q: &str) -> Res<Vec<Tok>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < q.len() {
        let rest = &q[pos..];
        let c = token_re().captures(rest).ok_or_else(|| format!("can't read this at position {}: {:?}", pos + 1, rest.chars().take(15).collect::<String>()))?;
        let m = c.get(0).unwrap();
        pos += m.end();
        if c.name("ws").is_some() {
            continue;
        }
        if let Some(s) = c.name("str") {
            let inner = &s.as_str()[1..s.as_str().len() - 1];
            let mut v = String::new();
            let mut esc = false;
            for ch in inner.chars() {
                if esc {
                    v.push(ch);
                    esc = false;
                } else if ch == '\\' {
                    esc = true;
                } else {
                    v.push(ch);
                }
            }
            out.push(Tok::Str(v));
        } else if let Some(d) = c.name("dur") {
            out.push(Tok::Dur(dur_ms(d.as_str()).ok_or("a duration too long")?));
        } else if let Some(n) = c.name("num") {
            out.push(Tok::Num(n.as_str().to_string()));
        } else if let Some(o) = c.name("op") {
            out.push(Tok::Op(o.as_str().to_string()));
        } else if let Some(i) = c.name("id") {
            out.push(Tok::Id(i.as_str().to_string()));
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Lit(Lit),
    /// ms
    Dur(i64),
    Field(String),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    /// = != < <= > >= ~ contains startswith endswith
    Cmp(String, Box<Expr>, Box<Expr>),
    In(Box<Expr>, Vec<Expr>),
    Subnet(Box<Expr>, Vec<Expr>),
    Arith(char, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    Dataset(Vec<String>),
    Filter(Expr),
    Search(String),
    Fields(Vec<String>),
    Alter(Vec<(String, Expr)>),
    Stats { aggs: Vec<(Expr, Option<String>)>, by: Vec<(Expr, Option<String>)>, timechart: bool, span: Option<i64> },
    Top { n: i64, field: String, by: Vec<String> },
    Dedup(Vec<String>),
    Sort(Vec<(String, bool)>),
    Limit(i64),
}

pub const AGGS: [&str; 20] = [
    "count", "dc", "count_distinct", "sum", "avg", "min", "max", "median", "values", "earliest", "latest", "first", "last",
    "percentile", "p50", "p90", "p95", "p99", "stdev", "range",
];

struct Parser {
    t: Vec<Tok>,
    i: usize,
}

impl Parser {
    fn peek(&self, k: usize) -> Option<&Tok> {
        self.t.get(self.i + k)
    }
    fn take(&mut self) -> Option<Tok> {
        let t = self.t.get(self.i).cloned();
        self.i += 1;
        t
    }
    fn is_id(&self, k: usize, word: &str) -> bool {
        matches!(self.peek(k), Some(Tok::Id(v)) if v.eq_ignore_ascii_case(word))
    }
    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(0), Some(Tok::Op(v)) if v == op)
    }
    fn found(&self) -> String {
        match self.peek(0) {
            None => "the end".into(),
            Some(Tok::Str(s)) => format!("\"{s}\""),
            Some(Tok::Dur(d)) => format!("{}s", d / 1000),
            Some(Tok::Num(s) | Tok::Op(s) | Tok::Id(s)) => s.clone(),
        }
    }
    fn accept_id(&mut self, word: &str) -> bool {
        if self.is_id(0, word) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn accept_op(&mut self, op: &str) -> bool {
        if self.is_op(op) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expect_op(&mut self, op: &str) -> Res<()> {
        if self.accept_op(op) {
            Ok(())
        } else {
            Err(format!("expected {op}, found {}", self.found()))
        }
    }
    fn name(&mut self) -> Res<String> {
        match self.peek(0) {
            Some(Tok::Id(v)) => {
                let v = v.clone();
                self.i += 1;
                Ok(v)
            }
            _ => Err(format!("expected a name, found {}", self.found())),
        }
    }
    fn names(&mut self) -> Res<Vec<String>> {
        let mut v = vec![self.name()?];
        while self.accept_op(",") {
            v.push(self.name()?);
        }
        Ok(v)
    }

    fn expr(&mut self) -> Res<Expr> {
        let mut a = self.and()?;
        while self.accept_id("or") {
            a = Expr::Or(Box::new(a), Box::new(self.and()?));
        }
        Ok(a)
    }
    fn and(&mut self) -> Res<Expr> {
        let mut a = self.not()?;
        while self.accept_id("and") {
            a = Expr::And(Box::new(a), Box::new(self.not()?));
        }
        Ok(a)
    }
    fn not(&mut self) -> Res<Expr> {
        if self.accept_id("not") {
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Res<Expr> {
        let a = self.add()?;
        let mut neg = false;
        if self.is_id(0, "not") && ["contains", "startswith", "endswith", "in", "incidr", "insubnet"].iter().any(|w| self.is_id(1, w)) {
            self.i += 1;
            neg = true;
        }
        let node = match self.peek(0).cloned() {
            Some(Tok::Op(v)) if ["=", "==", "!=", "<", "<=", ">", ">=", "~", "~=", "!~", "!~="].contains(&v.as_str()) => {
                self.i += 1;
                let mut op = match v.as_str() {
                    "==" => "=".to_string(),
                    "~=" => "~".to_string(),
                    "!~=" => "!~".to_string(),
                    o => o.to_string(),
                };
                if op == "!~" {
                    op = "~".into();
                    neg = !neg;
                }
                Expr::Cmp(op, Box::new(a), Box::new(self.add()?))
            }
            Some(Tok::Id(v)) if ["contains", "startswith", "endswith"].contains(&v.to_lowercase().as_str()) => {
                self.i += 1;
                Expr::Cmp(v.to_lowercase(), Box::new(a), Box::new(self.add()?))
            }
            Some(Tok::Id(v)) if ["incidr", "insubnet"].contains(&v.to_lowercase().as_str()) => {
                self.i += 1;
                let nets = if self.is_op("(") { self.items()? } else { vec![self.add()?] };
                Expr::Subnet(Box::new(a), nets)
            }
            Some(Tok::Id(v)) if v.eq_ignore_ascii_case("in") => {
                self.i += 1;
                Expr::In(Box::new(a), self.items()?)
            }
            _ => {
                if neg {
                    return Err("'not' needs contains, startswith, endswith, in or insubnet after it".into());
                }
                return Ok(a);
            }
        };
        Ok(if neg { Expr::Not(Box::new(node)) } else { node })
    }
    fn items(&mut self) -> Res<Vec<Expr>> {
        self.expect_op("(")?;
        let mut v = vec![self.add()?];
        while self.accept_op(",") {
            v.push(self.add()?);
        }
        self.expect_op(")")?;
        Ok(v)
    }
    fn add(&mut self) -> Res<Expr> {
        let mut a = self.mul()?;
        loop {
            let op = if self.is_op("+") {
                '+'
            } else if self.is_op("-") {
                '-'
            } else {
                break;
            };
            self.i += 1;
            a = Expr::Arith(op, Box::new(a), Box::new(self.mul()?));
        }
        Ok(a)
    }
    fn mul(&mut self) -> Res<Expr> {
        let mut a = self.atom()?;
        loop {
            let op = if self.is_op("*") {
                '*'
            } else if self.is_op("/") {
                '/'
            } else {
                break;
            };
            self.i += 1;
            a = Expr::Arith(op, Box::new(a), Box::new(self.atom()?));
        }
        Ok(a)
    }
    fn atom(&mut self) -> Res<Expr> {
        let found = self.found();
        match self.take() {
            Some(Tok::Str(s)) => Ok(Expr::Lit(Lit::Str(s))),
            Some(Tok::Num(n)) => Ok(Expr::Lit(if n.contains('.') {
                Lit::Real(n.parse().map_err(|_| format!("not a number: {n}"))?)
            } else {
                Lit::Int(n.parse().map_err(|_| format!("a number too big: {n}"))?)
            })),
            Some(Tok::Dur(d)) => Ok(Expr::Dur(d)),
            Some(Tok::Op(o)) if o == "(" => {
                let e = self.expr()?;
                self.expect_op(")")?;
                Ok(e)
            }
            Some(Tok::Op(o)) if o == "-" => Ok(Expr::Arith('-', Box::new(Expr::Lit(Lit::Int(0))), Box::new(self.atom()?))),
            Some(Tok::Id(v)) => {
                let lv = v.to_lowercase();
                match lv.as_str() {
                    "true" | "false" => return Ok(Expr::Lit(Lit::Bool(lv == "true"))),
                    "null" => return Ok(Expr::Lit(Lit::Null)),
                    _ => {}
                }
                if self.accept_op("(") {
                    let mut args = Vec::new();
                    if !self.is_op(")") {
                        args.push(self.expr()?);
                        while self.accept_op(",") {
                            args.push(self.expr()?);
                        }
                    }
                    self.expect_op(")")?;
                    return Ok(Expr::Call(lv, args));
                }
                Ok(Expr::Field(v))
            }
            _ => Err(format!("unexpected {found}")),
        }
    }

    fn stages(&mut self) -> Res<Vec<Stage>> {
        let mut out: Vec<Stage> = Vec::new();
        while self.peek(0).is_some() {
            if !out.is_empty() {
                self.expect_op("|")?;
            } else {
                self.accept_op("|");
            }
            let st = self.name()?.to_lowercase();
            let stage = match st.as_str() {
                "dataset" => {
                    if self.accept_id("in") {
                        self.expect_op("(")?;
                        let n = self.names()?;
                        self.expect_op(")")?;
                        Stage::Dataset(n)
                    } else {
                        self.expect_op("=")?;
                        if self.accept_op("*") {
                            Stage::Dataset(vec!["*".into()])
                        } else {
                            Stage::Dataset(vec![self.name()?])
                        }
                    }
                }
                "filter" | "where" => Stage::Filter(self.expr()?),
                "search" => match self.take() {
                    Some(Tok::Str(s)) => Stage::Search(s),
                    _ => return Err("search \"text\": the text in quotes".into()),
                },
                "fields" => Stage::Fields(self.names()?),
                "alter" | "eval" => {
                    let mut items = Vec::new();
                    loop {
                        let n = self.name()?;
                        self.expect_op("=")?;
                        items.push((n, self.expr()?));
                        if !self.accept_op(",") {
                            break;
                        }
                    }
                    Stage::Alter(items)
                }
                "stats" | "comp" | "timechart" => {
                    let timechart = st == "timechart";
                    let mut span = None;
                    if timechart && self.is_id(0, "span") {
                        self.i += 1;
                        self.expect_op("=")?;
                        match self.take() {
                            Some(Tok::Dur(d)) if d > 0 => span = Some(d),
                            _ => return Err("span: like 5m, 1h or 1d".into()),
                        }
                    }
                    let mut aggs = Vec::new();
                    loop {
                        let e = self.expr()?;
                        match &e {
                            Expr::Call(n, _) if AGGS.contains(&n.as_str()) => {}
                            _ => return Err(format!("{st}: an aggregation like count(), sum(bytes), dc(client): {}", AGGS.join(", "))),
                        }
                        let name = if self.accept_id("as") { Some(self.name()?) } else { None };
                        aggs.push((e, name));
                        if !self.accept_op(",") {
                            break;
                        }
                    }
                    let mut by = Vec::new();
                    if self.accept_id("by") {
                        loop {
                            let e = self.expr()?;
                            let name = if self.accept_id("as") { Some(self.name()?) } else { None };
                            by.push((e, name));
                            if !self.accept_op(",") {
                                break;
                            }
                        }
                    }
                    Stage::Stats { aggs, by, timechart, span }
                }
                "top" => {
                    let n = match self.peek(0) {
                        Some(Tok::Num(n)) => {
                            let n = n.parse().map_err(|_| "top N: a whole number")?;
                            self.i += 1;
                            n
                        }
                        _ => 10,
                    };
                    let field = self.name()?;
                    let by = if self.accept_id("by") { self.names()? } else { vec![] };
                    Stage::Top { n, field, by }
                }
                "dedup" => Stage::Dedup(self.names()?),
                "sort" => {
                    let mut items = Vec::new();
                    loop {
                        let f = self.name()?;
                        let desc = if self.accept_id("desc") {
                            true
                        } else {
                            self.accept_id("asc");
                            false
                        };
                        items.push((f, desc));
                        if !self.accept_op(",") {
                            break;
                        }
                    }
                    Stage::Sort(items)
                }
                "limit" | "head" => match self.take() {
                    Some(Tok::Num(n)) => Stage::Limit(n.parse().map_err(|_| "limit N: a whole number")?),
                    _ => return Err("limit N: a number".into()),
                },
                "join" => return Err("join is not here yet (sift has it; it comes later)".into()),
                other => return Err(format!("unknown stage {other:?}: dataset, filter, search, fields, alter, stats, timechart, top, dedup, sort, limit")),
            };
            out.push(stage);
        }
        Ok(out)
    }
}

pub fn parse(q: &str) -> Res<Vec<Stage>> {
    let mut p = Parser { t: tokenize(q)?, i: 0 };
    p.stages()
}

// ---- writing SQL ----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Events,
    Table,
    Timechart,
}

#[derive(Debug)]
pub struct Compiled {
    pub sql: String,
    pub params: Vec<SqlValue>,
    pub kind: Kind,
    pub columns: Vec<String>,
}

/// What a query is compiled for: the range (ms), now (ms, for ago()), and for event rules the
/// arrival window (`_ingested` after, up to).
#[derive(Clone, Copy, Debug)]
pub struct Range {
    pub from: i64,
    pub to: i64,
    pub now: i64,
    pub ingested: Option<(i64, i64)>,
}

struct Compiler<'a> {
    schemas: &'a Schemas,
    params: Vec<SqlValue>,
    range: Range,
}

fn q(name: &str) -> String {
    format!("\"{name}\"")
}

/// An ISO 8601 time ("2026-11-17", "2026-11-17T06:00", "…:00Z", "…+01:00") → ms.
pub fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = match s.find(['T', ' ']) {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    };
    let mut d = date.split('-');
    let (y, mo, da): (i64, i64, i64) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    if d.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&da) {
        return None;
    }
    let (clock, off) = if let Some(r) = rest.strip_suffix('Z') {
        (r, 0)
    } else if let Some(i) = rest.rfind(['+', '-']).filter(|&i| i > 0) {
        let (c, o) = rest.split_at(i);
        let sign = if o.starts_with('-') { -1 } else { 1 };
        let o = &o[1..];
        let (oh, om) = o.split_once(':').unwrap_or((o.get(..2)?, o.get(2..).unwrap_or("0")));
        (c, sign * (oh.parse::<i64>().ok()? * 60 + om.parse::<i64>().unwrap_or(0)))
    } else {
        (rest, 0)
    };
    let mut ms = 0i64;
    if !clock.is_empty() {
        let mut parts = clock.split(':');
        let h: i64 = parts.next()?.parse().ok()?;
        let mi: i64 = parts.next().map(|x| x.parse().ok()).unwrap_or(Some(0))?;
        let sec: f64 = parts.next().map(|x| x.parse().ok()).unwrap_or(Some(0.0))?;
        ms = (h * 3600 + mi * 60) * 1000 + (sec * 1000.0).round() as i64;
    }
    // days from civil (Howard Hinnant)
    let (y2, m2) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = (if y2 >= 0 { y2 } else { y2 - 399 }) / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400_000 + ms - off * 60_000)
}

/// A subnet as text ("10.0.0.0/8", a bare address) checked.
fn net_ok(s: &str) -> bool {
    parse_net(s).is_some()
}

fn parse_net(s: &str) -> Option<(IpAddr, u8)> {
    let s = s.trim();
    let (a, p) = match s.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (s, None),
    };
    let ip: IpAddr = a.parse().ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    let p = match p {
        Some(p) => p.parse::<u8>().ok().filter(|&p| p <= max)?,
        None => max,
    };
    Some((ip, p))
}

fn in_net(ip: IpAddr, (net, p): (IpAddr, u8)) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    };
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let mask = if p == 0 { 0 } else { u32::MAX << (32 - p) };
            u32::from(a) & mask == u32::from(b) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let mask = if p == 0 { 0 } else { u128::MAX << (128 - p) };
            u128::from(a) & mask == u128::from(b) & mask
        }
        _ => false,
    }
}

/// Whether `ip` (text) is in any of the comma-separated subnets.
pub fn ip_in(ip: &str, nets: &str) -> bool {
    let Ok(ip) = ip.trim().trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() else { return false };
    nets.split(',').filter_map(parse_net).any(|n| in_net(ip, n))
}

impl Compiler<'_> {
    fn param(&mut self, v: SqlValue) -> String {
        self.params.push(v);
        format!("?{}", self.params.len())
    }

    fn field(&self, name: &str, cols: &[String]) -> Res<String> {
        if cols.iter().any(|c| c == name) {
            return Ok(q(name));
        }
        let near: Vec<&str> = cols.iter().filter(|c| c.len() >= 2 && name.len() >= 2 && c[..2] == name[..2]).map(|s| s.as_str()).take(5).collect();
        Err(format!("no field {name:?} here{}", if near.is_empty() { String::new() } else { format!(" (maybe: {})", near.join(", ")) }))
    }

    fn lit(&mut self, l: &Lit) -> String {
        match l {
            Lit::Null => "NULL".into(),
            Lit::Bool(b) => (if *b { "1" } else { "0" }).into(),
            Lit::Int(i) => self.param(SqlValue::Integer(*i)),
            Lit::Real(r) => self.param(SqlValue::Real(*r)),
            Lit::Str(s) => self.param(SqlValue::Text(s.clone())),
        }
    }

    fn ex(&mut self, e: &Expr, cols: &[String], agg: bool) -> Res<String> {
        Ok(match e {
            Expr::Lit(l) => self.lit(l),
            Expr::Dur(ms) => ms.to_string(),
            Expr::Field(f) => self.field(f, cols)?,
            Expr::And(a, b) => format!("({} AND {})", self.ex(a, cols, agg)?, self.ex(b, cols, agg)?),
            Expr::Or(a, b) => format!("({} OR {})", self.ex(a, cols, agg)?, self.ex(b, cols, agg)?),
            Expr::Not(a) => format!("(NOT {})", self.ex(a, cols, agg)?),
            Expr::Arith(op, a, b) => format!("({} {op} {})", self.ex(a, cols, agg)?, self.ex(b, cols, agg)?),
            Expr::In(a, items) => {
                let a = self.ex(a, cols, agg)?;
                let mut v = Vec::new();
                for x in items {
                    v.push(self.ex(x, cols, agg)?);
                }
                format!("({a} IN ({}))", v.join(", "))
            }
            Expr::Subnet(a, nets) => {
                let a = self.ex(a, cols, agg)?;
                let mut parts = Vec::new();
                for x in nets {
                    match x {
                        Expr::Lit(Lit::Str(s)) => {
                            let list: Vec<&str> = s.split(',').map(str::trim).filter(|n| !n.is_empty()).collect();
                            if let Some(bad) = list.iter().find(|n| !net_ok(n)) {
                                return Err(format!("not a subnet: {bad:?} (like \"10.0.0.0/8\", \"192.168.1.5\" or \"fd00::/8\")"));
                            }
                            let p = self.param(SqlValue::Text(list.join(",")));
                            parts.push(format!("ql_insubnet({a}, {p})"));
                        }
                        Expr::Lit(_) => return Err("insubnet: subnets as text, like \"10.0.0.0/8\"".into()),
                        other => {
                            let n = self.ex(other, cols, agg)?;
                            parts.push(format!("ql_insubnet({a}, {n})"));
                        }
                    }
                }
                if parts.is_empty() {
                    return Err("insubnet: which subnet?".into());
                }
                format!("({})", parts.join(" OR "))
            }
            Expr::Cmp(op, a, b) => {
                // _time against a time as text: compared as ms
                let b_sql = match (&**a, &**b) {
                    (Expr::Field(f), Expr::Lit(Lit::Str(s))) if f == "_time" => match parse_time(s) {
                        Some(ms) => ms.to_string(),
                        None => return Err(format!("not a time: {s:?} (like \"2026-11-17T06:00:00Z\")")),
                    },
                    _ => {
                        if op == "~" {
                            if let Expr::Lit(Lit::Str(re)) = &**b {
                                Regex::new(re).map_err(|e| format!("regex {re:?}: {e}"))?;
                            }
                        }
                        self.ex(b, cols, agg)?
                    }
                };
                let a_sql = self.ex(a, cols, agg)?;
                match op.as_str() {
                    "contains" => format!("ql_contains({a_sql}, {b_sql})"),
                    "startswith" => format!("ql_startswith({a_sql}, {b_sql})"),
                    "endswith" => format!("ql_endswith({a_sql}, {b_sql})"),
                    "~" => format!("ql_regexp({a_sql}, {b_sql})"),
                    "=" => format!("({a_sql} = {b_sql})"),
                    o => format!("({a_sql} {o} {b_sql})"),
                }
            }
            Expr::Call(name, args) => self.call(name, args, cols, agg)?,
        })
    }

    fn call(&mut self, name: &str, args: &[Expr], cols: &[String], agg: bool) -> Res<String> {
        let a = |c: &mut Self, i: usize| -> Res<String> {
            match args.get(i) {
                Some(x) => c.ex(x, cols, false),
                None => Err(format!("{name}(): an argument is missing")),
            }
        };
        if AGGS.contains(&name) {
            if !agg {
                return Err(format!("{name}() only in stats or timechart"));
            }
            let time = || -> Res<String> {
                if cols.iter().any(|c| c == "_time") {
                    Ok(q("_time"))
                } else {
                    Err(format!("{name}(): there is no _time here"))
                }
            };
            return Ok(match name {
                "count" if args.is_empty() => "count(*)".into(),
                "count" => format!("count({})", a(self, 0)?),
                "dc" | "count_distinct" => format!("count(DISTINCT {})", a(self, 0)?),
                "sum" | "avg" | "min" | "max" => format!("{name}({})", a(self, 0)?),
                "range" => {
                    let x = a(self, 0)?;
                    format!("(max({x}) - min({x}))")
                }
                "stdev" => format!("ql_stdev({})", a(self, 0)?),
                "median" | "p50" => format!("ql_pct({}, 0.5)", a(self, 0)?),
                "p90" => format!("ql_pct({}, 0.9)", a(self, 0)?),
                "p95" => format!("ql_pct({}, 0.95)", a(self, 0)?),
                "p99" => format!("ql_pct({}, 0.99)", a(self, 0)?),
                "percentile" => {
                    let frac = match args.get(1) {
                        Some(Expr::Lit(Lit::Int(i))) => *i as f64,
                        Some(Expr::Lit(Lit::Real(r))) => *r,
                        _ => return Err("percentile(field, 95)".into()),
                    };
                    let frac = if frac > 1.0 { frac / 100.0 } else { frac };
                    if !(0.0..=1.0).contains(&frac) {
                        return Err("percentile: 0 to 100".into());
                    }
                    format!("ql_pct({}, {frac})", a(self, 0)?)
                }
                "values" => format!("ql_values({})", a(self, 0)?),
                "earliest" if args.is_empty() => format!("min({})", time()?),
                "latest" if args.is_empty() => format!("max({})", time()?),
                "earliest" => format!("min({})", a(self, 0)?),
                "latest" => format!("max({})", a(self, 0)?),
                "first" => format!("ql_first({}, {})", a(self, 0)?, time()?),
                "last" => format!("ql_last({}, {})", a(self, 0)?, time()?),
                _ => return Err(format!("unknown aggregation {name}()")),
            });
        }
        Ok(match name {
            "ago" => match args.first() {
                Some(Expr::Dur(d)) => (self.range.now - d).to_string(),
                _ => return Err("ago(5m): a duration".into()),
            },
            "now" => self.range.now.to_string(),
            "bin" => match args.get(1) {
                Some(Expr::Dur(d)) if *d > 0 => format!("(({} / {d}) * {d})", a(self, 0)?),
                _ => return Err("bin(_time, 5m)".into()),
            },
            "lower" | "upper" | "trim" | "abs" | "coalesce" | "round" | "substr" | "length" => {
                let mut v = Vec::new();
                for i in 0..args.len() {
                    v.push(a(self, i)?);
                }
                format!("{name}({})", v.join(", "))
            }
            "len" => format!("length({})", a(self, 0)?),
            "concat" => {
                let mut v = Vec::new();
                for i in 0..args.len() {
                    v.push(format!("coalesce(CAST({} AS TEXT), '')", a(self, i)?));
                }
                format!("({})", if v.is_empty() { "''".into() } else { v.join(" || ") })
            }
            "if" => format!("iif({}, {}, {})", a(self, 0)?, a(self, 1)?, a(self, 2)?),
            "tostring" => format!("CAST({} AS TEXT)", a(self, 0)?),
            "tonumber" => format!("ql_tonumber({})", a(self, 0)?),
            "toint" => format!("ql_toint({})", a(self, 0)?),
            "floor" | "ceil" => format!("ql_{name}({})", a(self, 0)?),
            "replace" => {
                if let Some(Expr::Lit(Lit::Str(re))) = args.get(1) {
                    Regex::new(re).map_err(|e| format!("regex {re:?}: {e}"))?;
                }
                format!("ql_replace({}, {}, {})", a(self, 0)?, a(self, 1)?, a(self, 2)?)
            }
            "extract" => {
                if let Some(Expr::Lit(Lit::Str(re))) = args.get(1) {
                    Regex::new(re).map_err(|e| format!("regex {re:?}: {e}"))?;
                }
                format!("ql_extract({}, {})", a(self, 0)?, a(self, 1)?)
            }
            "json" => format!("json_extract({}, '$.' || {})", a(self, 0)?, a(self, 1)?),
            "hour" => format!("((({} / 3600000) % 24))", a(self, 0)?),
            "day" => format!("strftime('%Y-%m-%d', {} / 1000, 'unixepoch')", a(self, 0)?),
            "weekday" => format!("(((CAST(strftime('%w', {} / 1000, 'unixepoch') AS INTEGER) + 6) % 7) + 1)", a(self, 0)?),
            "isnull" => format!("({} IS NULL)", a(self, 0)?),
            "empty" => {
                let x = a(self, 0)?;
                format!("({x} IS NULL OR {x} = '')")
            }
            "time" => format!("strftime('%Y-%m-%dT%H:%M:%fZ', {} / 1000.0, 'unixepoch')", a(self, 0)?),
            _ => return Err(format!("unknown function {name}()")),
        })
    }

    /// The dataset stage and the filters and searches right after it, as one scan per table.
    fn base(&mut self, mut stages: Vec<Stage>) -> Res<(String, Vec<String>, Vec<Stage>)> {
        let mut names = vec!["*".to_string()];
        if let Some(Stage::Dataset(n)) = stages.first() {
            names = n.clone();
            stages.remove(0);
        }
        if names == ["*"] {
            names = self.schemas.keys().cloned().collect();
        }
        for n in &names {
            if !self.schemas.contains_key(n) {
                return Err(format!("no dataset {n:?} (there are: {})", self.schemas.keys().cloned().collect::<Vec<_>>().join(", ")));
            }
        }
        let multi = names.len() > 1;
        // one dataset: its filters go into the table's scan; several: after the union, where a
        // column a dataset lacks is NULL
        let mut head = Vec::new();
        while !multi && matches!(stages.first(), Some(Stage::Filter(_) | Stage::Search(_))) {
            head.push(stages.remove(0));
        }
        let cols: Vec<String> = if !multi {
            self.schemas[&names[0]].iter().map(|(c, _)| c.clone()).collect()
        } else {
            let mut others: Vec<String> = names.iter().flat_map(|n| self.schemas[n].iter().map(|(c, _)| c.clone())).filter(|c| !COMMON.contains(&c.as_str())).collect();
            others.sort();
            others.dedup();
            let mut c = vec!["_dataset".to_string()];
            c.extend(COMMON.iter().map(|s| s.to_string()));
            c.extend(others);
            c
        };
        let mut parts = Vec::new();
        for n in &names {
            let have: Vec<String> = self.schemas[n].iter().map(|(c, _)| c.clone()).collect();
            let mut conds = vec![format!("\"_time\" >= {} AND \"_time\" < {}", self.range.from, self.range.to)];
            if let Some((after, upto)) = self.range.ingested {
                conds.push(format!("\"_ingested\" > {after} AND \"_ingested\" <= {upto}"));
            }
            for h in &head {
                match h {
                    Stage::Search(s) => {
                        let p = self.param(SqlValue::Text(s.clone()));
                        conds.push(format!("ql_contains(\"_raw\", {p})"));
                    }
                    Stage::Filter(e) => conds.push(self.ex(e, &have, false)?),
                    _ => {}
                }
            }
            let sel = if !multi {
                have.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ")
            } else {
                let dn = self.param(SqlValue::Text(n.clone()));
                std::iter::once(format!("{dn} AS \"_dataset\""))
                    .chain(cols[1..].iter().map(|c| if have.contains(c) { q(c) } else { format!("NULL AS {}", q(c)) }))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            parts.push(format!("SELECT {sel} FROM {} WHERE {}", q(&format!("ds_{n}")), conds.join(" AND ")));
        }
        Ok((parts.join("\nUNION ALL\n"), cols, stages))
    }

    fn pipeline(&mut self, stages: Vec<Stage>) -> Res<(String, Vec<String>, Kind, bool, bool)> {
        let (mut sql, mut cols, rest) = self.base(stages)?;
        let (mut kind, mut ordered, mut limited) = (Kind::Events, false, false);
        let list = |cols: &[String]| cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
        for st in rest {
            match st {
                Stage::Dataset(_) => return Err("dataset comes first".into()),
                Stage::Filter(e) => {
                    let c = self.ex(&e, &cols, false)?;
                    sql = format!("SELECT * FROM ({sql}) WHERE {c}");
                }
                Stage::Search(s) => {
                    if !cols.iter().any(|c| c == "_raw") {
                        return Err("search: there is no _raw here (search before stats or fields)".into());
                    }
                    let p = self.param(SqlValue::Text(s));
                    sql = format!("SELECT * FROM ({sql}) WHERE ql_contains(\"_raw\", {p})");
                }
                Stage::Fields(fs) => {
                    let mut sel = Vec::new();
                    for f in &fs {
                        sel.push(self.field(f, &cols)?);
                    }
                    sql = format!("SELECT {} FROM ({sql})", sel.join(", "));
                    cols = fs;
                }
                Stage::Alter(items) => {
                    for (n, e) in items {
                        if !valid_name(&n) {
                            return Err(format!("not a name for a field: {n:?}"));
                        }
                        let x = self.ex(&e, &cols, false)?;
                        let sel: Vec<String> = cols.iter().map(|c| if *c == n { format!("{x} AS {}", q(c)) } else { q(c) }).collect();
                        if cols.contains(&n) {
                            sql = format!("SELECT {} FROM ({sql})", sel.join(", "));
                        } else {
                            sql = format!("SELECT {}, {x} AS {} FROM ({sql})", sel.join(", "), q(&n));
                            cols.push(n);
                        }
                    }
                }
                Stage::Stats { aggs, by, timechart, span } => {
                    let (mut sel, mut grp, mut out) = (Vec::new(), Vec::new(), Vec::new());
                    if timechart {
                        if !cols.iter().any(|c| c == "_time") {
                            return Err("timechart: there is no _time here".into());
                        }
                        let s = span.unwrap_or_else(|| auto_span(self.range.from, self.range.to));
                        let b = format!("((\"_time\" / {s}) * {s})");
                        sel.push(format!("{b} AS \"_time\""));
                        grp.push(b);
                        out.push("_time".to_string());
                    }
                    for (i, (e, n)) in by.iter().enumerate() {
                        let n = n.clone().unwrap_or_else(|| if let Expr::Field(f) = e { f.clone() } else { format!("by{i}") });
                        let x = self.ex(e, &cols, false)?;
                        sel.push(format!("{x} AS {}", q(&n)));
                        // the expression, not the name: SQLite takes a source column before an alias
                        grp.push(x);
                        out.push(n);
                    }
                    for (i, (e, n)) in aggs.iter().enumerate() {
                        let n = n.clone().unwrap_or_else(|| match e {
                            Expr::Call(f, a) if a.is_empty() => f.clone(),
                            Expr::Call(f, a) => match &a[0] {
                                Expr::Field(x) => format!("{f}_{x}"),
                                _ => format!("{f}_{i}"),
                            },
                            _ => format!("agg{i}"),
                        });
                        let x = self.ex(e, &cols, true)?;
                        sel.push(format!("{x} AS {}", q(&n)));
                        out.push(n);
                    }
                    let mut dup = out.clone();
                    dup.sort();
                    dup.dedup();
                    if dup.len() != out.len() {
                        return Err("two result columns would have the same name: name them with as".into());
                    }
                    sql = format!("SELECT {} FROM ({sql}){}", sel.join(", "), if grp.is_empty() { String::new() } else { format!(" GROUP BY {}", grp.join(", ")) });
                    if timechart {
                        sql.push_str(" ORDER BY \"_time\"");
                        ordered = true;
                        kind = Kind::Timechart;
                    } else {
                        kind = Kind::Table;
                        ordered = false;
                    }
                    cols = out;
                }
                Stage::Top { n, field, by } => {
                    let f = self.field(&field, &cols)?;
                    let mut b = Vec::new();
                    for x in &by {
                        b.push(self.field(x, &cols)?);
                    }
                    let grp = b.iter().cloned().chain(std::iter::once(f.clone())).collect::<Vec<_>>().join(", ");
                    let part = if b.is_empty() { String::new() } else { format!("PARTITION BY {} ", b.join(", ")) };
                    let mut outc = by.clone();
                    outc.push(field.clone());
                    outc.push("count".into());
                    sql = format!(
                        "SELECT {} FROM (SELECT {grp}, count(*) AS \"count\", row_number() OVER ({part}ORDER BY count(*) DESC) AS \"__rn\" FROM ({sql}) GROUP BY {grp}) WHERE \"__rn\" <= {} ORDER BY {}\"count\" DESC",
                        list(&outc),
                        n.max(1),
                        if b.is_empty() { String::new() } else { format!("{}, ", b.join(", ")) }
                    );
                    cols = outc;
                    kind = Kind::Table;
                    ordered = true;
                    limited = true;
                }
                Stage::Dedup(fs) => {
                    let mut keys = Vec::new();
                    for f in &fs {
                        keys.push(self.field(f, &cols)?);
                    }
                    let order = if cols.iter().any(|c| c == "_time") { "ORDER BY \"_time\" DESC" } else { "" };
                    sql = format!("SELECT {} FROM (SELECT *, row_number() OVER (PARTITION BY {} {order}) AS \"__rn\" FROM ({sql})) WHERE \"__rn\" = 1", list(&cols), keys.join(", "));
                }
                Stage::Sort(items) => {
                    let mut o = Vec::new();
                    for (f, desc) in &items {
                        o.push(format!("{} {}", self.field(f, &cols)?, if *desc { "DESC" } else { "ASC" }));
                    }
                    sql = format!("SELECT * FROM ({sql}) ORDER BY {}", o.join(", "));
                    ordered = true;
                }
                Stage::Limit(n) => {
                    sql = format!("SELECT * FROM ({sql}) LIMIT {}", n.max(0));
                    limited = true;
                }
            }
        }
        Ok((sql, cols, kind, ordered, limited))
    }
}

fn valid_name(n: &str) -> bool {
    let mut c = n.chars();
    matches!(c.next(), Some(x) if x.is_ascii_alphabetic() || x == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// A round bucket (ms) giving about 80 buckets over the range.
pub fn auto_span(from: i64, to: i64) -> i64 {
    let secs = ((to - from).max(1) as f64) / 1000.0;
    for s in [1, 5, 10, 30, 60, 300, 600, 1800, 3600, 3 * 3600, 6 * 3600, 12 * 3600, 86_400, 7 * 86_400, 30 * 86_400] {
        if secs / s as f64 <= 80.0 {
            return s * 1000;
        }
    }
    30 * 86_400 * 1000
}

/// Query text → one SQL query, its parameters, what it gives (events, a table, a timechart) and
/// the columns. Events come newest first unless sorted; at most `default_limit` events (100 000
/// rows of a table) unless the query limits them itself.
pub fn compile(text: &str, schemas: &Schemas, range: Range, default_limit: usize) -> Res<Compiled> {
    let stages = parse(text)?;
    let mut c = Compiler { schemas, params: Vec::new(), range };
    let (mut sql, cols, kind, ordered, limited) = c.pipeline(stages)?;
    if kind == Kind::Events && !ordered && cols.iter().any(|x| x == "_time") {
        sql = format!("SELECT * FROM ({sql}) ORDER BY \"_time\" DESC");
    }
    if !limited {
        sql = format!("SELECT * FROM ({sql}) LIMIT {}", if kind == Kind::Events { default_limit } else { 100_000 });
    }
    Ok(Compiled { sql, params: c.params, kind, columns: cols })
}

/// The datasets a query reads (for the page: which columns it remembers).
pub fn datasets(text: &str, schemas: &Schemas) -> Vec<String> {
    match parse(text).ok().and_then(|s| s.into_iter().next()) {
        Some(Stage::Dataset(n)) if n != ["*"] => n,
        _ => schemas.keys().cloned().collect(),
    }
}

// ---- SQLite's side: the functions the SQL above calls ---------------------------------

fn text(v: ValueRef<'_>) -> Option<String> {
    match v {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(r) => Some(r.to_string()),
        ValueRef::Text(t) | ValueRef::Blob(t) => Some(String::from_utf8_lossy(t).into_owned()),
    }
}

fn num(v: ValueRef<'_>) -> Option<f64> {
    match v {
        ValueRef::Integer(i) => Some(i as f64),
        ValueRef::Real(r) => Some(r),
        ValueRef::Text(t) => std::str::from_utf8(t).ok()?.trim().parse().ok(),
        _ => None,
    }
}

fn re_of(ctx: &Context<'_>, i: usize) -> rusqlite::Result<std::sync::Arc<Regex>> {
    ctx.get_or_create_aux(i as i32, |v| -> Result<Regex, String> { Regex::new(&text(v).unwrap_or_default()).map_err(|e| e.to_string()) })
}

fn json_of(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(r) => Value::from(r),
        ValueRef::Text(t) | ValueRef::Blob(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
    }
}

struct Pct;
impl Aggregate<(Vec<f64>, f64), Option<f64>> for Pct {
    fn init(&self, _: &mut Context<'_>) -> rusqlite::Result<(Vec<f64>, f64)> {
        Ok((Vec::new(), 0.5))
    }
    fn step(&self, ctx: &mut Context<'_>, acc: &mut (Vec<f64>, f64)) -> rusqlite::Result<()> {
        if let Some(x) = num(ctx.get_raw(0)) {
            acc.0.push(x);
        }
        acc.1 = num(ctx.get_raw(1)).unwrap_or(0.5).clamp(0.0, 1.0);
        Ok(())
    }
    fn finalize(&self, _: &mut Context<'_>, acc: Option<(Vec<f64>, f64)>) -> rusqlite::Result<Option<f64>> {
        let Some((mut v, p)) = acc else { return Ok(None) };
        if v.is_empty() {
            return Ok(None);
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let pos = p * (v.len() - 1) as f64;
        let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
        Ok(Some(v[lo] + (v[hi] - v[lo]) * (pos - lo as f64)))
    }
}

struct Stdev;
impl Aggregate<Vec<f64>, Option<f64>> for Stdev {
    fn init(&self, _: &mut Context<'_>) -> rusqlite::Result<Vec<f64>> {
        Ok(Vec::new())
    }
    fn step(&self, ctx: &mut Context<'_>, acc: &mut Vec<f64>) -> rusqlite::Result<()> {
        if let Some(x) = num(ctx.get_raw(0)) {
            acc.push(x);
        }
        Ok(())
    }
    fn finalize(&self, _: &mut Context<'_>, acc: Option<Vec<f64>>) -> rusqlite::Result<Option<f64>> {
        let v = acc.unwrap_or_default();
        if v.len() < 2 {
            return Ok(None);
        }
        let m = v.iter().sum::<f64>() / v.len() as f64;
        Ok(Some((v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (v.len() - 1) as f64).sqrt()))
    }
}

/// Up to 50 distinct values, as a JSON array.
struct Values;
impl Aggregate<Vec<Value>, Option<String>> for Values {
    fn init(&self, _: &mut Context<'_>) -> rusqlite::Result<Vec<Value>> {
        Ok(Vec::new())
    }
    fn step(&self, ctx: &mut Context<'_>, acc: &mut Vec<Value>) -> rusqlite::Result<()> {
        let v = json_of(ctx.get_raw(0));
        if !v.is_null() && acc.len() < 50 && !acc.contains(&v) {
            acc.push(v);
        }
        Ok(())
    }
    fn finalize(&self, _: &mut Context<'_>, acc: Option<Vec<Value>>) -> rusqlite::Result<Option<String>> {
        Ok(Some(Value::Array(acc.unwrap_or_default()).to_string()))
    }
}

/// The value at the earliest (first) or latest (last) time.
struct ByTime(bool);
impl Aggregate<Option<(i64, SqlValue)>, Option<SqlValue>> for ByTime {
    fn init(&self, _: &mut Context<'_>) -> rusqlite::Result<Option<(i64, SqlValue)>> {
        Ok(None)
    }
    fn step(&self, ctx: &mut Context<'_>, acc: &mut Option<(i64, SqlValue)>) -> rusqlite::Result<()> {
        let t = num(ctx.get_raw(1)).unwrap_or(0.0) as i64;
        let better = match acc {
            None => true,
            Some((old, _)) => if self.0 { t > *old } else { t < *old },
        };
        if better {
            let v = match ctx.get_raw(0) {
                ValueRef::Null => SqlValue::Null,
                ValueRef::Integer(i) => SqlValue::Integer(i),
                ValueRef::Real(r) => SqlValue::Real(r),
                ValueRef::Text(t) => SqlValue::Text(String::from_utf8_lossy(t).into_owned()),
                ValueRef::Blob(b) => SqlValue::Blob(b.to_vec()),
            };
            *acc = Some((t, v));
        }
        Ok(())
    }
    fn finalize(&self, _: &mut Context<'_>, acc: Option<Option<(i64, SqlValue)>>) -> rusqlite::Result<Option<SqlValue>> {
        Ok(acc.flatten().map(|(_, v)| v))
    }
}

/// The functions the compiled SQL calls; on every connection that runs queries.
pub fn register(conn: &Connection) -> rusqlite::Result<()> {
    let f = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    conn.create_scalar_function("ql_contains", 2, f, |ctx| {
        Ok(match (text(ctx.get_raw(0)), text(ctx.get_raw(1))) {
            (Some(a), Some(b)) => a.to_lowercase().contains(&b.to_lowercase()),
            _ => false,
        })
    })?;
    conn.create_scalar_function("ql_startswith", 2, f, |ctx| Ok(matches!((text(ctx.get_raw(0)), text(ctx.get_raw(1))), (Some(a), Some(b)) if a.starts_with(&b))))?;
    conn.create_scalar_function("ql_endswith", 2, f, |ctx| Ok(matches!((text(ctx.get_raw(0)), text(ctx.get_raw(1))), (Some(a), Some(b)) if a.ends_with(&b))))?;
    conn.create_scalar_function("ql_regexp", 2, f, |ctx| {
        let re = re_of(ctx, 1)?;
        Ok(text(ctx.get_raw(0)).is_some_and(|s| re.is_match(&s)))
    })?;
    conn.create_scalar_function("ql_replace", 3, f, |ctx| {
        let re = re_of(ctx, 1)?;
        let with = text(ctx.get_raw(2)).unwrap_or_default();
        Ok(text(ctx.get_raw(0)).map(|s| re.replace_all(&s, with.as_str()).into_owned()))
    })?;
    conn.create_scalar_function("ql_extract", 2, f, |ctx| {
        let re = re_of(ctx, 1)?;
        Ok(text(ctx.get_raw(0)).and_then(|s| re.captures(&s).and_then(|c| c.get(1).or_else(|| c.get(0)).map(|m| m.as_str().to_string()))))
    })?;
    conn.create_scalar_function("ql_insubnet", 2, f, |ctx| {
        Ok(match (text(ctx.get_raw(0)), text(ctx.get_raw(1))) {
            (Some(ip), Some(nets)) => ip_in(&ip, &nets),
            _ => false,
        })
    })?;
    conn.create_scalar_function("ql_tonumber", 1, f, |ctx| Ok(num(ctx.get_raw(0))))?;
    conn.create_scalar_function("ql_toint", 1, f, |ctx| Ok(num(ctx.get_raw(0)).map(|x| x as i64)))?;
    conn.create_scalar_function("ql_floor", 1, f, |ctx| Ok(num(ctx.get_raw(0)).map(f64::floor)))?;
    conn.create_scalar_function("ql_ceil", 1, f, |ctx| Ok(num(ctx.get_raw(0)).map(f64::ceil)))?;
    conn.create_aggregate_function("ql_pct", 2, f, Pct)?;
    conn.create_aggregate_function("ql_stdev", 1, f, Stdev)?;
    conn.create_aggregate_function("ql_values", 1, f, Values)?;
    conn.create_aggregate_function("ql_first", 2, f, ByTime(false))?;
    conn.create_aggregate_function("ql_last", 2, f, ByTime(true))?;
    Ok(())
}

/// The rows of a compiled query, each a list of JSON values in the order of its columns; stopped
/// after `max` seconds.
pub fn run(conn: &Connection, c: &Compiled, max: std::time::Duration) -> Res<Vec<Vec<Value>>> {
    let handle = conn.get_interrupt_handle();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let d2 = done.clone();
    std::thread::spawn(move || {
        let end = std::time::Instant::now() + max;
        while std::time::Instant::now() < end {
            if d2.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if !d2.load(std::sync::atomic::Ordering::Relaxed) {
            handle.interrupt();
        }
    });
    let out = (|| -> rusqlite::Result<Vec<Vec<Value>>> {
        let mut st = conn.prepare(&c.sql)?;
        let n = st.column_count();
        let mut rows = st.query(rusqlite::params_from_iter(c.params.iter()))?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push((0..n).map(|i| r.get_ref(i).map(json_of).unwrap_or(Value::Null)).collect());
        }
        Ok(out)
    })();
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    out.map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::OperationInterrupted => format!("the query took longer than {} s", max.as_secs()),
        e => format!("the query failed: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schemas() -> Schemas {
        let mut s = Schemas::new();
        let common = || COMMON.iter().map(|c| (c.to_string(), if *c == "_node" || *c == "_raw" { ColType::Text } else { ColType::Time })).collect::<Vec<_>>();
        let mut http = common();
        http.extend([("client", ColType::Text), ("path", ColType::Text), ("status", ColType::Int), ("bytes", ColType::Int), ("ms", ColType::Real)].map(|(a, b)| (a.to_string(), b)));
        s.insert("http".into(), http);
        let mut auth = common();
        auth.extend([("event", ColType::Text), ("user", ColType::Text), ("addr", ColType::Text)].map(|(a, b)| (a.to_string(), b)));
        s.insert("auth".into(), auth);
        s
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        register(&c).unwrap();
        for (name, cols) in schemas() {
            let defs: Vec<String> = cols.iter().map(|(c, t)| format!("\"{c}\" {}", t.sql())).collect();
            c.execute(&format!("CREATE TABLE \"ds_{name}\" ({})", defs.join(", ")), []).unwrap();
        }
        let t0 = parse_time("2026-11-17T06:00:00Z").unwrap();
        let rows: [(i64, &str, &str, i64, i64); 6] = [
            (0, "203.0.113.5", "/wp-login.php", 404, 120),
            (1000, "203.0.113.5", "/.env", 404, 90),
            (2000, "203.0.113.5", "/", 200, 5000),
            (3000, "192.168.1.20", "/files/home/", 200, 8000),
            (4000, "2001:db8::7", "/cgi-bin/x", 403, 10),
            (5000, "192.168.1.20", "/api/me", 200, 300),
        ];
        for (dt, client, path, status, bytes) in rows {
            let raw = json!({ "client": client, "path": path, "status": status }).to_string();
            c.execute(
                "INSERT INTO ds_http (_time, _node, _ingested, _raw, client, path, status, bytes, ms) VALUES (?1, 'mail-1', ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![t0 + dt, raw, client, path, status, bytes, (bytes as f64) / 100.0],
            )
            .unwrap();
        }
        c.execute("INSERT INTO ds_auth (_time, _node, _ingested, _raw, event, user, addr) VALUES (?1, 'home', ?1, '{}', 'signin_failed', 'vera', '203.0.113.5')", [t0 + 500]).unwrap();
        c
    }

    use serde_json::json;

    fn rng() -> Range {
        let t0 = parse_time("2026-11-17T06:00:00Z").unwrap();
        Range { from: t0 - 3_600_000, to: t0 + 3_600_000, now: t0 + 10_000, ingested: None }
    }

    fn rows(q: &str) -> (Compiled, Vec<Vec<Value>>) {
        let c = compile(q, &schemas(), rng(), 1000).unwrap_or_else(|e| panic!("{q}: {e}"));
        let r = run(&db(), &c, std::time::Duration::from_secs(5)).unwrap_or_else(|e| panic!("{q}: {e}\n{}", c.sql));
        (c, r)
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("1970-01-01"), Some(0));
        assert_eq!(parse_time("1970-01-01T00:00:01Z"), Some(1000));
        assert_eq!(parse_time("2026-11-17T07:00:00+01:00"), parse_time("2026-11-17T06:00:00Z"));
        assert_eq!(parse_time("2026-11-17T06:00:00.250Z").unwrap() - parse_time("2026-11-17T06:00:00Z").unwrap(), 250);
        assert_eq!(parse_time("2026-11-17T06:00:00Z"), Some(1_794_895_200_000));
        assert!(parse_time("2026-13-01").is_none() && parse_time("nope").is_none());
    }

    #[test]
    fn subnets() {
        assert!(ip_in("10.1.2.3", "10.0.0.0/8") && !ip_in("11.1.2.3", "10.0.0.0/8"));
        assert!(ip_in("192.168.1.5", "172.16.0.0/12, 192.168.1.5"));
        assert!(ip_in("2001:db8::7", "2001:db8::/32") && !ip_in("2001:db9::7", "2001:db8::/32"));
        assert!(ip_in("::ffff:10.0.0.1", "10.0.0.0/8"));
        assert!(!ip_in("not an ip", "0.0.0.0/0") && ip_in("8.8.8.8", "0.0.0.0/0"));
    }

    #[test]
    fn parsing_errors() {
        let s = schemas();
        for (q, want) in [
            ("dataset = nope", "no dataset"),
            ("dataset = http | filter nope = 1", "no field"),
            ("dataset = http | stats status", "an aggregation"),
            ("dataset = http | filter count() > 1", "only in stats"),
            ("dataset = http | filter client insubnet \"10.0.0.0/33\"", "not a subnet"),
            ("dataset = http | filter path ~= \"(\"", "regex"),
            ("dataset = http | frobnicate", "unknown stage"),
            ("dataset = http | filter _time > \"yesterday\"", "not a time"),
            ("dataset = http | join (dataset = auth) as a a.addr = client", "join"),
            ("dataset = http | filter client not = 1", "expected |"),
            ("dataset = http | filter client = 1 or nothere = 2", "no field"),
        ] {
            let e = compile(q, &s, rng(), 100).unwrap_err();
            assert!(e.contains(want), "{q}: {e}");
        }
    }

    #[test]
    fn filters_and_events() {
        let (c, r) = rows("dataset = http | filter status >= 400 and not path startswith \"/files\"");
        assert_eq!(c.kind, Kind::Events);
        assert_eq!(r.len(), 3);
        // newest first
        let pi = c.columns.iter().position(|x| x == "path").unwrap();
        assert_eq!(r[0][pi], json!("/cgi-bin/x"));
        let (_, r) = rows("dataset = http | filter client insubnet \"192.168.0.0/16\"");
        assert_eq!(r.len(), 2);
        let (_, r) = rows("dataset = http | filter client not insubnet (\"192.168.0.0/16\", \"2001:db8::/32\")");
        assert_eq!(r.len(), 3);
        let (_, r) = rows("dataset = http | filter path ~= \"(?i)\\.(php|env)$\"");
        assert_eq!(r.len(), 2);
        let (_, r) = rows("dataset = http | filter path !~= \"^/(files|api)/\" and status in (404, 403)");
        assert_eq!(r.len(), 3);
        let (_, r) = rows("dataset = http | search \"WP-LOGIN\" | limit 5");
        assert_eq!(r.len(), 1);
        let (_, r) = rows("dataset = http | filter _time >= \"2026-11-17T06:00:03Z\"");
        assert_eq!(r.len(), 3);
        let (_, r) = rows("dataset = http | filter _time >= ago(8s)");
        assert_eq!(r.len(), 4);
        let (_, r) = rows("dataset = http | filter path contains \"LOGIN\"");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn stats_top_dedup_sort() {
        let (c, r) = rows("dataset = http | filter status >= 400 | stats count() as n, dc(path) as paths, values(path) as sample by client | filter n >= 2");
        assert_eq!(c.kind, Kind::Table);
        assert_eq!(c.columns, ["client", "n", "paths", "sample"]);
        assert_eq!(r, vec![vec![json!("203.0.113.5"), json!(2), json!(2), json!("[\"/wp-login.php\",\"/.env\"]")]]);
        let (c, r) = rows("dataset = http | stats sum(bytes), avg(ms) as avgms, p50(bytes) as med, max(status) by client | sort sum_bytes desc");
        assert_eq!(c.columns, ["client", "sum_bytes", "avgms", "med", "max_status"]);
        assert_eq!(r[0][0], json!("192.168.1.20"));
        assert_eq!(r[0][1], json!(8300));
        assert_eq!(r[0][3], json!(4150.0));
        let (c, r) = rows("dataset = http | top 2 client");
        assert_eq!(c.columns, ["client", "count"]);
        assert_eq!(r, vec![vec![json!("203.0.113.5"), json!(3)], vec![json!("192.168.1.20"), json!(2)]]);
        let (_, r) = rows("dataset = http | dedup client | fields client, path");
        assert_eq!(r.len(), 3);
        assert!(r.contains(&vec![json!("203.0.113.5"), json!("/")]));
        let (_, r) = rows("dataset = http | stats first(path) as f, last(path) as l, earliest() as e by client | filter client = \"203.0.113.5\"");
        assert_eq!(r[0][1], json!("/wp-login.php"));
        assert_eq!(r[0][2], json!("/"));
        assert_eq!(r[0][3], json!(parse_time("2026-11-17T06:00:00Z").unwrap()));
    }

    #[test]
    fn alter_timechart_and_several_datasets() {
        let (c, r) = rows("dataset = http | alter kb = bytes / 1000.0, path = upper(path) | filter kb > 1 | fields path, kb | sort kb");
        assert_eq!(c.columns, ["path", "kb"]);
        assert_eq!(r, vec![vec![json!("/"), json!(5.0)], vec![json!("/FILES/HOME/"), json!(8.0)]]);
        let (c, r) = rows("dataset = http | timechart span=2s count() as n");
        assert_eq!(c.kind, Kind::Timechart);
        assert_eq!(r.iter().map(|x| x[1].as_i64().unwrap()).collect::<Vec<_>>(), vec![2, 2, 2]);
        let (c, r) = rows("dataset in (http, auth) | filter client = \"203.0.113.5\" or addr = \"203.0.113.5\" | stats count() by _dataset");
        assert_eq!(c.columns, ["_dataset", "count"]);
        let mut r = r;
        r.sort_by(|a, b| a[0].as_str().cmp(&b[0].as_str()));
        assert_eq!(r, vec![vec![json!("auth"), json!(1)], vec![json!("http"), json!(3)]]);
        // a filter on a field only one dataset has: the other is left out
        let (_, r) = rows("dataset = * | filter event = \"signin_failed\"");
        assert_eq!(r.len(), 1);
        let (_, r) = rows("dataset = http | filter status = 200 | limit 2");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn event_rules_see_arrivals_only() {
        let t0 = parse_time("2026-11-17T06:00:00Z").unwrap();
        let mut rg = rng();
        rg.ingested = Some((t0 + 2500, t0 + 5000));
        let c = compile("dataset = http", &schemas(), rg, 100).unwrap();
        let r = run(&db(), &c, std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn values_are_parameters() {
        let c = compile("dataset = http | filter path = \"x'); DROP TABLE ds_http; --\"", &schemas(), rng(), 10).unwrap();
        assert!(!c.sql.contains("DROP"));
        let r = run(&db(), &c, std::time::Duration::from_secs(5)).unwrap();
        assert!(r.is_empty());
    }
}
