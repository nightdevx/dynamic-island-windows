//! Just enough JSON to edit Claude Code's settings.json without disturbing it: keys keep
//! their order, strings and numbers keep their exact text, and the output is indented like
//! `JSON.stringify(value, null, 2)`, the way Claude Code writes the file. (Windows.Data.Json
//! reorders keys and writes everything on one line.)

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A string (with its quotes and escapes), number, true, false or null, as written.
    Raw(String),
    Array(Vec<Value>),
    /// Keys as written between the quotes.
    Object(Vec<(String, Value)>),
}

impl Value {
    /// A JSON string holding `text`.
    pub fn string(text: &str) -> Value {
        let mut out = String::from('"');
        for c in text.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        Value::Raw(out)
    }

    pub fn object(fields: Vec<(&str, Value)>) -> Value {
        Value::Object(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        match self {
            Value::Object(fields) => fields.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Replaces the value in place, or adds the key at the end.
    pub fn set(&mut self, key: &str, value: Value) {
        if let Value::Object(fields) = self {
            match fields.iter_mut().find(|(k, _)| k == key) {
                Some((_, slot)) => *slot = value,
                None => fields.push((key.to_string(), value)),
            }
        }
    }

    pub fn remove(&mut self, key: &str) {
        if let Value::Object(fields) = self {
            fields.retain(|(k, _)| k != key);
        }
    }

    /// The text of a Raw value as written (a string keeps its quotes and escapes).
    pub fn raw(&self) -> Option<&str> {
        match self {
            Value::Raw(text) => Some(text),
            _ => None,
        }
    }
}

/// None when `text` is not one complete JSON value.
pub fn parse(text: &str) -> Option<Value> {
    let mut parser = Parser { bytes: text.trim_start_matches('\u{feff}').as_bytes(), at: 0 };
    let value = parser.value()?;
    parser.space();
    (parser.at == parser.bytes.len()).then_some(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.bytes.get(self.at).is_some_and(|b| b.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        self.space();
        let found = self.bytes.get(self.at) == Some(&byte);
        if found {
            self.at += 1;
        }
        found
    }

    /// A string with its quotes, as written.
    fn string(&mut self) -> Option<String> {
        self.space();
        let start = self.at;
        if self.bytes.get(self.at) != Some(&b'"') {
            return None;
        }
        self.at += 1;
        loop {
            match *self.bytes.get(self.at)? {
                b'\\' => self.at += 2,
                b'"' => {
                    self.at += 1;
                    return String::from_utf8(self.bytes[start..self.at].to_vec()).ok();
                }
                _ => self.at += 1,
            }
        }
    }

    fn value(&mut self) -> Option<Value> {
        self.space();
        match *self.bytes.get(self.at)? {
            b'{' => {
                self.at += 1;
                let mut fields = Vec::new();
                if self.eat(b'}') {
                    return Some(Value::Object(fields));
                }
                loop {
                    let key = self.string()?;
                    if !self.eat(b':') {
                        return None;
                    }
                    fields.push((key[1..key.len() - 1].to_string(), self.value()?));
                    if self.eat(b'}') {
                        return Some(Value::Object(fields));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                if self.eat(b']') {
                    return Some(Value::Array(items));
                }
                loop {
                    items.push(self.value()?);
                    if self.eat(b']') {
                        return Some(Value::Array(items));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'"' => self.string().map(Value::Raw),
            _ => {
                let start = self.at;
                while self.bytes.get(self.at).is_some_and(|b| b.is_ascii_alphanumeric() || b"+-.".contains(b)) {
                    self.at += 1;
                }
                let word = std::str::from_utf8(&self.bytes[start..self.at]).ok()?;
                let valid = matches!(word, "true" | "false" | "null") || word.parse::<f64>().is_ok();
                valid.then(|| Value::Raw(word.to_string()))
            }
        }
    }
}

/// Indented by two spaces per level, like `JSON.stringify(value, null, 2)`.
pub fn print(value: &Value) -> String {
    let mut out = String::new();
    write(value, 0, &mut out);
    out
}

fn write(value: &Value, depth: usize, out: &mut String) {
    let indent = |out: &mut String, depth: usize| out.push_str(&"  ".repeat(depth));
    match value {
        Value::Raw(text) => out.push_str(text),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Object(fields) if fields.is_empty() => out.push_str("{}"),
        Value::Array(items) => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                indent(out, depth + 1);
                write(item, depth + 1, out);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            indent(out, depth);
            out.push(']');
        }
        Value::Object(fields) => {
            out.push_str("{\n");
            for (i, (key, item)) in fields.iter().enumerate() {
                indent(out, depth + 1);
                out.push_str(&format!("\"{key}\": "));
                write(item, depth + 1, out);
                out.push_str(if i + 1 < fields.len() { ",\n" } else { "\n" });
            }
            indent(out, depth);
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_as_json_stringify_writes() {
        let text = "{\n  \"env\": {\n    \"A\": \"x \\\"q\\\" \\\\ ü\"\n  },\n  \"n\": -1.5e3,\n  \"list\": [\n    true,\n    null,\n    []\n  ],\n  \"empty\": {}\n}";
        let value = parse(text).unwrap();
        assert_eq!(print(&value), text);
        assert_eq!(value.get("n").and_then(Value::raw), Some("-1.5e3"));
    }

    #[test]
    fn rejects_broken_json() {
        for bad in ["", "{", "{\"a\":}", "[1,]", "{\"a\":1} x", "tru", "{\"a\" 1}"] {
            assert!(parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn edits_keep_order() {
        let mut value = parse(r#"{"b":1,"a":2}"#).unwrap();
        value.set("b", Value::string("C:\\x\"y"));
        value.set("c", Value::Raw("3".into()));
        value.remove("a");
        assert_eq!(print(&value), "{\n  \"b\": \"C:\\\\x\\\"y\",\n  \"c\": 3\n}");
    }
}
