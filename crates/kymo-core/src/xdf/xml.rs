//! A small XML reader for XDF's file and stream headers: elements with their text and
//! children. Attributes, comments, processing instructions and the doctype are skipped;
//! CDATA sections and the predefined and numeric character references are read as text.

/// One element: its name, its own text (all text directly inside it, joined) and its child
/// elements, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Element {
    pub name: String,
    pub text: String,
    pub children: Vec<Element>,
}

/// Elements nested deeper than this are an error, so a damaged header can't build a tree
/// whose drop overflows the stack.
const MAX_DEPTH: usize = 64;

impl Element {
    /// The first child named `name`.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Every child named `name`.
    pub fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// The text of the first child named `name`, as written.
    pub fn text_of(&self, name: &str) -> Option<&str> {
        self.child(name).map(|c| c.text.as_str())
    }
}

/// The root element of `bytes` (UTF-8, invalid sequences replaced), or why it isn't XML.
pub(crate) fn parse(bytes: &[u8]) -> Result<Element, String> {
    let text = String::from_utf8_lossy(bytes);
    let s = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut stack: Vec<Element> = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if let Some(after) = rest.strip_prefix("<?") {
            i += 2 + skip_past(after, "?>")?;
        } else if let Some(after) = rest.strip_prefix("<!--") {
            i += 4 + skip_past(after, "-->")?;
        } else if let Some(after) = rest.strip_prefix("<![CDATA[") {
            let end = after.find("]]>").ok_or("unterminated CDATA section")?;
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&after[..end]);
            }
            i += 9 + end + 3;
        } else if let Some(after) = rest.strip_prefix("<!") {
            i += 2 + skip_past(after, ">")?;
        } else if let Some(after) = rest.strip_prefix("</") {
            let end = after.find('>').ok_or("unterminated closing tag")?;
            let name = after[..end].trim();
            let element = stack.pop().ok_or("closing tag without an opening one")?;
            if element.name != name {
                return Err(format!(
                    "closing tag </{name}> doesn't match <{}>",
                    element.name
                ));
            }
            match stack.last_mut() {
                Some(parent) => parent.children.push(element),
                None => return Ok(element),
            }
            i += 2 + end + 1;
        } else if let Some(after) = rest.strip_prefix('<') {
            let (name, len, closed) = open_tag(after)?;
            let element = Element {
                name,
                ..Element::default()
            };
            i += 1 + len;
            if closed {
                match stack.last_mut() {
                    Some(parent) => parent.children.push(element),
                    None => return Ok(element),
                }
            } else {
                if stack.len() >= MAX_DEPTH {
                    return Err(format!("elements nested deeper than {MAX_DEPTH}"));
                }
                stack.push(element);
            }
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            if let Some(top) = stack.last_mut() {
                unescape(&rest[..end], &mut top.text);
            }
            i += end;
        }
    }
    Err(match stack.first() {
        Some(e) => format!("element <{}> is not closed", e.name),
        None => "no root element".to_owned(),
    })
}

/// The bytes up to and including the first `end` in `s`.
fn skip_past(s: &str, end: &str) -> Result<usize, String> {
    s.find(end)
        .map(|k| k + end.len())
        .ok_or_else(|| format!("unterminated markup (no {end:?})"))
}

/// An opening tag after its `<`: its name, its length up to and including `>`, and whether
/// it closes itself (`<a/>`). Attribute values may hold `>` inside quotes.
fn open_tag(s: &str) -> Result<(String, usize, bool), String> {
    let name_end = s
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .ok_or("unterminated tag")?;
    let name = &s[..name_end];
    if name.is_empty() {
        return Err("tag without a name".into());
    }
    let mut quote = None;
    for (k, c) in s[name_end..].char_indices() {
        let k = name_end + k;
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => {
                let closed = s[..k].ends_with('/');
                return Ok((name.to_owned(), k + 1, closed));
            }
            _ => {}
        }
    }
    Err(format!("tag <{name}> is not terminated"))
}

/// Appends `s` to `out` with character references replaced; unknown ones stay as written.
fn unescape(s: &str, out: &mut String) {
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let replaced = rest.find(';').filter(|&k| k <= 12).and_then(|semi| {
            let entity = &rest[1..semi];
            let c = match entity {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .map(|hex| u32::from_str_radix(hex, 16))
                    .or_else(|| entity.strip_prefix('#').map(str::parse))
                    .and_then(Result::ok)
                    .and_then(char::from_u32),
            }?;
            Some((c, semi))
        });
        match replaced {
            Some((c, semi)) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn reads_elements_text_and_references() {
        let xml = "\u{feff}<?xml version=\"1.0\"?><!-- c --><info a=\"x>y\"><name>A &amp; B&#33;\
                   &#x41;&bogus;</name><e/><desc><![CDATA[<raw>]]></desc></info>trailing";
        let root = parse(xml.as_bytes()).unwrap();
        assert_eq!(root.name, "info");
        assert_eq!(root.text_of("name"), Some("A & B!A&bogus;"));
        assert_eq!(root.text_of("e"), Some(""));
        assert_eq!(root.text_of("desc"), Some("<raw>"));
    }

    #[test]
    fn rejects_malformed_xml() {
        for bad in ["", "<a>", "<a></b>", "</a>", "<a", "<a><b x='1></a>"] {
            assert!(parse(bad.as_bytes()).is_err(), "{bad:?}");
        }
        let deep = "<a>".repeat(MAX_DEPTH + 1);
        assert!(parse(deep.as_bytes()).is_err());
    }
}
