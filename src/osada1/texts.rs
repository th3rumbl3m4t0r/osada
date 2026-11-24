//! The UI texts. static/texts.js has every text in CZ, SK, HU, FR and AT (Austrian German), keyed by the
//! English; here it is split into one pack per language (`lang-<code>.js`), so a browser
//! loads only the language it shows (static/i18n.js). The file is a JavaScript object
//! literal, and this reads the part of JavaScript it uses: strings in '…' or "…", joined
//! with +; keys as a string or as [string]; values as an array of one per language,
//! each a string or an array of plural forms; comments.

use serde_json::{json, Map, Value};

/// the languages of the columns, in their order
pub const LANGS: [&str; N] = ["cs", "sk", "hu", "fr", "de"];
const N: usize = 5;

#[derive(Debug, PartialEq)]
enum Tok {
    Str(String),
    P(char),
    Word(String),
}

fn tokens(src: &str) -> Result<Vec<Tok>, String> {
    let s: Vec<char> = src.chars().collect();
    let (mut i, mut out) = (0, Vec::new());
    let line = |i: usize| s[..i].iter().filter(|&&c| c == '\n').count() + 1;
    while i < s.len() {
        let c = s[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && s.get(i + 1) == Some(&'/') {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && s.get(i + 1) == Some(&'*') {
            let end = (i + 2..s.len().saturating_sub(1)).find(|&j| s[j] == '*' && s[j + 1] == '/').ok_or("unclosed comment")?;
            i = end + 2;
        } else if c == '\'' || c == '"' {
            let start = i;
            let mut t = String::new();
            i += 1;
            loop {
                let Some(&d) = s.get(i) else { return Err(format!("line {}: unclosed string", line(start))) };
                i += 1;
                match d {
                    _ if d == c => break,
                    '\n' => return Err(format!("line {}: newline in a string", line(start))),
                    '\\' => {
                        let e = *s.get(i).ok_or("escape at the end")?;
                        i += 1;
                        match e {
                            'n' => t.push('\n'),
                            't' => t.push('\t'),
                            'r' => t.push('\r'),
                            'u' => {
                                let hex: String = s.get(i..i + 4).ok_or("short \\u")?.iter().collect();
                                let n = u32::from_str_radix(&hex, 16).map_err(|_| format!("line {}: bad \\u{hex}", line(start)))?;
                                t.push(char::from_u32(n).ok_or("bad \\u")?);
                                i += 4;
                            }
                            other => t.push(other),
                        }
                    }
                    _ => t.push(d),
                }
            }
            out.push(Tok::Str(t));
        } else if c.is_ascii_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < s.len() && (s[i].is_ascii_alphanumeric() || s[i] == '_' || s[i] == '$') {
                i += 1;
            }
            out.push(Tok::Word(s[start..i].iter().collect()));
        } else if "{}[]:,+=;".contains(c) {
            out.push(Tok::P(c));
            i += 1;
        } else {
            return Err(format!("line {}: unexpected {c:?}", line(i)));
        }
    }
    Ok(out)
}

struct Parser {
    t: Vec<Tok>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn is(&self, c: char) -> bool {
        self.peek() == Some(&Tok::P(c))
    }
    fn eat(&mut self, c: char) -> bool {
        let y = self.is(c);
        if y {
            self.i += 1;
        }
        y
    }
    fn want(&mut self, c: char, what: &str) -> Result<(), String> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(format!("{what}: expected {c:?}, found {:?}", self.peek()))
        }
    }
    /// 'a' + 'b' + …
    fn string(&mut self, what: &str) -> Result<String, String> {
        let mut s = String::new();
        loop {
            match self.t.get(self.i) {
                Some(Tok::Str(x)) => {
                    s.push_str(x);
                    self.i += 1;
                }
                other => return Err(format!("{what}: expected a string, found {other:?}")),
            }
            if !self.eat('+') {
                return Ok(s);
            }
        }
    }
}

