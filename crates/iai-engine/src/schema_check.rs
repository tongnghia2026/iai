//! Ranges read from a command's JSON Schema and checked against the params
//! before serde reads them. The schema an agent is shown and what the engine
//! refuses therefore cannot drift apart: a bound is written once, as a
//! `#[schemars(range(...))]` on the params.
//!
//! Types, required fields and unknown fields are serde's to refuse; this
//! checks what serde does not: `minimum` / `maximum` (and their exclusive
//! forms), string lengths and array lengths, down through `properties`,
//! `items`, `$ref` and `anyOf` / `oneOf` (an `Option` or a tagged enum).

use serde_json::{Map, Value};

use crate::{CmdError, ErrorCode};

/// Nesting deeper than this is not looked into (a schema refers to itself).
const MAX_DEPTH: u32 = 24;

pub fn check(schema: &Value, args: &Value) -> Result<(), CmdError> {
    let defs = schema.get("$defs").and_then(Value::as_object);
    Checker { defs }.at(schema, args, "", 0)
}

struct Checker<'a> {
    defs: Option<&'a Map<String, Value>>,
}

impl<'a> Checker<'a> {
    fn resolve(&self, schema: &'a Value) -> &'a Value {
        let mut schema = schema;
        // A chain of references, followed a few steps at most.
        for _ in 0..8 {
            let Some(target) = schema
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| r.strip_prefix("#/$defs/"))
                .and_then(|name| self.defs.and_then(|d| d.get(name)))
            else {
                break;
            };
            schema = target;
        }
        schema
    }

    fn at(&self, schema: &'a Value, value: &Value, path: &str, depth: u32) -> Result<(), CmdError> {
        if depth > MAX_DEPTH {
            return Ok(());
        }
        let schema = self.resolve(schema);
        let Some(obj) = schema.as_object() else {
            return Ok(());
        };
        let next = depth.saturating_add(1);

        for key in ["anyOf", "oneOf"] {
            if let Some(branches) = obj.get(key).and_then(Value::as_array) {
                let fitting: Vec<&Value> =
                    branches.iter().filter(|b| self.fits(b, value)).collect();
                if fitting.is_empty() {
                    // serde names the problem better.
                    continue;
                }
                let mut first_error = None;
                for branch in fitting {
                    match self.at(branch, value, path, next) {
                        Ok(()) => {
                            first_error = None;
                            break;
                        }
                        Err(e) => {
                            first_error.get_or_insert(e);
                        }
                    }
                }
                if let Some(e) = first_error {
                    return Err(e);
                }
            }
        }
        if let Some(all) = obj.get("allOf").and_then(Value::as_array) {
            for branch in all {
                self.at(branch, value, path, next)?;
            }
        }

        match value {
            Value::Number(n) => {
                if let Some(x) = n.as_f64() {
                    number(obj, x, path)?;
                }
            }
            Value::String(s) => {
                let len = s.chars().count() as f64;
                let min = obj.get("minLength").and_then(Value::as_f64);
                let max = obj.get("maxLength").and_then(Value::as_f64);
                if min.is_some_and(|m| len < m) || max.is_some_and(|m| len > m) {
                    return Err(out_of_range(path, &length_text(min, max, "characters")));
                }
            }
            Value::Array(items) => {
                let len = items.len() as f64;
                let min = obj.get("minItems").and_then(Value::as_f64);
                let max = obj.get("maxItems").and_then(Value::as_f64);
                if min.is_some_and(|m| len < m) || max.is_some_and(|m| len > m) {
                    return Err(out_of_range(path, &length_text(min, max, "items")));
                }
                if let Some(item) = obj.get("items") {
                    for (i, v) in items.iter().enumerate() {
                        self.at(item, v, &format!("{path}[{i}]"), next)?;
                    }
                }
                if let Some(prefix) = obj.get("prefixItems").and_then(Value::as_array) {
                    for (i, (item, v)) in prefix.iter().zip(items).enumerate() {
                        self.at(item, v, &format!("{path}[{i}]"), next)?;
                    }
                }
            }
            Value::Object(fields) => {
                if let Some(props) = obj.get("properties").and_then(Value::as_object) {
                    for (name, v) in fields {
                        if let Some(prop) = props.get(name) {
                            let inner = if path.is_empty() {
                                name.clone()
                            } else {
                                format!("{path}.{name}")
                            };
                            self.at(prop, v, &inner, next)?;
                        }
                    }
                }
            }
            Value::Null | Value::Bool(_) => {}
        }
        Ok(())
    }

