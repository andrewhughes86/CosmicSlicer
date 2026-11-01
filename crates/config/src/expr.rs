//! OrcaSlicer's custom G-code macro language (PrusaSlicer's PlaceholderParser): `[var]` and
//! `{expression}` substitutions, `{if}` / `{elsif}` / `{else}` / `{endif}` blocks and
//! `{local x = …}` / `{global x = …}` variables, used in start and end G-code and in
//! `compatible_printers_condition`.
//!
//! Anything that cannot be evaluated is an error naming what went wrong. Nothing is passed
//! through unevaluated: a printer must never receive `{if …}` as G-code.

use std::collections::HashMap;
use std::fmt;

/// A value in an expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Num(f64),
    Str(String),
    Bool(bool),
    Vec(Vec<Value>),
    Nil,
}

impl Value {
    pub fn truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Vec(v) => !v.is_empty(),
            Value::Nil => false,
        }
    }

    fn num(&self) -> Result<f64, MacroError> {
        match self {
            Value::Num(n) => Ok(*n),
            Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            Value::Str(s) => s
                .trim()
                .trim_end_matches('%')
                .parse::<f64>()
                .map_err(|_| MacroError::Type(format!("\"{s}\" is not a number"))),
            Value::Vec(v) => {
                v.first().map_or(Err(MacroError::Type("an empty list is not a number".into())), Value::num)
            }
            Value::Nil => Err(MacroError::Type("nil is not a number".into())),
        }
    }

    /// As G-code text: whole numbers without decimals, others as written with up to 6 places.
    pub fn text(&self) -> String {
        match self {
            Value::Num(n) => format_number(*n),
            Value::Str(s) => s.clone(),
            Value::Bool(b) => if *b { "true" } else { "false" }.into(),
            Value::Vec(v) => v.iter().map(Value::text).collect::<Vec<_>>().join(","),
            Value::Nil => "nil".into(),
        }
    }
}

fn format_number(n: f64) -> String {
    if !n.is_finite() {
        return "0".into();
    }
    if (n - n.round()).abs() < 1e-9 && n.abs() < 1e15 {
        return format!("{}", n.round() as i64);
    }
    let s = format!("{n:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

#[derive(Clone, Debug, PartialEq)]
pub enum MacroError {
    Unknown(String),
    Syntax(String),
    Type(String),
}

impl fmt::Display for MacroError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MacroError::Unknown(v) => write!(f, "unknown variable \"{v}\""),
            MacroError::Syntax(s) => write!(f, "syntax error: {s}"),
            MacroError::Type(s) => write!(f, "{s}"),
        }
    }
}

/// Where variables come from.
pub trait Vars {
    fn var(&self, name: &str) -> Option<Value>;
}

impl Vars for HashMap<String, Value> {
    fn var(&self, name: &str) -> Option<Value> {
        self.get(name).cloned()
    }
}

/// Variables from a config: each option typed by the schema (lists become `Vec`, booleans
/// `Bool`, numbers `Num`), with `extra` values taking precedence.
pub struct ConfigVars<'a> {
    pub schema: &'a crate::Schema,
    pub config: &'a crate::Config,
    pub extra: HashMap<String, Value>,
}

/// One option value as a typed value.
pub fn typed(def: Option<&crate::OptionDef>, raw: &str) -> Value {
    use crate::OptionType;
    let scalar = |s: &str| -> Value {
        match def.map(crate::OptionDef::kind) {
            Some(OptionType::Bool) => Value::Bool(matches!(s.trim(), "1" | "true")),
            Some(OptionType::Float | OptionType::Int | OptionType::Percent | OptionType::FloatOrPercent) => {
                s.trim().trim_end_matches('%').parse::<f64>().map_or_else(|_| Value::Str(s.to_owned()), Value::Num)
            }
            Some(_) => Value::Str(s.to_owned()),
            // Not a known option: a number if it reads as one.
            None => s.trim().parse::<f64>().map_or_else(|_| Value::Str(s.to_owned()), Value::Num),
        }
    };
    // An option the schema does not know, written as a list of numbers ("16,16").
    if def.is_none() && raw.contains(',') && raw.split(',').all(|p| p.trim().parse::<f64>().is_ok()) {
        return Value::Vec(raw.split(',').map(scalar).collect());
    }
    if def.is_some_and(crate::OptionDef::is_vector) {
        Value::Vec(raw.split(',').map(scalar).collect())
    } else {
        scalar(raw)
    }
}

