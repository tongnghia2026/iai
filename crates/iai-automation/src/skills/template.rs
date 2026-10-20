//! Filling a recipe's templates: `${name}` / `${f.stem}` from the params and
//! the loops in scope, and `{"$ref": "s3", "path": "/doc"}` from what an
//! earlier step answered.
//!
//! A string that is exactly one `${…}` takes the value's own type (a number
//! stays a number); inside a longer string a value is written out as text.
//! `$${` writes a literal `${`.

use std::collections::BTreeMap;

use serde_json::Value;

/// The names a template can see, innermost last.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    vars: Vec<(String, Value)>,
}

impl Scope {
    pub fn with(mut self, name: &str, value: Value) -> Self {
        self.vars.push((name.to_string(), value));
        self
    }

    pub fn push(&mut self, name: &str, value: Value) {
        self.vars.push((name.to_string(), value));
    }

    /// `f.stem` → the value of `f`'s field `stem`; array fields by index.
    pub fn lookup(&self, expr: &str) -> Option<Value> {
        let mut parts = expr.trim().split('.');
        let head = parts.next()?;
        let mut value = self
            .vars
            .iter()
            .rev()
            .find(|(n, _)| n == head)
            .map(|(_, v)| v.clone())?;
        for part in parts {
            value = match &value {
                Value::Object(o) => o.get(part).cloned()?,
                Value::Array(a) => a.get(part.parse::<usize>().ok()?).cloned()?,
                _ => return None,
            };
        }
        Some(value)
    }
}

/// What the steps answered so far: `"<namespace>#<step id>"` → result.
pub type Results = BTreeMap<String, Value>;

/// The key a step's result is kept under.
pub fn result_key(ns: &str, id: &str) -> String {
    format!("{ns}#{id}")
}

/// `template` with every `${…}` and `$ref` filled in.
pub fn fill(template: &Value, scope: &Scope, results: &Results, ns: &str) -> Result<Value, String> {
    match template {
        Value::String(s) => fill_str(s, scope),
        Value::Array(items) => items
            .iter()
            .map(|v| fill(v, scope, results, ns))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(o) => {
            if let Some(step) = o.get("$ref").and_then(Value::as_str) {
                let path = o.get("path").and_then(Value::as_str).unwrap_or("");
                let result = results
                    .get(&result_key(ns, step))
                    .ok_or_else(|| format!("step `{step}` has not answered yet"))?;
                return result
                    .pointer(path)
                    .cloned()
                    .ok_or_else(|| format!("step `{step}` answered nothing at `{path}`"));
            }
            let mut out = serde_json::Map::new();
            for (k, v) in o {
                out.insert(k.clone(), fill(v, scope, results, ns)?);
            }
            Ok(Value::Object(out))
        }
        other => Ok(other.clone()),
    }
}

/// The parts of `s`: literal text and `${…}` expressions.
fn pieces(s: &str) -> Result<Vec<(bool, String)>, String> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut rest = s;
    while let Some(at) = rest.find('$') {
        let (before, from) = rest.split_at(at);
        text.push_str(before);
        if let Some(after) = from.strip_prefix("$${") {
            text.push_str("${");
            rest = after;
        } else if let Some(after) = from.strip_prefix("${") {
            let end = after
                .find('}')
                .ok_or_else(|| format!("`{s}`: a `${{` is not closed"))?;
            let (expr, tail) = after.split_at(end);
            if !text.is_empty() {
                out.push((false, std::mem::take(&mut text)));
            }
            out.push((true, expr.to_string()));
            rest = tail.get(1..).unwrap_or("");
        } else {
            text.push('$');
            rest = from.get(1..).unwrap_or("");
        }
    }
    text.push_str(rest);
    if !text.is_empty() {
        out.push((false, text));
    }
    Ok(out)
}

fn fill_str(s: &str, scope: &Scope) -> Result<Value, String> {
    if !s.contains('$') {
        return Ok(Value::String(s.to_string()));
    }
    let parts = pieces(s)?;
    let value_of = |expr: &str| {
        scope
            .lookup(expr)
            .ok_or_else(|| format!("`${{{expr}}}` names nothing here"))
    };
    if let [(true, expr)] = parts.as_slice() {
        return value_of(expr);
    }
    let mut out = String::new();
    for (is_expr, text) in parts {
        if is_expr {
            match value_of(&text)? {
                Value::String(v) => out.push_str(&v),
                Value::Null => {}
                v => out.push_str(&v.to_string()),
            }
        } else {
            out.push_str(&text);
        }
    }
    Ok(Value::String(out))
}

/// Whether `v` holds any `${…}` or `$ref`.
pub fn is_template(v: &Value) -> bool {
    match v {
        Value::String(s) => s.contains("${"),
        Value::Array(a) => a.iter().any(is_template),
        Value::Object(o) => o.contains_key("$ref") || o.values().any(is_template),
        _ => false,
    }
}

/// The `${…}` names `v` uses (heads only: `f` of `f.stem`).
pub fn names_used(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => {
            if let Ok(parts) = pieces(s) {
                for (is_expr, text) in parts {
                    let head = text.split('.').next().unwrap_or("").trim().to_string();
                    if is_expr && !out.contains(&head) {
                        out.push(head);
                    }
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| names_used(x, out)),
        Value::Object(o) => o.values().for_each(|x| names_used(x, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_fill_strings_and_keep_their_type_when_alone() {
        let scope = Scope::default()
            .with("ra", json!(r"D:\out"))
            .with("f", json!({"stem": "an", "name": "an.jpg"}))
            .with("n", json!(3));
        let r = Results::new();
        assert_eq!(
            fill(&json!("${ra}/${f.stem}.png"), &scope, &r, "x").unwrap(),
            json!(r"D:\out/an.png")
        );
        assert_eq!(fill(&json!("${n}"), &scope, &r, "x").unwrap(), json!(3));
        assert_eq!(
            fill(&json!("to_in_${n}.pdf"), &scope, &r, "x").unwrap(),
            json!("to_in_3.pdf")
        );
        assert_eq!(
            fill(&json!("$${n} $5"), &scope, &r, "x").unwrap(),
            json!("${n} $5")
        );
        assert!(fill(&json!("${nope}"), &scope, &r, "x").is_err());
        assert!(fill(&json!("${ra"), &scope, &r, "x").is_err());
        // An inner loop's name hides an outer one.
        let inner = scope.clone().with("n", json!(7));
        assert_eq!(fill(&json!("${n}"), &inner, &r, "x").unwrap(), json!(7));
    }

    #[test]
    fn a_ref_reads_an_earlier_answer_of_the_same_skill() {
        let mut r = Results::new();
        r.insert(
            result_key("k", "s2"),
            json!({"doc": 5, "sheets": [{"doc": 9}]}),
        );
        let s = Scope::default();
        let t = json!({"doc": {"$ref": "s2", "path": "/doc"}, "keep": [1, {"$ref": "s2", "path": "/sheets/0/doc"}]});
        assert_eq!(
            fill(&t, &s, &r, "k").unwrap(),
            json!({"doc": 5, "keep": [1, 9]})
        );
        // Another skill's step of the same id is not this one's.
        assert!(fill(&t, &s, &r, "other").is_err());
        assert!(fill(&json!({"$ref": "s2", "path": "/none"}), &s, &r, "k").is_err());
        let mut used = Vec::new();
        names_used(&json!({"a": "${f.stem}-${n}", "b": ["${ra}"]}), &mut used);
        assert_eq!(used, ["f", "n", "ra"]);
    }
}
