//! The ISO 10303-21 exchange structure ("Part 21", the text of a STEP file): the DATA
//! section's entity instances, `#12 = NAME(args);` or complex `#12 = (A(…) B(…));`.
//!
//! Values are kept untyped (numbers, strings, enumerations, references, lists, typed values);
//! the schema-aware reading happens in [`super`]. Comments are skipped. A damaged instance is
//! left out rather than failing the file.

use std::collections::HashMap;

use crate::ImportError;

/// Most instances read.
const MAX_INSTANCES: usize = 50_000_000;
/// Deepest list nesting.
const MAX_NESTING: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub enum P {
    Int(i64),
    Real(f64),
    Str(String),
    Enum(String),
    Ref(u64),
    /// `$`, an omitted value.
    Unset,
    /// `*`, a value derived from others.
    Derived,
    List(Vec<P>),
    /// `NAME(value)`, a value with its type spelled out.
    Typed(String, Vec<P>),
}

impl P {
    pub fn num(&self) -> Option<f64> {
        match self {
            Self::Int(i) => Some(*i as f64),
            Self::Real(r) => Some(*r),
            Self::Typed(_, v) => v.first().and_then(P::num),
            _ => None,
        }
    }

    pub fn reference(&self) -> Option<u64> {
        match self {
            Self::Ref(r) => Some(*r),
            _ => None,
        }
    }

    pub fn list(&self) -> &[P] {
        match self {
            Self::List(l) => l,
            _ => &[],
        }
    }

    pub fn flag(&self) -> Option<bool> {
        match self {
            Self::Enum(e) if e == "T" => Some(true),
            Self::Enum(e) if e == "F" => Some(false),
            _ => None,
        }
    }