impl Vars for ConfigVars<'_> {
    fn var(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.extra.get(name) {
            return Some(v.clone());
        }
        let def = self.schema.option(name);
        let raw = match self.config.values.get(name) {
            Some(v) => v.as_str(),
            None => def.map(|d| d.default.as_str())?,
        };
        Some(typed(def, raw))
    }
}

// ---- tokens ----

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Regex(String),
    Ident(String),
    Op(&'static str),
}

const OPS: [&str; 24] = [
    "==", "!=", "<>", "<=", ">=", "=~", "!~", "&&", "||", "<", ">", "+", "-", "*", "/", "%", "(", ")", "[", "]", ",",
    "?", ":", "!",
];

fn lex(src: &str) -> Result<Vec<Tok>, MacroError> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            // Exponents: 1e-3.
            if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                let save = i;
                i += 1;
                if i < chars.len() && (chars[i] == '-' || chars[i] == '+') {
                    i += 1;
                }
                if i < chars.len() && chars[i].is_ascii_digit() {
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                } else {
                    i = save;
                }
            }
            let text: String = chars[start..i].iter().collect();
            out.push(Tok::Num(text.parse().map_err(|_| MacroError::Syntax(format!("bad number {text}")))?));
            continue;
        }
        if c == '"' || c == '\'' {
            let mut s = String::new();
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                    s.push(match chars[i] {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                } else {
                    s.push(chars[i]);
                }
                i += 1;
            }
            if i >= chars.len() {
                return Err(MacroError::Syntax("unterminated string".into()));
            }
            i += 1;
            out.push(Tok::Str(s));
            continue;
        }
        // A regex literal follows =~ or !~.
        if c == '/' && matches!(out.last(), Some(Tok::Op("=~" | "!~"))) {
            let mut s = String::new();
            i += 1;
            while i < chars.len() && chars[i] != '/' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    s.push('\\');
                    i += 1;
                }
                s.push(chars[i]);
                i += 1;
            }
            if i >= chars.len() {
                return Err(MacroError::Syntax("unterminated regular expression".into()));
            }
            i += 1;
            out.push(Tok::Regex(s));
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(Tok::Ident(chars[start..i].iter().collect()));
            continue;
        }
        let rest: String = chars[i..chars.len().min(i + 2)].iter().collect();
        match OPS.iter().find(|op| rest.starts_with(**op)) {
            Some(op) => {
                out.push(Tok::Op(op));
                i += op.chars().count();
            }
            None => return Err(MacroError::Syntax(format!("unexpected \"{c}\""))),
        }
    }
    Ok(out)
}

// ---- evaluation (a recursive-descent parser that evaluates as it goes) ----

struct Eval<'a> {
    toks: &'a [Tok],
    at: usize,
    vars: &'a dyn Vars,
    locals: &'a HashMap<String, Value>,
    /// Skip side effects and unknowns in a branch not taken (`a ? b : c`, `and`, `or`).
    lazy: bool,
}

