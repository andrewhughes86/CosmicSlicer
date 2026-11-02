//! XML for the formats built on it: a streaming walk (3MF, AMF, whose vertices would make a
//! tree too big) and a small tree (COLLADA, X3D). Names lose their namespace prefix.

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

use crate::ImportError;

/// Deepest element nesting accepted.
const MAX_DEPTH: usize = 512;
/// Most elements in a tree.
const MAX_NODES: usize = 20_000_000;

pub enum Ev<'a> {
    Start(&'a str, &'a [(String, String)]),
    End(&'a str),
    Text(&'a str),
}

fn malformed(e: impl std::fmt::Display) -> ImportError {
    ImportError::Malformed(format!("XML: {e}"))
}

/// The text a general entity reference (`&amp;`, `&#65;`) stands for.
fn entity(name: &str) -> String {
    match name {
        "amp" => "&".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "quot" => "\"".into(),
        "apos" => "'".into(),
        _ => {
            let code = name
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()));
            code.and_then(char::from_u32).map(String::from).unwrap_or_default()
        }
    }
}

/// Calls `f` for every element start and end and every run of text, in document order.
pub fn stream(bytes: &[u8], mut f: impl FnMut(Ev<'_>) -> Result<(), ImportError>) -> Result<(), ImportError> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let mut reader = Reader::from_reader(bytes);
    let mut depth = 0usize;
    let mut attrs: Vec<(String, String)> = Vec::new();
    loop {
        match reader.read_event().map_err(malformed)? {
            Event::Start(e) | Event::Empty(e) if depth >= MAX_DEPTH => {
                let _ = e;
                return Err(ImportError::Malformed("XML nests too deeply".into()));
            }
            ev @ (Event::Start(_) | Event::Empty(_)) => {
                let (empty, e) = match ev {
                    Event::Empty(e) => (true, e),
                    Event::Start(e) => (false, e),
                    _ => continue,
                };
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                attrs.clear();
                for a in e.attributes().flatten() {
                    let key = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
                    let value = a.normalized_value(XmlVersion::Implicit1_0).map(|v| v.into_owned()).unwrap_or_default();
                    attrs.push((key, value));
                }
                f(Ev::Start(&name, &attrs))?;
                if empty {
                    f(Ev::End(&name))?;
                } else {
                    depth += 1;
                }
            }
            Event::End(e) => {
                depth = depth.saturating_sub(1);
                f(Ev::End(&String::from_utf8_lossy(e.local_name().as_ref())))?;
            }
            Event::Text(t) => f(Ev::Text(&t.decode().map_err(malformed)?))?,
            Event::CData(t) => f(Ev::Text(&t.decode().map_err(malformed)?))?,
            Event::GeneralRef(r) => f(Ev::Text(&entity(&r.decode().map_err(malformed)?)))?,
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// An element: its local name, attributes, child elements and text.
#[derive(Debug, Default)]
pub struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
    pub text: String,
}

impl Node {
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// Every element below this one (depth first) named `name`.
    pub fn descendants<'a>(&'a self, name: &'a str) -> Vec<&'a Node> {
        let mut out = Vec::new();
        let mut stack: Vec<&Node> = self.children.iter().rev().collect();
        while let Some(n) = stack.pop() {
            if n.name == name {
                out.push(n);
            }
            stack.extend(n.children.iter().rev());
        }
        out
    }
}

/// Parses a document into a tree; the returned node holds the root element as its child.
pub fn tree(bytes: &[u8]) -> Result<Node, ImportError> {
    let mut stack = vec![Node::default()];
    let mut count = 0usize;
    stream(bytes, |ev| {
        match ev {
            Ev::Start(name, attrs) => {
                count += 1;
                if count > MAX_NODES {
                    return Err(ImportError::Malformed("XML has too many elements".into()));
                }
                stack.push(Node { name: name.to_owned(), attrs: attrs.to_vec(), ..Node::default() });
            }
            Ev::End(_) => {
                if stack.len() > 1
                    && let Some(n) = stack.pop()
                    && let Some(parent) = stack.last_mut()
                {
                    parent.children.push(n);
                }
            }
            Ev::Text(t) => {
                if let Some(n) = stack.last_mut() {
                    n.text.push_str(t);
                }
            }
        }
        Ok(())
    })?;
    // An unclosed document: fold what is open into its parents.
    while stack.len() > 1 {
        if let Some(n) = stack.pop()
            && let Some(parent) = stack.last_mut()
        {
            parent.children.push(n);
        }
    }
    stack.pop().ok_or_else(|| ImportError::Malformed("empty XML".into()))
}

/// Whitespace- or comma-separated numbers in `text`; anything else is an error.
pub fn floats(text: &str) -> Result<Vec<f32>, ImportError> {
    text.split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .map(|w| {
            crate::finite(w).ok_or_else(|| ImportError::Malformed(format!("`{}` is not a finite number", short(w))))
        })
        .collect()
}

/// Whitespace- or comma-separated integers in `text`.
pub fn ints(text: &str) -> Result<Vec<i64>, ImportError> {
    text.split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .map(|w| w.parse::<i64>().map_err(|_| ImportError::Malformed(format!("`{}` is not an integer", short(w)))))
        .collect()
}

/// The first 32 characters of `s`, for messages.
pub fn short(s: &str) -> String {
    s.chars().take(32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_keeps_attributes_children_and_text() -> Result<(), ImportError> {
        let doc = tree(b"<?xml version='1.0'?><a:root x='1 &amp; 2'><b>1 2</b><b/>tail &lt;</a:root>")?;
        let root = doc.child("root").ok_or(ImportError::Empty)?;
        assert_eq!(root.attr("x"), Some("1 & 2"));
        assert_eq!(root.children_named("b").count(), 2);
        assert_eq!(root.descendants("b").len(), 2);
        assert_eq!(root.text, "tail <");
        assert_eq!(floats("1, 2.5 -3e2")?, [1.0, 2.5, -300.0]);
        assert!(floats("1 nan").is_err());
        Ok(())
    }

    #[test]
    fn broken_documents_do_not_panic() {
        for doc in [&b"<a><b></a>"[..], b"<a", b"<<<>>>", b"<a x='1>", b"\xff\xfe<\x00a\x00>\x00"] {
            let _ = tree(doc);
        }
        let deep = "<a>".repeat(MAX_DEPTH + 5);
        assert!(tree(deep.as_bytes()).is_err());
    }
}