/// The table: each key with its values, one per language (a string, or the plural forms).
pub fn parse(src: &str) -> Result<Vec<(String, [Value; N])>, String> {
    let t = tokens(src)?;
    let start = t
        .windows(4)
        .position(|w| w == [Tok::Word("const".into()), Tok::Word("TEXTS".into()), Tok::P('='), Tok::P('{')])
        .ok_or("no `const TEXTS = {`")?;
    let mut p = Parser { t, i: start + 4 };
    let mut out: Vec<(String, [Value; N])> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    while !p.eat('}') {
        let key = if p.eat('[') {
            let k = p.string("a [key]")?;
            p.want(']', "a [key]")?;
            k
        } else {
            p.string("a key")?
        };
        let at = format!("{key:?}");
        p.want(':', &at)?;
        p.want('[', &at)?;
        let mut vals = Vec::new();
        while !p.eat(']') {
            if p.eat('[') {
                let mut forms = Vec::new();
                while !p.eat(']') {
                    forms.push(Value::String(p.string(&at)?));
                    if !p.eat(',') {
                        p.want(']', &at)?;
                        break;
                    }
                }
                vals.push(Value::Array(forms));
            } else {
                vals.push(Value::String(p.string(&at)?));
            }
            if !p.eat(',') {
                p.want(']', &at)?;
                break;
            }
        }
        let vals: [Value; N] = vals.try_into().map_err(|v: Vec<Value>| format!("{at}: {} values, not {N}", v.len()))?;
        if !seen.insert(key.clone()) {
            return Err(format!("{at} twice"));
        }
        out.push((key, vals));
        if !p.eat(',') {
            p.want('}', "the table")?;
            break;
        }
    }
    Ok(out)
}

/// `lang-<code>.js` for each of LANGS: `osadaTexts("cs", {…});`
pub fn packs(src: &str) -> Result<Vec<(String, String)>, String> {
    let table = parse(src)?;
    Ok(LANGS
        .iter()
        .enumerate()
        .map(|(i, code)| {
            let m: Map<String, Value> = table.iter().map(|(k, v)| (k.clone(), v[i].clone())).collect();
            (format!("lang-{code}.js"), format!("osadaTexts({}, {});\n", json!(code), Value::Object(m)))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_table_uses() {
        let src = r#"/* x */ const TEXTS = {
          // a comment
          'a': ['á', "b\"", 'c\'', 'dé', 'ä'],
          ['long ' +
            'key']: ['x' + 'y', 'z', ['1' + '', 'w'], ['one', 'more',], ['eins', 'mehr']],
        };"#;
        let t = parse(src).unwrap();
        assert_eq!(t[0], ("a".into(), [json!("á"), json!("b\""), json!("c'"), json!("dé"), json!("ä")]));
        assert_eq!(t[1].0, "long key");
        assert_eq!(t[1].1, [json!("xy"), json!("z"), json!(["1", "w"]), json!(["one", "more"]), json!(["eins", "mehr"])]);
        assert!(parse("const TEXTS = { 'a': ['1', '2', '3', '4'] };").unwrap_err().contains("4 values"));
        assert!(parse("const TEXTS = { 'a': ['1','2','3','4','5'], 'a': ['1','2','3','4','5'] };").unwrap_err().contains("twice"));
    }

    #[test]
    fn the_shipped_table_splits() {
        let src = include_str!("../../static1/texts.js");
        let t = parse(src).unwrap();
        assert!(t.len() > 600, "{}", t.len());
        let del = t.iter().find(|(k, _)| k == "delete").expect("delete");
        assert_eq!(del.1[0], json!("smazat"));
        // plural forms: three in cs and sk, one in hu, two in fr and de
        let pl = t.iter().find(|(k, _)| k == "ok: {n} entries").expect("plural");
        assert_eq!([pl.1[0].as_array().unwrap().len(), pl.1[3].as_array().unwrap().len(), pl.1[4].as_array().unwrap().len()], [3, 2, 2]);
        assert!(pl.1[2].is_string());
        assert_eq!(del.1[4], json!("löschen"));
        let packs = packs(src).unwrap();
        assert_eq!(packs.len(), 5);
        assert_eq!(packs[4].0, "lang-de.js");
        let (name, js) = &packs[0];
        assert_eq!(name, "lang-cs.js");
        let body = js.strip_prefix("osadaTexts(\"cs\", ").unwrap().strip_suffix(");\n").unwrap();
        let m: Map<String, Value> = serde_json::from_str(body).unwrap();
        assert_eq!(m.len(), t.len());
    }
}