impl Eval<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Op(o)) if *o == op) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn eat_word(&mut self, word: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Ident(w)) if w == word) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect_op(&mut self, op: &str) -> Result<(), MacroError> {
        if self.eat_op(op) { Ok(()) } else { Err(MacroError::Syntax(format!("expected \"{op}\""))) }
    }

    fn expr(&mut self) -> Result<Value, MacroError> {
        let cond = self.or()?;
        if self.eat_op("?") {
            let saved = self.lazy;
            let take = cond.truthy();
            self.lazy = saved || !take;
            let a = self.expr()?;
            self.expect_op(":")?;
            self.lazy = saved || take;
            let b = self.expr()?;
            self.lazy = saved;
            return Ok(if take { a } else { b });
        }
        Ok(cond)
    }

    fn or(&mut self) -> Result<Value, MacroError> {
        let mut v = self.and()?;
        while self.eat_op("||") || self.eat_word("or") {
            let saved = self.lazy;
            self.lazy = saved || v.truthy();
            let r = self.and()?;
            self.lazy = saved;
            v = Value::Bool(v.truthy() || r.truthy());
        }
        Ok(v)
    }

    fn and(&mut self) -> Result<Value, MacroError> {
        let mut v = self.not()?;
        while self.eat_op("&&") || self.eat_word("and") {
            let saved = self.lazy;
            self.lazy = saved || !v.truthy();
            let r = self.not()?;
            self.lazy = saved;
            v = Value::Bool(v.truthy() && r.truthy());
        }
        Ok(v)
    }

    fn not(&mut self) -> Result<Value, MacroError> {
        if self.eat_op("!") || self.eat_word("not") {
            return Ok(Value::Bool(!self.not()?.truthy()));
        }
        self.compare()
    }

    fn compare(&mut self) -> Result<Value, MacroError> {
        let a = self.sum()?;
        for op in ["==", "!=", "<>", "<=", ">=", "<", ">"] {
            if self.eat_op(op) {
                let b = self.sum()?;
                if self.lazy {
                    return Ok(Value::Bool(false));
                }
                return Ok(Value::Bool(compare(&a, &b, op)));
            }
        }
        for op in ["=~", "!~"] {
            if self.eat_op(op) {
                let pattern = match self.peek().cloned() {
                    Some(Tok::Regex(r)) => {
                        self.at += 1;
                        r
                    }
                    _ => self.sum()?.text(),
                };
                if self.lazy {
                    return Ok(Value::Bool(false));
                }
                let re = regex::Regex::new(&format!("(?s)^(?:{pattern})$"))
                    .map_err(|e| MacroError::Syntax(format!("bad regular expression: {e}")))?;
                let hit = re.is_match(&a.text());
                return Ok(Value::Bool(if op == "=~" { hit } else { !hit }));
            }
        }
        Ok(a)
    }

    fn sum(&mut self) -> Result<Value, MacroError> {
        let mut v = self.product()?;
        loop {
            if self.eat_op("+") {
                let r = self.product()?;
                v = if self.lazy {
                    Value::Nil
                } else if matches!(v, Value::Str(_)) || matches!(r, Value::Str(_)) {
                    Value::Str(v.text() + &r.text())
                } else {
                    Value::Num(v.num()? + r.num()?)
                };
            } else if self.eat_op("-") {
                let r = self.product()?;
                v = if self.lazy { Value::Nil } else { Value::Num(v.num()? - r.num()?) };
            } else {
                return Ok(v);
            }
        }
    }

    fn product(&mut self) -> Result<Value, MacroError> {
        let mut v = self.unary()?;
        loop {
            let op = if self.eat_op("*") {
                "*"
            } else if self.eat_op("/") {
                "/"
            } else if self.eat_op("%") {
                "%"
            } else {
                return Ok(v);
            };
            let r = self.unary()?;
            if self.lazy {
                v = Value::Nil;
                continue;
            }
            let (a, b) = (v.num()?, r.num()?);
            v = Value::Num(match op {
                "*" => a * b,
                "/" if b == 0.0 => return Err(MacroError::Type("division by zero".into())),
                "/" => a / b,
                _ if b == 0.0 => return Err(MacroError::Type("modulo by zero".into())),
                _ => a % b,
            });
        }
    }

    fn unary(&mut self) -> Result<Value, MacroError> {
        if self.eat_op("-") {
            let v = self.unary()?;
            return if self.lazy { Ok(Value::Nil) } else { Ok(Value::Num(-v.num()?)) };
        }
        if self.eat_op("+") {
            return self.unary();
        }
        if self.eat_op("!") || self.eat_word("not") {
            return Ok(Value::Bool(!self.unary()?.truthy()));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Value, MacroError> {
        let mut v = self.primary()?;
        while self.eat_op("[") {
            let i = self.expr()?;
            self.expect_op("]")?;
            if self.lazy {
                continue;
            }
            v = index(&v, &i)?;
        }
        Ok(v)
    }

    fn primary(&mut self) -> Result<Value, MacroError> {
        let Some(tok) = self.peek().cloned() else { return Err(MacroError::Syntax("expression ends early".into())) };
        self.at += 1;
        match tok {
            Tok::Num(n) => Ok(Value::Num(n)),
            Tok::Str(s) => Ok(Value::Str(s)),
            Tok::Regex(r) => Ok(Value::Str(r)),
            Tok::Op("(") => {
                // `(a, b)` is a tuple (interpolate_table's points).
                let mut items = vec![self.expr()?];
                while self.eat_op(",") {
                    items.push(self.expr()?);
                }
                self.expect_op(")")?;
                Ok(if items.len() == 1 { items.remove(0) } else { Value::Vec(items) })
            }
            Tok::Op(o) => Err(MacroError::Syntax(format!("unexpected \"{o}\""))),
            Tok::Ident(name) => {
                if self.eat_op("(") {
                    let mut args = Vec::new();
                    if !self.eat_op(")") {
                        loop {
                            args.push(self.expr()?);
                            if self.eat_op(")") {
                                break;
                            }
                            self.expect_op(",")?;
                        }
                    }
                    return if self.lazy { Ok(Value::Nil) } else { call(&name, &args) };
                }
                match name.as_str() {
                    "true" => return Ok(Value::Bool(true)),
                    "false" => return Ok(Value::Bool(false)),
                    "nil" => return Ok(Value::Nil),
                    _ => {}
                }
                if let Some(v) = self.locals.get(&name) {
                    return Ok(v.clone());
                }
                if let Some(v) = self.vars.var(&name) {
                    return Ok(v);
                }
                // Slic3r's legacy `name_0`: element 0 of `name`.
                if let Some((base, i)) = name.rsplit_once('_')
                    && let Ok(i) = i.parse::<usize>()
                    && let Some(v) = self.locals.get(base).cloned().or_else(|| self.vars.var(base))
                {
                    return index(&v, &Value::Num(i as f64));
                }
                if self.lazy { Ok(Value::Nil) } else { Err(MacroError::Unknown(name)) }
            }
        }
    }
}

fn index(v: &Value, i: &Value) -> Result<Value, MacroError> {
    let n = i.num()?;
    if n < 0.0 || !n.is_finite() {
        return Err(MacroError::Type(format!("index {n} is out of range")));
    }
    let k = n as usize;
    match v {
        Value::Vec(items) => items
            .get(k)
            .or_else(|| items.last().filter(|_| items.len() == 1))
            .cloned()
            .ok_or_else(|| MacroError::Type(format!("index {k} is out of range"))),
        // Scalars index as themselves, as OrcaSlicer's do.
        other => Ok(other.clone()),
    }
}

fn compare(a: &Value, b: &Value, op: &str) -> bool {
    let ord = match (a.num(), b.num()) {
        (Ok(x), Ok(y)) if !matches!((a, b), (Value::Str(_), Value::Str(_))) => x.partial_cmp(&y),
        _ => Some(a.text().cmp(&b.text())),
    };
    let Some(ord) = ord else { return op == "!=" || op == "<>" };
    match op {
        "==" => ord.is_eq(),
        "!=" | "<>" => !ord.is_eq(),
        "<" => ord.is_lt(),
        ">" => ord.is_gt(),
        "<=" => ord.is_le(),
        _ => ord.is_ge(),
    }
}

fn call(name: &str, args: &[Value]) -> Result<Value, MacroError> {
    let arg = |i: usize| args.get(i).ok_or_else(|| MacroError::Syntax(format!("{name}() needs more arguments")));
    let num = |i: usize| arg(i).and_then(Value::num);
    Ok(match name {
        "min" => Value::Num(num(0)?.min(num(1)?)),
        "max" => Value::Num(num(0)?.max(num(1)?)),
        "int" => Value::Num(num(0)?.trunc()),
        "round" => Value::Num(num(0)?.round()),
        "abs" => Value::Num(num(0)?.abs()),
        "ceil" => Value::Num(num(0)?.ceil()),
        "floor" => Value::Num(num(0)?.floor()),
        "is_nil" => Value::Bool(matches!(arg(0)?, Value::Nil)),
        "size" => Value::Num(match arg(0)? {
            Value::Vec(v) => v.len() as f64,
            _ => 1.0,
        }),
        "empty" => Value::Bool(!arg(0)?.truthy()),
        "random" => Value::Num(num(0)?),
        "digits" | "zdigits" => {
            let (x, width) = (num(0)?, num(1)?.clamp(0.0, 64.0) as usize);
            let decimals = args.get(2).map(Value::num).transpose()?.unwrap_or(0.0).clamp(0.0, 16.0) as usize;
            Value::Str(if name == "zdigits" {
                format!("{x:0width$.decimals$}")
            } else {
                format!("{x:width$.decimals$}")
            })
        }
        "one_of" => {
            let s = arg(0)?.text();
            Value::Bool(args.iter().skip(1).any(|a| match a {
                Value::Vec(v) => v.iter().any(|x| x.text() == s),
                other => {
                    let p = other.text();
                    p == s || regex::Regex::new(&format!("^(?:{p})$")).is_ok_and(|re| re.is_match(&s))
                }
            }))
        }
        "interpolate_table" => {
            // interpolate_table(x, (x0, y0), (x1, y1), …): args arrive flattened.
            let x = num(0)?;
            let flat: Vec<&Value> = args
                .iter()
                .skip(1)
                .flat_map(|a| match a {
                    Value::Vec(v) => v.iter().collect::<Vec<_>>(),
                    other => vec![other],
                })
                .collect();
            let pts: Vec<f64> = flat.into_iter().map(Value::num).collect::<Result<_, _>>()?;
            let pairs: Vec<(f64, f64)> = pts.chunks(2).filter_map(|c| Some((*c.first()?, *c.get(1)?))).collect();
            let Some(&(fx, fy)) = pairs.first() else {
                return Err(MacroError::Syntax("interpolate_table() needs points".into()));
            };
            if x <= fx {
                return Ok(Value::Num(fy));
            }
            let mut y = pairs.last().map_or(fy, |p| p.1);
            for w in pairs.windows(2) {
                let (a, b) = (w[0], w[1]);
                if x >= a.0 && x <= b.0 {
                    y = if b.0 > a.0 { a.1 + (b.1 - a.1) * (x - a.0) / (b.0 - a.0) } else { a.1 };
                    break;
                }
            }
            Value::Num(y)
        }
        _ => return Err(MacroError::Syntax(format!("unknown function {name}()"))),
    })
}

fn evaluate(src: &str, vars: &dyn Vars, locals: &HashMap<String, Value>) -> Result<Value, MacroError> {
    let toks = lex(src)?;
    let mut e = Eval { toks: &toks, at: 0, vars, locals, lazy: false };
    let v = e.expr()?;
    if e.at != toks.len() {
        return Err(MacroError::Syntax(format!("unexpected text after the expression in \"{src}\"")));
    }
    Ok(v)
}

/// Evaluates a condition (`compatible_printers_condition`). Empty is true.
pub fn condition(src: &str, vars: &dyn Vars) -> Result<bool, MacroError> {
    if src.trim().is_empty() {
        return Ok(true);
    }
    Ok(evaluate(src, vars, &HashMap::new())?.truthy())
}

/// Renders a G-code template. `globals` holds `{global x = …}` values across templates.
pub fn render(template: &str, vars: &dyn Vars, globals: &mut HashMap<String, Value>) -> Result<String, MacroError> {
    let mut out = String::with_capacity(template.len());
    let mut locals: HashMap<String, Value> = globals.clone();
    // Each `if` level: (this branch is printing, a branch of this if has already printed).
    let mut stack: Vec<(bool, bool)> = Vec::new();
    let printing = |stack: &Vec<(bool, bool)>| stack.iter().all(|(on, _)| *on);
    let mut rest = template;
    while let Some(i) = rest.find(['{', '[']) {
        let (before, from) = rest.split_at(i);
        if printing(&stack) {
            out.push_str(before);
        }
        let open = from.as_bytes().first().copied().unwrap_or(b'{');
        if open == b'[' {
            // `[name]` or `[name[i]]` is a variable; any other bracket is plain text.
            let end = from.find(']');
            let inner = end.map(|e| &from[1..e]).unwrap_or("");
            let simple = !inner.is_empty() && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            match (simple, end) {
                (true, Some(e)) => {
                    if printing(&stack) {
                        match evaluate(inner, vars, &locals) {
                            Ok(v) => out.push_str(&v.text()),
                            // An unknown `[name]` is plain text, as in Slic3r (Klipper macro
                            // arguments are often written that way).
                            Err(MacroError::Unknown(_)) => out.push_str(&from[..=e]),
                            Err(err) => return Err(err),
                        }
                    }
                    rest = &from[e + 1..];
                }
                _ => {
                    if printing(&stack) {
                        out.push('[');
                    }
                    rest = &from[1..];
                }
            }
            continue;
        }
        // Find the matching close brace, minding strings and nested braces.
        let Some(end) = matching_brace(from) else { return Err(MacroError::Syntax("a \"{\" is never closed".into())) };
        let inner = from[1..end].trim();
        rest = &from[end + 1..];
        // `if(…)` with no space is still an if.
        let keyword = ["if", "elsif"].into_iter().find(|k| inner.strip_prefix(k).is_some_and(|r| r.starts_with('(')));
        let (word, tail) = match keyword {
            Some(k) => (k, inner[k.len()..].trim()),
            None => inner.split_once(char::is_whitespace).map_or((inner, ""), |(w, t)| (w, t.trim())),
        };
        match word {
            "if" => {
                let on = printing(&stack) && evaluate(tail, vars, &locals).map_err(|e| context(e, inner))?.truthy();
                stack.push((on, on));
            }
            "elsif" => {
                let parent = stack.len().saturating_sub(1);
                let outer = stack.iter().take(parent).all(|(on, _)| *on);
                let Some(top) = stack.last_mut() else { return Err(MacroError::Syntax("elsif without if".into())) };
                let on = outer && !top.1 && evaluate(tail, vars, &locals).map_err(|e| context(e, inner))?.truthy();
                *top = (on, top.1 || on);
            }
            "else" => {
                let Some(top) = stack.last_mut() else { return Err(MacroError::Syntax("else without if".into())) };
                *top = (!top.1, true);
            }
            "endif" => {
                if stack.pop().is_none() {
                    return Err(MacroError::Syntax("endif without if".into()));
                }
            }
            _ => {
                if printing(&stack) {
                    // Several statements may share braces, separated by `;`.
                    for statement in split_statements(inner) {
                        let (w, t) =
                            statement.split_once(char::is_whitespace).map_or((statement, ""), |(w, t)| (w, t.trim()));
                        if w == "local" || w == "global" {
                            let Some((name, value)) = t.split_once('=') else {
                                return Err(MacroError::Syntax(format!("{w} needs \"name = value\"")));
                            };
                            let name = name.trim().to_owned();
                            let v = evaluate(value.trim(), vars, &locals).map_err(|e| context(e, statement))?;
                            if w == "global" {
                                globals.insert(name.clone(), v.clone());
                            }
                            locals.insert(name, v);
                        } else if let Some((target, index_src, value)) = assignment(statement) {
                            // `x = …` or `x[i] = …`: changes a variable (OrcaSlicer's writable
                            // ones such as e_retracted, or an earlier local or global).
                            let v = evaluate(value, vars, &locals).map_err(|e| context(e, statement))?;
                            let new = match index_src {
                                None => v,
                                Some(i) => {
                                    let i = evaluate(i, vars, &locals).map_err(|e| context(e, statement))?.num()?;
                                    let current =
                                        locals.get(target).cloned().or_else(|| vars.var(target)).unwrap_or(Value::Nil);
                                    let mut items = match current {
                                        Value::Vec(items) => items,
                                        Value::Nil => Vec::new(),
                                        other => vec![other],
                                    };
                                    let k = i.max(0.0) as usize;
                                    if k > 1024 {
                                        return Err(context(MacroError::Type("index out of range".into()), statement));
                                    }
                                    if items.len() <= k {
                                        items.resize(k + 1, Value::Num(0.0));
                                    }
                                    if let Some(slot) = items.get_mut(k) {
                                        *slot = v;
                                    }
                                    Value::Vec(items)
                                }
                            };
                            if globals.contains_key(target) {
                                globals.insert(target.to_owned(), new.clone());
                            }
                            locals.insert(target.to_owned(), new);
                        } else if !statement.is_empty() {
                            let v = evaluate(statement, vars, &locals).map_err(|e| context(e, statement))?;
                            out.push_str(&v.text());
                        }
                    }
                }
            }
        }
    }
    if printing(&stack) {
        out.push_str(rest);
    }
    if !stack.is_empty() {
        return Err(MacroError::Syntax("an {if} is never closed with {endif}".into()));
    }
    Ok(out)
}

/// Adds the expression to an error, so a failing template says where.
fn context(e: MacroError, expression: &str) -> MacroError {
    let short: String = expression.chars().take(80).collect();
    match e {
        MacroError::Unknown(_) => e,
        MacroError::Syntax(m) => MacroError::Syntax(format!("{m} in {{{short}}}")),
        MacroError::Type(m) => MacroError::Type(format!("{m} in {{{short}}}")),
    }
}

/// `name = value` or `name[index] = value` (not `==`): (name, index, value).
fn assignment(s: &str) -> Option<(&str, Option<&str>, &str)> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    for (i, &c) in bytes.iter().enumerate() {
        match c {
            b'[' | b'(' => depth += 1,
            b']' | b')' => depth -= 1,
            b'"' => return None,
            b'=' if depth == 0 => {
                let prev = i.checked_sub(1).and_then(|p| bytes.get(p)).copied();
                let next = bytes.get(i + 1).copied();
                if next == Some(b'=') || matches!(prev, Some(b'=' | b'!' | b'<' | b'>')) {
                    return None;
                }
                let lhs = s[..i].trim();
                let value = s[i + 1..].trim();
                let (name, index) = match lhs.split_once('[') {
                    Some((n, rest)) => (n.trim(), Some(rest.strip_suffix(']')?.trim())),
                    None => (lhs, None),
                };
                let ident = !name.is_empty()
                    && name.chars().all(|c| c.is_alphanumeric() || c == '_')
                    && !name.starts_with(|c: char| c.is_ascii_digit());
                return ident.then_some((name, index, value));
            }
            _ => {}
        }
    }
    None
}

