use serde_json::Value;
use std::collections::HashSet;

/// Expand local `$ref` entries in a JSON Schema without infinite recursion.
pub fn deref_json_schema(schema: &Value) -> Value {
    let full_schema = schema.clone();

    fn resolve_pointer(root: &Value, pointer: &str) -> Option<Value> {
        let parts = pointer.trim_start_matches("#/").split('/');
        let mut current = root;
        for part in parts {
            match current {
                Value::Object(map) => current = map.get(part)?,
                _ => return None,
            }
        }
        Some(current.clone())
    }

    fn traverse(node: &mut Value, root: &Value) {
        if let Value::Object(map) = node {
            if let Some(Value::String(ref_path)) = map.get("$ref")
                && ref_path.starts_with('#')
                && let Some(mut target) = resolve_pointer(root, ref_path)
            {
                traverse(&mut target, root);
                if let Value::Object(ref mut t_map) = target {
                    map.remove("$ref");
                    for (k, v) in t_map.iter() {
                        if !map.contains_key(k) {
                            map.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            for v in map.values_mut() {
                traverse(v, root);
            }
        } else if let Value::Array(arr) = node {
            for v in arr.iter_mut() {
                traverse(v, root);
            }
        }
    }

    let mut resolved = full_schema.clone();
    traverse(&mut resolved, &full_schema);
    if let Value::Object(ref mut map) = resolved {
        map.remove("$defs");
        map.remove("definitions");
    }
    resolved
}

const COMBINATOR_KEYS: &[&str] = &[
    "anyOf", "oneOf", "allOf", "not", "if", "then", "else", "$ref",
];

/// Return a deep copy of `schema` with an explicit `type` on every property.
pub fn ensure_property_types(schema: &Value) -> Value {
    let mut result = schema.clone();
    recurse_schema(&mut result);
    result
}

fn recurse_schema(node: &mut Value) {
    if let Value::Object(map) = node {
        if let Some(Value::Object(props)) = map.get_mut("properties") {
            for value in props.values_mut() {
                normalize_property(value);
            }
        }
        if let Some(Value::Object(items)) = map.get_mut("items") {
            normalize_property(&mut Value::Object(std::mem::take(items)));
        }
        if let Some(Value::Array(items)) = map.get_mut("items") {
            for value in items.iter_mut() {
                normalize_property(value);
            }
        }
        if let Some(Value::Object(additional)) = map.get_mut("additionalProperties") {
            normalize_property(&mut Value::Object(std::mem::take(additional)));
        }
        for key in ["anyOf", "oneOf", "allOf"] {
            if let Some(Value::Array(branches)) = map.get_mut(key) {
                for value in branches.iter_mut() {
                    normalize_property(value);
                }
            }
        }
    }
}

fn normalize_property(node: &mut Value) {
    if let Value::Object(map) = node {
        if !map.contains_key("type") && !COMBINATOR_KEYS.iter().any(|k| map.contains_key(*k)) {
            if let Some(Value::Array(values)) = map.get("enum") {
                if !values.is_empty() {
                    map.insert(
                        "type".to_string(),
                        Value::String(infer_type_from_values(values)),
                    );
                }
            } else if map.contains_key("const") {
                if let Some(v) = map.get("const") {
                    map.insert(
                        "type".to_string(),
                        Value::String(infer_type_from_values(std::slice::from_ref(v))),
                    );
                }
            } else {
                map.insert(
                    "type".to_string(),
                    Value::String(infer_type_from_structure(map)),
                );
            }
        }
        recurse_schema(node);
    }
}

const OBJECT_KEYWORDS: &[&str] = &[
    "properties",
    "additionalProperties",
    "patternProperties",
    "propertyNames",
    "required",
    "minProperties",
    "maxProperties",
];
const ARRAY_KEYWORDS: &[&str] = &[
    "items",
    "prefixItems",
    "minItems",
    "maxItems",
    "uniqueItems",
    "contains",
];
const STRING_KEYWORDS: &[&str] = &["minLength", "maxLength", "pattern", "format"];
const NUMERIC_KEYWORDS: &[&str] = &[
    "minimum",
    "maximum",
    "multipleOf",
    "exclusiveMinimum",
    "exclusiveMaximum",
];

fn infer_type_from_structure(node: &serde_json::Map<String, Value>) -> String {
    if OBJECT_KEYWORDS.iter().any(|k| node.contains_key(*k)) {
        return "object".to_string();
    }
    if ARRAY_KEYWORDS.iter().any(|k| node.contains_key(*k)) {
        return "array".to_string();
    }
    if STRING_KEYWORDS.iter().any(|k| node.contains_key(*k)) {
        return "string".to_string();
    }
    if NUMERIC_KEYWORDS.iter().any(|k| node.contains_key(*k)) {
        return "number".to_string();
    }
    "string".to_string()
}

fn infer_type_from_values(values: &[Value]) -> String {
    let mut inferred = HashSet::new();
    for value in values {
        match value {
            Value::Bool(_) => {
                inferred.insert("boolean");
            }
            Value::Number(n) => {
                if n.is_i64() || n.is_u64() {
                    inferred.insert("integer");
                } else {
                    inferred.insert("number");
                }
            }
            Value::String(_) => {
                inferred.insert("string");
            }
            Value::Null => {
                inferred.insert("null");
            }
            Value::Object(_) => {
                inferred.insert("object");
            }
            Value::Array(_) => {
                inferred.insert("array");
            }
        }
    }
    if inferred.len() == 1 {
        return inferred.into_iter().next().unwrap().to_string();
    }
    let numeric: HashSet<_> = ["integer", "number"].iter().cloned().collect();
    if inferred == numeric {
        return "number".to_string();
    }
    "string".to_string()
}