    /// Whether `value` could be an instance of `branch`: its type, and for
    /// an object every `const` property it names (the tag of a tagged enum).
    fn fits(&self, branch: &'a Value, value: &Value) -> bool {
        let branch = self.resolve(branch);
        if let Some(c) = branch.get("const") {
            return c == value;
        }
        if let Some(options) = branch.get("enum").and_then(Value::as_array) {
            return options.contains(value);
        }
        let types: Vec<&str> = match branch.get("type") {
            Some(Value::String(t)) => vec![t.as_str()],
            Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !types.is_empty() && !types.iter().any(|t| type_fits(t, value)) {
            return false;
        }
        if let (Some(props), Value::Object(fields)) =
            (branch.get("properties").and_then(Value::as_object), value)
        {
            for (name, prop) in props {
                let prop = self.resolve(prop);
                if let Some(c) = prop.get("const")
                    && fields.get(name) != Some(c)
                {
                    return false;
                }
            }
        }
        true
    }
}

fn type_fits(t: &str, value: &Value) -> bool {
    match (t, value) {
        ("null", Value::Null) | ("boolean", Value::Bool(_)) => true,
        ("string", Value::String(_)) => true,
        ("array", Value::Array(_)) => true,
        ("object", Value::Object(_)) => true,
        ("number", Value::Number(_)) => true,
        ("integer", Value::Number(n)) => n.is_i64() || n.is_u64(),
        _ => false,
    }
}

fn number(obj: &Map<String, Value>, x: f64, path: &str) -> Result<(), CmdError> {
    let get = |k: &str| obj.get(k).and_then(Value::as_f64);
    let (min, max) = (get("minimum"), get("maximum"));
    let (xmin, xmax) = (get("exclusiveMinimum"), get("exclusiveMaximum"));
    let bad = min.is_some_and(|m| x < m)
        || max.is_some_and(|m| x > m)
        || xmin.is_some_and(|m| x <= m)
        || xmax.is_some_and(|m| x >= m);
    if !bad {
        return Ok(());
    }
    let low = min.map(fmt).or(xmin.map(|m| format!("above {}", fmt(m))));
    let high = max.map(fmt).or(xmax.map(|m| format!("below {}", fmt(m))));
    let text = match (low, high) {
        (Some(l), Some(h)) => format!("a number from {l} to {h}"),
        (Some(l), None) => format!("a number of at least {l}"),
        (None, Some(h)) => format!("a number of at most {h}"),
        (None, None) => "a number in range".to_string(),
    };
    Err(out_of_range(path, &text))
}

fn length_text(min: Option<f64>, max: Option<f64>, unit: &str) -> String {
    match (min, max) {
        (Some(l), Some(h)) => format!("{} to {} {unit}", fmt(l), fmt(h)),
        (Some(l), None) => format!("at least {} {unit}", fmt(l)),
        (None, Some(h)) => format!("at most {} {unit}", fmt(h)),
        (None, None) => unit.to_string(),
    }
}

fn fmt(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

fn out_of_range(path: &str, expected: &str) -> CmdError {
    let field = if path.is_empty() { "(params)" } else { path };
    CmdError::new(
        ErrorCode::BadParams,
        format!("`{field}`: expected {expected}"),
    )
    .vi(format!("Giá trị `{field}` không hợp lệ: cần {expected}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[allow(dead_code)]
    struct Inner {
        #[schemars(range(min = 1, max = 6))]
        width_px: u32,
    }

    #[derive(Deserialize, JsonSchema)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    #[allow(dead_code)]
    enum Source {
        Document {
            doc: u32,
        },
        Folder {
            #[schemars(length(min = 1, max = 4))]
            path: String,
        },
    }

    #[derive(Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[allow(dead_code)]
    struct Params {
        #[schemars(range(min = 0.0, max = 64.0))]
        feather_px: f32,
        #[schemars(range(min = 1, max = 500))]
        copies: Option<u32>,
        inner: Option<Inner>,
        #[schemars(length(max = 2))]
        list: Vec<Inner>,
        source: Option<Source>,
    }

    fn schema() -> Value {
        serde_json::to_value(schemars::schema_for!(Params)).unwrap()
    }

    fn checked(args: Value) -> Result<(), CmdError> {
        check(&schema(), &args)
    }

    #[test]
    fn values_in_range_pass() {
        checked(json!({"featherPx": 0, "copies": 500, "list": []})).unwrap();
        checked(json!({"featherPx": 64.0, "copies": null, "inner": {"widthPx": 6}, "list": [{"widthPx": 1}]})).unwrap();
        checked(json!({"featherPx": 3, "list": [], "source": {"kind": "document", "doc": 9}}))
            .unwrap();
    }

    #[test]
    fn a_bound_is_read_from_the_schema_at_any_depth() {
        let e = checked(json!({"featherPx": 64.5, "list": []})).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadParams);
        assert!(
            e.message.contains("`featherPx`") && e.message.contains("0 to 64"),
            "{}",
            e.message
        );

        let e = checked(json!({"featherPx": 1, "copies": 0, "list": []})).unwrap_err();
        assert!(
            e.message.contains("`copies`") && e.message.contains("1 to 500"),
            "{}",
            e.message
        );

        let e = checked(json!({"featherPx": 1, "inner": {"widthPx": 7}, "list": []})).unwrap_err();
        assert!(e.message.contains("`inner.widthPx`"), "{}", e.message);

        let e =
            checked(json!({"featherPx": 1, "list": [{"widthPx": 1}, {"widthPx": 0}]})).unwrap_err();
        assert!(e.message.contains("`list[1].widthPx`"), "{}", e.message);

        let e = checked(
            json!({"featherPx": 1, "list": [{"widthPx": 1}, {"widthPx": 1}, {"widthPx": 1}]}),
        )
        .unwrap_err();
        assert!(
            e.message.contains("`list`") && e.message.contains("items"),
            "{}",
            e.message
        );
    }

    #[test]
    fn the_variant_named_by_its_tag_is_the_one_checked() {
        let e = checked(
            json!({"featherPx": 1, "list": [], "source": {"kind": "folder", "path": "abcdef"}}),
        )
        .unwrap_err();
        assert!(e.message.contains("`source.path`"), "{}", e.message);
        checked(json!({"featherPx": 1, "list": [], "source": {"kind": "folder", "path": "ab"}}))
            .unwrap();
    }

    #[test]
    fn a_wrong_type_is_left_to_serde() {
        checked(json!({"featherPx": "lots", "copies": "many", "list": 3})).unwrap();
    }
}