/// Splits `a; b; c` at semicolons outside strings.
fn split_statements(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut quote = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '"' => quote = !quote,
            ';' if !quote => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

fn matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"') => quote = Some('"'),
            (None, '{') => depth += 1,
            (None, '}') => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> HashMap<String, Value> {
        let mut v = HashMap::new();
        v.insert("bed_temperature_initial_layer_single".into(), Value::Num(60.0));
        v.insert("nozzle_temperature_initial_layer".into(), Value::Vec(vec![Value::Num(215.0), Value::Num(220.0)]));
        v.insert("initial_extruder".into(), Value::Num(1.0));
        v.insert("filament_type".into(), Value::Vec(vec![Value::Str("PLA".into()), Value::Str("PETG".into())]));
        v.insert("curr_bed_type".into(), Value::Str("Textured PEI Plate".into()));
        v.insert("printer_notes".into(), Value::Str("PRINTER_VENDOR_PRUSA3D\nPRINTER_MODEL_MK4S HF_NOZZLE".into()));
        v.insert("nozzle_diameter".into(), Value::Vec(vec![Value::Num(0.4)]));
        v.insert("spiral_mode".into(), Value::Bool(false));
        v.insert("first_layer_print_min".into(), Value::Vec(vec![Value::Num(10.5), Value::Num(20.0)]));
        v
    }

    #[test]
    fn substitutions() {
        let mut g = HashMap::new();
        let out = render(
            "M190 S[bed_temperature_initial_layer_single]\nM109 S{nozzle_temperature_initial_layer[initial_extruder]}\nG1 X{first_layer_print_min[0] - 5}",
            &vars(),
            &mut g,
        );
        assert_eq!(out.as_deref(), Ok("M190 S60\nM109 S220\nG1 X5.5"));
    }

    #[test]
    fn if_blocks() {
        let mut g = HashMap::new();
        let t = "{if curr_bed_type==\"Textured PEI Plate\"}G29.1 Z-0.04{elsif spiral_mode}X{else}G29.1 Z0{endif}\n\
                 {if filament_type[0]==\"PETG\"}petg{elsif filament_type[1]==\"PETG\"}second{else}none{endif}";
        assert_eq!(render(t, &vars(), &mut g).as_deref(), Ok("G29.1 Z-0.04\nsecond"));
        // Unknown variables in branches not taken are fine; in the taken branch, an error.
        assert!(render("{if false}{nope}{endif}ok", &vars(), &mut g).is_ok());
        assert!(matches!(render("{nope}", &vars(), &mut g), Err(MacroError::Unknown(_))));
        assert!(render("{if true}never closed", &vars(), &mut g).is_err());
    }

    #[test]
    fn locals_globals_and_functions() {
        let mut g = HashMap::new();
        let t =
            "{local x = 3}{global y = x * 2}{min(x, 10)} {max(1.5, x)} {digits(7.5, 5, 2)} {zdigits(7, 3)} {int(7.9)}";
        assert_eq!(render(t, &vars(), &mut g).as_deref(), Ok("3 3  7.50 007 7"));
        assert_eq!(g.get("y"), Some(&Value::Num(6.0)));
        assert_eq!(render("{y + 1}", &vars(), &mut g).as_deref(), Ok("7"));
        assert_eq!(render("{interpolate_table(5, (0,0), (10,100))}", &vars(), &mut g).as_deref(), Ok("50"));
        assert_eq!(render("{1 < 2 ? \"a\" : unknown_var}", &vars(), &mut g).as_deref(), Ok("a"));
    }

    #[test]
    fn prusa_style_conditions() {
        let v = vars();
        assert_eq!(
            condition("printer_notes=~/.*MK4S.*/ and nozzle_diameter[0]==0.4 and printer_notes=~/.*HF_NOZZLE.*/", &v),
            Ok(true)
        );
        assert_eq!(condition("printer_notes=~/.*PRINTER_MODEL_COREONE[^_a-zA-Z0-9].*/", &v), Ok(false));
        assert_eq!(condition("", &v), Ok(true));
        assert_eq!(condition("nozzle_diameter[0]!=0.4 or not spiral_mode", &v), Ok(true));
    }

    #[test]
    fn brackets_that_are_not_variables_stay() {
        let mut g = HashMap::new();
        assert_eq!(render("M117 [hello world] ; [ok", &vars(), &mut g).as_deref(), Ok("M117 [hello world] ; [ok"));
        assert_eq!(
            render("BED_MESH ALGORITHM=[bed_mesh_algo]", &vars(), &mut g).as_deref(),
            Ok("BED_MESH ALGORITHM=[bed_mesh_algo]")
        );
        assert_eq!(
            render("{first_layer_print_min_1} {local a = 2; local b = a * 3; b}{if(a == 2)}x{endif}", &vars(), &mut g)
                .as_deref(),
            Ok("20 6x")
        );
        assert_eq!(
            render(
                "{local v = 1; v = v + 1; v} {first_layer_print_min[1] = 5; first_layer_print_min[1]} {1 == 1}",
                &vars(),
                &mut g
            )
            .as_deref(),
            Ok("2 5 true")
        );
    }
}
