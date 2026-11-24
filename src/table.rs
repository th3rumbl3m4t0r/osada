//! Tables for the pages, made for a million rows (§15): the node filters,
//! sorts and pages, and the page gets one page and the count. The filter is
//! the grid's (static/grid.js): OR-groups of AND-lines over named columns,
//! with the grid's operators, regular expressions included; the node applies
//! it exactly, since the page only ever sees one page.
//!
//! A table is anything whose rows can say what is in a column ([`Row`]). One
//! query is one pass over the rows, then a sort of what passed, then the
//! page: linear in the rows, without indexes yet.

use std::{cmp::Ordering, collections::BTreeMap};

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    #[serde(default)]
    pub groups: Vec<Vec<Line>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub col: String,
    pub op: String,
    #[serde(default)]
    pub vals: Vec<Value>,
}

/// What a row has in a column.
pub enum Cell {
    Text(String),
    Texts(Vec<String>),
    Num(f64),
    Bool(bool),
    Empty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Contains,
    Eq,
    Ne,
    Starts,
    Ends,
    Regexp,
    Lt,
    Le,
    Gt,
    Ge,
    Between,
}

struct Compiled {
    col: String,
    op: Op,
    text: String,
    a: Option<f64>,
    b: Option<f64>,
    re: Option<Regex>,
}

/// A filter made ready to run over many rows.
pub struct Prepared {
    groups: Vec<Vec<Compiled>>,
}

fn number(v: Option<&Value>) -> Option<f64> {
    v.and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
}

impl Filter {
    pub fn prepare(&self) -> Result<Prepared, String> {
        let mut groups = Vec::new();
        for g in &self.groups {
            let mut lines = Vec::new();
            for l in g {
                let op = match l.op.as_str() {
                    "contains" => Op::Contains,
                    "=" => Op::Eq,
                    "!=" => Op::Ne,
                    "startswith" => Op::Starts,
                    "endswith" => Op::Ends,
                    "regexp" => Op::Regexp,
                    "<" | "before" => Op::Lt,
                    "<=" => Op::Le,
                    ">" | "after" => Op::Gt,
                    ">=" => Op::Ge,
                    "between" => Op::Between,
                    other => return Err(format!("no operator {other:?}")),
                };
                let text = match l.vals.first() {
                    Some(Value::String(s)) => s.to_lowercase(),
                    Some(v) => v.to_string().to_lowercase(),
                    None => String::new(),
                };
                let re = if op == Op::Regexp {
                    let src = l.vals.first().and_then(|v| v.as_str()).unwrap_or_default();
                    let re = RegexBuilder::new(src).case_insensitive(true).size_limit(1 << 20).build().map_err(|e| format!("regexp: {e}"))?;
                    Some(re)
                } else {
                    None
                };
                lines.push(Compiled { col: l.col.clone(), op, text, a: number(l.vals.first()), b: number(l.vals.get(1)), re });
            }
            groups.push(lines);
        }
        Ok(Prepared { groups })
    }
}

impl Compiled {
    fn holds(&self, cell: Cell) -> bool {
        match cell {
            Cell::Num(n) => {
                let (a, b) = (self.a.unwrap_or(f64::NAN), self.b.unwrap_or(f64::NAN));
                match self.op {
                    Op::Eq => n == a,
                    Op::Ne => n != a,
                    Op::Lt => n < a,
                    Op::Le => n <= a,
                    Op::Gt => n > a,
                    Op::Ge => n >= a,
                    Op::Between => n >= a && n <= b,
                    _ => self.text_holds(&[n.to_string()]),
                }
            }
            Cell::Bool(v) => {
                let want = matches!(self.text.as_str(), "true" | "1" | "yes");
                match self.op {
                    Op::Ne => v != want,
                    _ => v == want,
                }
            }
            Cell::Text(s) => self.text_holds(std::slice::from_ref(&s)),
            Cell::Texts(v) => self.text_holds(&v),
            Cell::Empty => self.op == Op::Ne,
        }
    }

