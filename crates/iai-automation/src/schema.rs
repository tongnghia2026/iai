//! A command's JSON Schema as MCP clients take it best: every `$ref`
//! written out in place (some clients follow no references), no `$schema`
//! or `$defs`, and `type: object` at the top.

use serde_json::{Map, Value};

/// How deep references are written out; a schema that refers to itself
/// deeper stops there (with an open schema).
const MAX_DEPTH: u32 = 12;

pub fn for_tool(schema: &Value) -> Value {
    let defs = schema
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut out = inline(schema, &defs, 0);
    if let Some(o) = out.as_object_mut() {
        o.remove("$schema");
        o.remove("$defs");
        o.remove("title");
        o.entry("type")
            .or_insert(Value::String("object".to_string()));
        o.entry("properties").or_insert(Value::Object(Map::new()));
    }
    out
}

fn inline(value: &Value, defs: &Map<String, Value>, depth: u32) -> Value {
    match value {
        Value::Object(fields) => {
            if let Some(name) = fields
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| r.strip_prefix("#/$defs/"))
            {
                if depth >= MAX_DEPTH {
                    return Value::Object(Map::new());
                }
                let mut target = defs
                    .get(name)
                    .map(|d| inline(d, defs, depth.saturating_add(1)))
                    .unwrap_or_else(|| Value::Object(Map::new()));
                // Keywords beside the reference (a description) are kept.
                if let Some(t) = target.as_object_mut() {
                    for (k, v) in fields {
                        if k != "$ref" {
                            t.insert(k.clone(), inline(v, defs, depth));
                        }
                    }
                }
                return target;
            }
            Value::Object(
                fields
                    .iter()
                    .filter(|(k, _)| k.as_str() != "$defs")
                    .map(|(k, v)| (k.clone(), inline(v, defs, depth)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| inline(v, defs, depth)).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reference_is_written_out_and_the_top_is_an_object() {
        for c in iai_engine::cmds::core_registry().list() {
            let s = for_tool(&c.params_schema);
            let text = s.to_string();
            assert!(
                !text.contains("$ref") && !text.contains("$defs"),
                "{}: {text}",
                c.id
            );
            assert_eq!(s["type"], "object", "{}", c.id);
        }
        let select = iai_engine::cmds::core_registry()
            .list()
            .into_iter()
            .find(|c| c.id == "select.rect")
            .unwrap();
        let s = for_tool(&select.params_schema);
        assert_eq!(
            s["properties"]["rect"]["properties"]["w"]["maximum"],
            100000
        );
    }
}