    pub fn enumeration(&self) -> Option<&str> {
        match self {
            Self::Enum(e) => Some(e),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// An instance: one (simple) or several (complex) named records.
#[derive(Clone, Debug, Default)]
pub struct Instance {
    pub parts: Vec<(String, Vec<P>)>,
}

impl Instance {
    /// The record named `name`.
    pub fn part(&self, name: &str) -> Option<&[P]> {
        self.parts.iter().find(|(n, _)| n == name).map(|(_, a)| a.as_slice())
    }

    /// The type of a simple instance (the first record's of a complex one).
    pub fn kind(&self) -> &str {
        self.parts.first().map_or("", |(n, _)| n.as_str())
    }

    pub fn is(&self, name: &str) -> bool {
        self.parts.iter().any(|(n, _)| n == name)
    }
}

struct Lexer<'a> {
    b: &'a [u8],
    at: usize,
}

impl Lexer<'_> {
    /// Skips white space and comments.
    fn skip(&mut self) {
        loop {
            while self.b.get(self.at).is_some_and(|c| c.is_ascii_whitespace()) {
                self.at += 1;
            }
            if self.b.get(self.at..self.at + 2) == Some(b"/*") {
                self.at += 2;
                while self.at < self.b.len() && self.b.get(self.at..self.at + 2) != Some(b"*/") {
                    self.at += 1;
                }
                self.at = (self.at + 2).min(self.b.len());
            } else {
                return;
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.b.get(self.at).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn keyword(&mut self) -> Option<String> {
        self.skip();
        let start = self.at;
        while self.b.get(self.at).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'!') {
            self.at += 1;
        }
        (self.at > start)
            .then(|| String::from_utf8_lossy(self.b.get(start..self.at).unwrap_or_default()).to_ascii_uppercase())
    }

    fn value(&mut self, depth: usize) -> Option<P> {
        if depth > MAX_NESTING {
            return None;
        }
        match self.peek()? {
            b'(' => {
                self.at += 1;
                let mut items = Vec::new();
                if self.eat(b')') {
                    return Some(P::List(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    if self.eat(b')') {
                        return Some(P::List(items));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'#' => {
                self.at += 1;
                let start = self.at;
                while self.b.get(self.at).is_some_and(u8::is_ascii_digit) {
                    self.at += 1;
                }
                std::str::from_utf8(self.b.get(start..self.at)?).ok()?.parse().ok().map(P::Ref)
            }
            b'$' => {
                self.at += 1;
                Some(P::Unset)
            }
            b'*' => {
                self.at += 1;
                Some(P::Derived)
            }
            b'.' if self.b.get(self.at + 1).is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_') => {
                self.at += 1;
                let start = self.at;
                while self.b.get(self.at).is_some_and(|c| *c != b'.') {
                    self.at += 1;
                }
                let e = String::from_utf8_lossy(self.b.get(start..self.at)?).to_ascii_uppercase();
                self.at += 1;
                Some(P::Enum(e))
            }
            b'\'' => {
                self.at += 1;
                let mut s = Vec::new();
                loop {
                    let c = *self.b.get(self.at)?;
                    self.at += 1;
                    if c == b'\'' {
                        if self.b.get(self.at) == Some(&b'\'') {
                            s.push(b'\'');
                            self.at += 1;
                        } else {
                            break;
                        }
                    } else {
                        s.push(c);
                    }
                }
                Some(P::Str(String::from_utf8_lossy(&s).into_owned()))
            }
            b'"' => {
                self.at += 1;
                while self.b.get(self.at).is_some_and(|c| *c != b'"') {
                    self.at += 1;
                }
                self.at += 1;
                Some(P::Unset)
            }
            c if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() => {
                let start = self.at;
                self.at += 1;
                while self
                    .b
                    .get(self.at)
                    .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'-' | b'+'))
                {
                    self.at += 1;
                }
                let t = std::str::from_utf8(self.b.get(start..self.at)?).ok()?;
                if t.contains(['.', 'e', 'E']) {
                    // Part 21 allows "1." and ".5"; Rust parses both.
                    t.parse::<f64>().ok().filter(|v| v.is_finite()).map(P::Real)
                } else {
                    t.parse::<i64>()
                        .ok()
                        .map(P::Int)
                        .or_else(|| t.parse::<f64>().ok().filter(|v| v.is_finite()).map(P::Real))
                }
            }
            c if c.is_ascii_alphabetic() || c == b'!' => {
                let name = self.keyword()?;
                if self.peek() == Some(b'(') {
                    match self.value(depth + 1)? {
                        P::List(args) => Some(P::Typed(name, args)),
                        _ => None,
                    }
                } else {
                    Some(P::Enum(name))
                }
            }
            _ => None,
        }
    }

    /// `NAME(args)`.
    fn record(&mut self) -> Option<(String, Vec<P>)> {
        let name = self.keyword()?;
        match self.value(0)? {
            P::List(args) => Some((name, args)),
            _ => None,
        }
    }

    /// Moves past the next `;` that is not inside a string.
    fn skip_statement(&mut self) {
        let mut in_string = false;
        while let Some(&c) = self.b.get(self.at) {
            self.at += 1;
            match c {
                b'\'' => in_string = !in_string,
                b';' if !in_string => return,
                _ => {}
            }
        }
    }
}

/// The instances of the DATA section(s), by number.
pub fn parse(bytes: &[u8]) -> Result<HashMap<u64, Instance>, ImportError> {
    let head = bytes.get(..bytes.len().min(256)).unwrap_or_default();
    if !String::from_utf8_lossy(head).trim_start().starts_with("ISO-10303-21") {
        return Err(ImportError::Malformed("not a STEP file: it does not start with ISO-10303-21".into()));
    }
    let mut lx = Lexer { b: bytes, at: 0 };
    let mut out: HashMap<u64, Instance> = HashMap::new();
    let mut in_data = false;
    while lx.peek().is_some() {
        if lx.peek() == Some(b'#') && in_data {
            let start = lx.at;
            let parsed = (|| {
                let id = lx.value(0)?.reference()?;
                if !lx.eat(b'=') {
                    return None;
                }
                let mut inst = Instance::default();
                if lx.peek() == Some(b'(') {
                    lx.at += 1;
                    while !lx.eat(b')') {
                        inst.parts.push(lx.record()?);
                        if inst.parts.len() > 64 {
                            return None;
                        }
                    }
                } else {
                    inst.parts.push(lx.record()?);
                }
                lx.eat(b';').then_some((id, inst))
            })();
            match parsed {
                Some((id, inst)) => {
                    if out.len() >= MAX_INSTANCES {
                        return Err(ImportError::Malformed("the STEP file has too many instances".into()));
                    }
                    out.insert(id, inst);
                }
                None => {
                    lx.at = start;
                    lx.skip_statement();
                }
            }
            continue;
        }
        match lx.keyword().as_deref() {
            Some("DATA") => {
                in_data = true;
                // DATA may carry a name and schema list: DATA('name', ('schema'));
                if lx.peek() == Some(b'(') {
                    let _ = lx.value(0);
                }
                lx.eat(b';');
            }
            Some("ENDSEC") => {
                in_data = false;
                lx.eat(b';');
            }
            Some(_) => lx.skip_statement(),
            None => {
                lx.at += 1;
                lx.skip_statement();
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_and_complex_instances() -> Result<(), ImportError> {
        let text = b"ISO-10303-21;\nHEADER;\nFILE_NAME('a;b','',(''),(''),'','','');\nENDSEC;\nDATA;\n\
            #1=CARTESIAN_POINT('it''s',(0.,1.5,-2.E1));\n/* comment ; */\n#2 = ( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.) );\n\
            #3=MEASURE_WITH_UNIT(LENGTH_MEASURE(25.4),#2);\n#4=BROKEN(((;\n#5=VERTEX_POINT('',#1);\nENDSEC;\nEND-ISO-10303-21;\n";
        let m = parse(text)?;
        assert_eq!(m.len(), 4);
        let p = m.get(&1).ok_or(ImportError::Empty)?;
        assert_eq!(p.kind(), "CARTESIAN_POINT");
        assert_eq!(p.parts[0].1[0], P::Str("it's".into()));
        assert_eq!(p.parts[0].1[1].list().iter().filter_map(P::num).collect::<Vec<_>>(), [0.0, 1.5, -20.0]);
        let u = m.get(&2).ok_or(ImportError::Empty)?;
        assert_eq!(u.part("SI_UNIT"), Some(&[P::Enum("MILLI".into()), P::Enum("METRE".into())][..]));
        assert_eq!(m.get(&3).and_then(|i| i.parts[0].1[0].num()), Some(25.4));
        assert_eq!(m.get(&5).and_then(|i| i.parts[0].1[1].reference()), Some(1));
        Ok(())
    }

    #[test]
    fn not_step_and_garbage() {
        assert!(parse(b"solid x").is_err());
        let _ = parse(b"ISO-10303-21;DATA;#1=A(((((((((");
        let _ = parse(b"ISO-10303-21;DATA;#1=A('unterminated");
        let deep = format!("ISO-10303-21;DATA;#1=A({});", "(".repeat(500));
        let _ = parse(deep.as_bytes());
    }
}