    fn text_holds(&self, vals: &[String]) -> bool {
        if let Some(re) = &self.re {
            return vals.iter().any(|v| re.is_match(v));
        }
        let lower = |v: &String| v.to_lowercase();
        let w = &self.text;
        match self.op {
            Op::Contains => vals.iter().any(|v| lower(v).contains(w.as_str())),
            Op::Eq => vals.iter().any(|v| lower(v) == *w),
            Op::Ne => !vals.iter().any(|v| lower(v) == *w),
            Op::Starts => vals.iter().any(|v| lower(v).starts_with(w.as_str())),
            Op::Ends => vals.iter().any(|v| lower(v).ends_with(w.as_str())),
            // Numbers on text (an id, a seq): compared as numbers where they are.
            Op::Lt | Op::Le | Op::Gt | Op::Ge | Op::Between => vals.iter().filter_map(|v| v.parse::<f64>().ok()).any(|n| {
                let (a, b) = (self.a.unwrap_or(f64::NAN), self.b.unwrap_or(f64::NAN));
                match self.op {
                    Op::Lt => n < a,
                    Op::Le => n <= a,
                    Op::Gt => n > a,
                    Op::Ge => n >= a,
                    _ => n >= a && n <= b,
                }
            }),
            Op::Regexp => false,
        }
    }
}

impl Prepared {
    pub fn admits(&self, cell: impl Fn(&str) -> Cell) -> bool {
        self.groups.is_empty() || self.groups.iter().any(|g| g.iter().all(|l| l.holds(cell(&l.col))))
    }
}

/// A table query from the pages.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Query {
    #[serde(default)]
    pub filter: Option<Filter>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub desc: bool,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub limit: usize,
    /// What the table is of: `ou`, `below`, `group`, `member`, `subject`, …
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

impl Query {
    pub fn param(&self, k: &str) -> Option<&str> {
        self.params.get(k).map(String::as_str).filter(|v| !v.is_empty())
    }
}

#[derive(Debug, Serialize)]
pub struct Page {
    pub rows: Vec<Value>,
    /// How many rows meet the filter.
    pub total: usize,
    pub offset: usize,
}

pub trait Row {
    fn cell(&self, col: &str) -> Cell;
    fn json(&self) -> Value;
}

/// How a cell sorts: nothing first, then numbers, then text.
#[derive(PartialEq)]
enum Key {
    Empty,
    Num(f64),
    Text(String),
}

impl Eq for Key {}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Key::Num(a), Key::Num(b)) => a.total_cmp(b),
            (Key::Text(a), Key::Text(b)) => a.cmp(b),
            (Key::Empty, Key::Empty) => Ordering::Equal,
            (Key::Empty, _) => Ordering::Less,
            (_, Key::Empty) => Ordering::Greater,
            (Key::Num(_), Key::Text(_)) => Ordering::Less,
            (Key::Text(_), Key::Num(_)) => Ordering::Greater,
        }
    }
}

fn key(c: Cell) -> Key {
    match c {
        Cell::Num(n) => Key::Num(n),
        Cell::Bool(b) => Key::Num(f64::from(u8::from(b))),
        Cell::Text(s) => Key::Text(s.to_lowercase()),
        Cell::Texts(v) => Key::Text(v.join(", ").to_lowercase()),
        Cell::Empty => Key::Empty,
    }
}

pub const MAX_PAGE: usize = 1000;

/// Every row that meets `filter` (a mass update over a query's result).
pub fn matching<R: Row>(rows: impl Iterator<Item = R>, filter: Option<&Filter>) -> Result<Vec<R>, String> {
    let f = filter.map(Filter::prepare).transpose()?;
    Ok(rows.filter(|r| f.as_ref().is_none_or(|f| f.admits(|c| r.cell(c)))).collect())
}

