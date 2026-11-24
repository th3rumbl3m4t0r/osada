use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag, TagEnd};

/// Markdown to HTML for the preview and viewer. Raw HTML is shown as text and
/// only http(s)/mailto or relative urls survive. Relative urls are prefixed:
/// images with `img_base` (raw files), links with `link_base` (the viewer).
pub fn render(text: &str, img_base: Option<&str>, link_base: Option<&str>) -> String {
    let events = Parser::new_ext(text, options()).map(|ev| match ev {
        Event::Html(h) | Event::InlineHtml(h) => Event::Text(h),
        Event::Start(Tag::Link { link_type, dest_url, title, id }) => {
            Event::Start(Tag::Link { link_type, dest_url: fix(dest_url, link_base, false), title, id })
        }
        Event::Start(Tag::Image { link_type, dest_url, title, id }) => {
            Event::Start(Tag::Image { link_type, dest_url: fix(dest_url, img_base, true), title, id })
        }
        e => e,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_FOOTNOTES
}

/// The pictures and links of a text, as written: (a picture?, its url, its words:
/// a picture's alt text, a link's text).
pub fn targets(text: &str) -> Vec<(bool, String, String)> {
    let mut out: Vec<(bool, String, String)> = Vec::new();
    // the ones open: a picture may be inside a link
    let mut open: Vec<usize> = Vec::new();
    for ev in Parser::new_ext(text, options()) {
        match ev {
            Event::Start(Tag::Link { dest_url, .. }) => {
                open.push(out.len());
                out.push((false, dest_url.trim().to_string(), String::new()));
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                open.push(out.len());
                out.push((true, dest_url.trim().to_string(), String::new()));
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                open.pop();
            }
            Event::Text(t) | Event::Code(t) => open.iter().for_each(|&i| out[i].2.push_str(&t)),
            Event::SoftBreak | Event::HardBreak => open.iter().for_each(|&i| out[i].2.push(' ')),
            _ => {}
        }
    }
    out
}

/// Links to wiki articles (`/wiki/<id>`) in rendered HTML get `class="wiki-ref"`,
/// and `wiki-ref missing` when there is no such article yet (yet to be written).
pub fn mark_refs(html: &str, exists: impl Fn(&str) -> bool) -> String {
    const A: &str = "<a href=\"/wiki/";
    let mut out = String::with_capacity(html.len() + 64);
    let mut rest = html;
    while let Some(i) = rest.find(A) {
        out.push_str(&rest[..i]);
        let after = &rest[i + A.len()..];
        let id = &after[..after.find(['"', '?', '#']).unwrap_or(after.len())];
        // the list, a new article, an article's history: not references
        if id.is_empty() || id.contains('/') || id == "new" {
            out.push_str(A);
        } else {
            out.push_str(if exists(id) { "<a class=\"wiki-ref\" href=\"/wiki/" } else { "<a class=\"wiki-ref missing\" href=\"/wiki/" });
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

fn fix<'a>(url: CowStr<'a>, base: Option<&str>, image: bool) -> CowStr<'a> {
    let u = url.trim();
    if let Some((scheme, _)) = u.split_once(':') {
        if !scheme.contains(['/', '?', '#']) {
            let s = scheme.to_ascii_lowercase();
            let ok = matches!(s.as_str(), "http" | "https") || (s == "mailto" && !image);
            return if ok { url } else { CowStr::Borrowed("#") };
        }
    }
    if u.is_empty() || u.starts_with(['#', '/']) {
        return url;
    }
    match base {
        Some(b) => CowStr::from(format!("{b}{u}")),
        None => url,
    }
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn sanitizes() {
        let h = render("<script>x</script>\n\n[a](javascript:alert(1)) ![i](p.png) [b](c.md)", Some("/raw/"), Some("/view/"));
        assert!(!h.contains("<script>"), "{h}");
        assert!(h.contains("href=\"#\""), "{h}");
        assert!(h.contains("src=\"/raw/p.png\""), "{h}");
        assert!(h.contains("href=\"/view/c.md\""), "{h}");
    }

    #[test]
    fn targets() {
        let t = super::targets("![Jiří *u* kovárny](jiri.png)\n\n> zápis\n\n[celá\nstrana](/api/wiki/x/media/a.jpg) · [archiv](https://x.org) [![](b.png)](c)");
        assert_eq!(t[0], (true, "jiri.png".into(), "Jiří u kovárny".into()));
        assert_eq!(t[1], (false, "/api/wiki/x/media/a.jpg".into(), "celá strana".into()));
        assert_eq!(t[2], (false, "https://x.org".into(), "archiv".into()));
        assert_eq!((t[3].1.as_str(), t[4].0, t[4].1.as_str()), ("c", true, "b.png"));
    }

    #[test]
    fn wiki_references() {
        let h = render("[a](k3x9q2pa) [b](nothere) [c](/wiki/) [d](/wiki/new) [e](https://x.org)", None, Some("/wiki/"));
        let m = super::mark_refs(&h, |id| id == "k3x9q2pa");
        assert!(m.contains("<a class=\"wiki-ref\" href=\"/wiki/k3x9q2pa\">a</a>"), "{m}");
        assert!(m.contains("<a class=\"wiki-ref missing\" href=\"/wiki/nothere\">b</a>"), "{m}");
        assert!(m.contains("<a href=\"/wiki/\">c</a>") && m.contains("<a href=\"/wiki/new\">d</a>") && m.contains("<a href=\"https://x.org\">e</a>"), "{m}");
    }
}
