//! A specification's schemas, as the arguments of a tool.
//!
//! An OpenAPI schema is not quite JSON Schema, and a provider takes only part
//! of JSON Schema for a tool's parameters. So each one is translated: a named
//! type in `components/schemas` becomes a shared type of the skill
//! (`{"$ref": "#/types/Pet"}`, kept once in `types.json`), any other
//! reference is followed, `allOf` is merged, `nullable` becomes a type list,
//! and what a request does not send -- a `readOnly` field, an example -- is
//! left out. See `docs/skill-operations.md`.

use serde_json::{Map, Value, json};

use super::parse;

/// How deep a schema is followed before the rest is "an object". A
/// specification can nest without end; a tool's arguments should not.
const DEPTH: usize = 8;

/// What a tool's schema keeps of a schema. Everything else is about
/// responses, documentation or validation a provider does not do.
const KEPT: &[&str] = &[
    "type",
    "description",
    "enum",
    "const",
    "format",
    "default",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "pattern",
    "minItems",
    "maxItems",
];

const COMPONENT: &str = "#/components/schemas/";

/// The named types an operation's arguments referred to, by name.
pub type Types = Map<String, Value>;

/// One schema, translated. Named types it reaches are added to `types`.
pub fn normalize(value: &Value, root: &Value, types: &mut Types, depth: usize) -> Value {
    let Some(obj) = value.as_object() else {
        return json!({});
    };
    if depth > DEPTH {
        return json!({ "type": "object" });
    }

    if let Some(reference) = obj.get("$ref").and_then(Value::as_str) {
        if let Some(name) = reference.strip_prefix(COMPONENT) {
            let name = type_name(name);
            if !types.contains_key(&name) {
                // Claimed before it is filled, so a type that contains itself
                // refers to itself rather than being followed for ever.
                types.insert(name.clone(), json!({ "type": "object" }));
                let target = pointer(root, reference).cloned().unwrap_or(Value::Null);
                let translated = normalize(&target, root, types, depth + 1);
                types.insert(name.clone(), translated);
            }
            return json!({ "$ref": format!("#/types/{name}") });
        }
        return match pointer(root, reference) {
            Some(target) => normalize(&target.clone(), root, types, depth + 1),
            None => json!({}),
        };
    }

    if let Some(parts) = obj.get("allOf").and_then(Value::as_array) {
        return merge(parts, obj, root, types, depth);
    }

    let mut out = Map::new();
    for key in KEPT {
        if let Some(v) = obj.get(*key) {
            out.insert((*key).to_string(), v.clone());
        }
    }
    if obj.get("nullable").and_then(Value::as_bool) == Some(true)
        && let Some(Value::String(t)) = out.get("type").cloned()
    {
        out.insert("type".into(), json!([t, "null"]));
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(options) = obj.get(key).and_then(Value::as_array) {
            out.insert(
                "anyOf".into(),
                Value::Array(
                    options
                        .iter()
                        .map(|o| normalize(o, root, types, depth + 1))
                        .collect(),
                ),
            );
        }
    }
    if let Some(items) = obj.get("items") {
        out.insert("items".into(), normalize(items, root, types, depth + 1));
    }
    if let Some(props) = obj.get("properties").and_then(Value::as_object) {
        let mut kept = Map::new();
        for (name, prop) in props {
            // The server's to set, so not the caller's to send.
            if prop.get("readOnly").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            kept.insert(name.clone(), normalize(prop, root, types, depth + 1));
        }
        let required: Vec<Value> = obj
            .get("required")
            .and_then(Value::as_array)
            .map(|r| {
                r.iter()
                    .filter(|n| n.as_str().is_some_and(|n| kept.contains_key(n)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        out.insert("properties".into(), Value::Object(kept));
        if !required.is_empty() {
            out.insert("required".into(), Value::Array(required));
        }
        out.entry("type").or_insert(json!("object"));
    }
    if let Some(extra) = obj.get("additionalProperties") {
        out.insert(
            "additionalProperties".into(),
            match extra {
                Value::Bool(b) => Value::Bool(*b),
                other => normalize(other, root, types, depth + 1),
            },
        );
    }
    Value::Object(out)
}

/// `allOf` as one object: a base type and what is added to it, which is the
/// use nearly every specification makes of it.
fn merge(
    parts: &[Value],
    obj: &Map<String, Value>,
    root: &Value,
    types: &mut Types,
    depth: usize,
) -> Value {
    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();
    let mut description = obj.get("description").cloned();
    for part in parts {
        let translated = normalize(part, root, types, depth + 1);
        let resolved = resolve_type(&translated, types);
        if let Some(props) = resolved.get("properties").and_then(Value::as_object) {
            properties.extend(props.clone());
        }
        if let Some(r) = resolved.get("required").and_then(Value::as_array) {
            required.extend(r.iter().cloned());
        }
        if description.is_none() {
            description = resolved.get("description").cloned();
        }
    }
    let mut out = Map::new();
    out.insert("type".into(), json!("object"));
    if let Some(d) = description {
        out.insert("description".into(), d);
    }
    out.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        out.insert("required".into(), Value::Array(required));
    }
    Value::Object(out)
}

/// A translated schema, with a reference to a shared type looked through.
pub fn resolve_type(value: &Value, types: &Types) -> Value {
    match value
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix("#/types/"))
    {
        Some(name) => types.get(name).cloned().unwrap_or(Value::Null),
        None => value.clone(),
    }
}

/// An operation's arguments as one object schema -- the path parameters, the
/// query parameters and the JSON body's fields side by side -- and the names
/// that go in the query. `None` where they cannot be one object: a body that
/// is not an object or not JSON, or a name both a parameter and a field.
pub fn arguments(
    op: &parse::Operation,
    root: &Value,
    types: &mut Types,
) -> Option<(Value, Vec<String>)> {
    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();
    let mut query = Vec::new();

    for p in &op.parameters {
        let place = p.location.as_str();
        if place != "path" && place != "query" {
            // Headers are the platform's or left out; see render.rs.
            continue;
        }
        let mut schema = normalize(&p.raw, root, types, 0);
        if !p.description.is_empty()
            && let Some(o) = schema.as_object_mut()
        {
            o.entry("description").or_insert(json!(p.description));
        }
        properties.insert(p.name.clone(), schema);
        if place == "path" || p.required {
            required.push(json!(p.name));
        }
        if place == "query" {
            query.push(p.name.clone());
        }
    }

    if let Some(body) = &op.request_body {
        let raw = body.raw.as_ref().filter(|_| body.is_json())?;
        let translated = normalize(raw, root, types, 0);
        let object = resolve_type(&translated, types);
        let fields = object.get("properties").and_then(Value::as_object)?;
        for (name, field) in fields {
            if properties.contains_key(name) {
                return None;
            }
            properties.insert(name.clone(), field.clone());
        }
        if let Some(r) = object.get("required").and_then(Value::as_array) {
            required.extend(r.iter().cloned());
        }
    }

    let mut schema = Map::new();
    schema.insert("type".into(), json!("object"));
    schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".into(), Value::Array(required));
    }
    Some((Value::Object(schema), query))
}

/// The named types a set of schemas reaches, transitively, so `types.json`
/// carries only what some tool uses.
pub fn reachable(schemas: &[&Value], types: &Types) -> Types {
    let mut found = Types::new();
    let mut pending: Vec<Value> = schemas.iter().map(|s| (*s).clone()).collect();
    while let Some(value) = pending.pop() {
        collect(&value, &mut |name| {
            if !found.contains_key(name)
                && let Some(t) = types.get(name)
            {
                found.insert(name.to_string(), t.clone());
                pending.push(t.clone());
            }
        });
    }
    found
}

fn collect(value: &Value, found: &mut impl FnMut(&str)) {
    match value {
        Value::Object(map) => {
            if let Some(name) = map
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| r.strip_prefix("#/types/"))
            {
                found(name);
            }
            for v in map.values() {
                collect(v, found);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect(v, found)),
        _ => {}
    }
}

/// A component's name as a type name: the last segment, unescaped.
fn type_name(raw: &str) -> String {
    raw.replace("~1", "/").replace("~0", "~")
}

fn pointer<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    root.pointer(reference.strip_prefix('#')?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> Value {
        json!({"components": {"schemas": {
            "Category": {"type": "object", "properties": {
                "id": {"type": "integer", "readOnly": true},
                "name": {"type": "string", "example": "Dogs"}
            }},
            "Pet": {"allOf": [
                {"$ref": "#/components/schemas/Named"},
                {"type": "object", "required": ["status"], "properties": {
                    "status": {"type": "string", "enum": ["available", "sold"], "nullable": true},
                    "category": {"$ref": "#/components/schemas/Category"},
                    "parent": {"$ref": "#/components/schemas/Pet"}
                }}
            ]},
            "Named": {"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}}
        }}})
    }

    #[test]
    fn a_named_type_is_shared_and_its_reference_kept() {
        let mut types = Types::new();
        let out = normalize(
            &json!({"$ref": "#/components/schemas/Category"}),
            &root(),
            &mut types,
            0,
        );
        assert_eq!(out, json!({"$ref": "#/types/Category"}));
        let category = &types["Category"];
        assert!(
            category["properties"].get("id").is_none(),
            "readOnly is the server's"
        );
        assert!(category["properties"]["name"].get("example").is_none());
    }

    #[test]
    fn all_of_merges_and_nullable_becomes_a_type_list() {
        let mut types = Types::new();
        normalize(
            &json!({"$ref": "#/components/schemas/Pet"}),
            &root(),
            &mut types,
            0,
        );
        let pet = &types["Pet"];
        assert_eq!(pet["properties"]["name"]["type"], "string", "from the base");
        assert_eq!(pet["required"], json!(["name", "status"]));
        assert_eq!(
            pet["properties"]["status"]["type"],
            json!(["string", "null"])
        );
        assert_eq!(
            pet["properties"]["parent"],
            json!({"$ref": "#/types/Pet"}),
            "itself, by name"
        );
    }
}