/// The rows that meet the query's filter, sorted, and the page of them it
/// asks for.
pub fn page<R: Row>(rows: impl Iterator<Item = R>, q: &Query) -> Result<Page, String> {
    let filter = q.filter.as_ref().map(Filter::prepare).transpose()?;
    let mut hits: Vec<R> = rows.filter(|r| filter.as_ref().is_none_or(|f| f.admits(|c| r.cell(c)))).collect();
    let total = hits.len();
    if let Some(col) = &q.sort {
        hits.sort_by_cached_key(|r| key(r.cell(col)));
        if q.desc {
            hits.reverse();
        }
    }
    let limit = if q.limit == 0 { 50 } else { q.limit.min(MAX_PAGE) };
    let rows = hits.into_iter().skip(q.offset).take(limit).map(|r| r.json()).collect();
    Ok(Page { rows, total, offset: q.offset })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Instant;

    struct Person {
        name: String,
        age: f64,
        tags: Vec<String>,
        active: bool,
    }

    impl Row for &Person {
        fn cell(&self, col: &str) -> Cell {
            match col {
                "name" => Cell::Text(self.name.clone()),
                "age" => Cell::Num(self.age),
                "tags" => Cell::Texts(self.tags.clone()),
                "active" => Cell::Bool(self.active),
                _ => Cell::Empty,
            }
        }
        fn json(&self) -> Value {
            json!({ "name": self.name, "age": self.age })
        }
    }

    fn people() -> Vec<Person> {
        [("Anna", 30.0, vec!["de"], true), ("bob", 25.0, vec!["cz", "de"], false), ("Cyril", 41.0, vec![], true), ("dora", 25.0, vec!["at"], true)]
            .into_iter()
            .map(|(n, a, t, x)| Person { name: n.into(), age: a, tags: t.into_iter().map(String::from).collect(), active: x })
            .collect()
    }

    fn q(filter: Value, sort: Option<&str>, desc: bool) -> Query {
        Query {
            filter: Some(serde_json::from_value(json!({ "groups": filter })).unwrap()),
            sort: sort.map(String::from),
            desc,
            offset: 0,
            limit: 10,
            params: BTreeMap::new(),
        }
    }

    fn names(p: &Page) -> Vec<String> {
        p.rows.iter().map(|r| r["name"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn filters_sort_and_page() {
        let ps = people();
        let run = |f: Value, sort, desc| page(ps.iter(), &q(f, sort, desc)).unwrap();
        assert_eq!(names(&run(json!([]), Some("name"), false)), ["Anna", "bob", "Cyril", "dora"]);
        assert_eq!(names(&run(json!([[{ "col": "age", "op": "=", "vals": [25] }]]), Some("name"), true)), ["dora", "bob"]);
        assert_eq!(names(&run(json!([[{ "col": "name", "op": "contains", "vals": ["A"] }, { "col": "age", "op": ">=", "vals": ["30"] }]]), None, false)), ["Anna"]);
        // OR of two groups; a list column matches when any of it does; "!=" when none does.
        let r = run(json!([[{ "col": "tags", "op": "=", "vals": ["cz"] }], [{ "col": "active", "op": "=", "vals": ["false"] }, { "col": "age", "op": "<", "vals": [26] }]]), Some("age"), false);
        assert_eq!(names(&r), ["bob"]);
        assert_eq!(names(&run(json!([[{ "col": "tags", "op": "!=", "vals": ["de"] }]]), Some("name"), false)), ["Cyril", "dora"]);
        assert_eq!(names(&run(json!([[{ "col": "name", "op": "regexp", "vals": ["^[ab]"] }]]), Some("name"), false)), ["Anna", "bob"]);
        assert_eq!(names(&run(json!([[{ "col": "age", "op": "between", "vals": [26, 41] }]]), Some("age"), true)), ["Cyril", "Anna"]);
        assert!(page(ps.iter(), &q(json!([[{ "col": "name", "op": "regexp", "vals": ["("] }]]), None, false)).is_err());
        assert!(page(ps.iter(), &q(json!([[{ "col": "name", "op": "~", "vals": ["x"] }]]), None, false)).is_err());
        let mut paged = q(json!([]), Some("name"), false);
        paged.offset = 1;
        paged.limit = 2;
        let p = page(ps.iter(), &paged).unwrap();
        assert_eq!((names(&p), p.total), (vec!["bob".to_string(), "Cyril".to_string()], 4));
    }

    /// A million rows: `cargo test --release table::tests::a_million -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn a_million_rows() {
        let ps: Vec<Person> = (0..1_000_000)
            .map(|i| Person { name: format!("user{i:07}"), age: (i % 90) as f64, tags: vec![format!("ou{}", i % 1000)], active: i % 7 != 0 })
            .collect();
        for (what, f) in [
            ("no filter, sorted by name, last page", json!([])),
            ("name contains 99, age > 50", json!([[{ "col": "name", "op": "contains", "vals": ["99"] }, { "col": "age", "op": ">", "vals": [50] }]])),
            ("regexp on tags, or inactive", json!([[{ "col": "tags", "op": "regexp", "vals": ["^ou12[0-9]$"] }], [{ "col": "active", "op": "=", "vals": ["false"] }]])),
        ] {
            let mut query = q(f, Some("name"), true);
            query.offset = 999_950;
            let t = Instant::now();
            let p = page(ps.iter(), &query).unwrap();
            println!("{what}: {} rows meet it, {} on the page, {:?}", p.total, p.rows.len(), t.elapsed());
        }
    }
}
