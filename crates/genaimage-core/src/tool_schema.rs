use std::collections::BTreeSet;

use serde_json::{Map, Number, Value};

const SUPPORTED_KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "enum",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "items",
    "minItems",
    "maxItems",
];

pub(crate) fn validate_schema(schema: &Value) -> Result<(), String> {
    validate_schema_at(schema, "$")
}

pub(crate) fn validate_instance(schema: &Value, arguments: &Value) -> Result<(), String> {
    validate_schema(schema).map_err(|error| format!("Tool JSON schema is invalid: {error}"))?;
    validate_instance_at(schema, arguments, "$")
}

fn validate_schema_at(schema: &Value, path: &str) -> Result<(), String> {
    let object = schema
        .as_object()
        .ok_or_else(|| format!("{path}: schema must be an object"))?;
    for key in object.keys() {
        if !SUPPORTED_KEYWORDS.contains(&key.as_str()) {
            return Err(format!("{path}: unsupported keyword '{key}'"));
        }
    }

    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{path}: 'type' must be a supported string"))?;
    if !matches!(
        kind,
        "object" | "string" | "integer" | "number" | "boolean" | "array"
    ) {
        return Err(format!("{path}: unsupported type '{kind}'"));
    }
    allowed_for_type(object, kind, path)?;

    if let Some(properties) = object.get("properties") {
        let properties = properties
            .as_object()
            .ok_or_else(|| format!("{path}.properties: must be an object"))?;
        for (name, property) in properties {
            validate_schema_at(property, &format!("{path}.properties.{name}"))?;
        }
    }
    if let Some(required) = object.get("required") {
        let required = required
            .as_array()
            .ok_or_else(|| format!("{path}.required: must be an array"))?;
        let mut names = BTreeSet::new();
        for value in required {
            let name = value
                .as_str()
                .ok_or_else(|| format!("{path}.required: entries must be strings"))?;
            if !names.insert(name) {
                return Err(format!("{path}.required: duplicate property '{name}'"));
            }
        }
    }
    if object.contains_key("additionalProperties")
        && object.get("additionalProperties") != Some(&Value::Bool(false))
    {
        return Err(format!(
            "{path}.additionalProperties: only false is supported"
        ));
    }
    if let Some(values) = object.get("enum") {
        if !values.as_array().is_some_and(|values| !values.is_empty()) {
            return Err(format!("{path}.enum: must be a non-empty array"));
        }
    }
    validate_number_keyword(object, "minimum", path)?;
    validate_number_keyword(object, "maximum", path)?;
    validate_size_keyword(object, "minLength", path)?;
    validate_size_keyword(object, "maxLength", path)?;
    validate_size_keyword(object, "minItems", path)?;
    validate_size_keyword(object, "maxItems", path)?;
    if let Some(items) = object.get("items") {
        validate_schema_at(items, &format!("{path}.items"))?;
    }
    if matches!(kind, "string") {
        check_order(object, "minLength", "maxLength", path, |value| {
            value.as_u64().expect("size keyword validated")
        })?;
    }
    if matches!(kind, "array") {
        check_order(object, "minItems", "maxItems", path, |value| {
            value.as_u64().expect("size keyword validated")
        })?;
    }
    if matches!(kind, "integer" | "number") {
        if let (Some(minimum), Some(maximum)) = (object.get("minimum"), object.get("maximum")) {
            if compare_numbers(
                minimum.as_number().expect("minimum validated"),
                maximum.as_number().expect("maximum validated"),
            )
            .is_gt()
            {
                return Err(format!("{path}: minimum must not be greater than maximum"));
            }
        }
    }
    Ok(())
}

fn allowed_for_type(object: &Map<String, Value>, kind: &str, path: &str) -> Result<(), String> {
    for key in object.keys() {
        let valid = match key.as_str() {
            "properties" | "required" | "additionalProperties" => kind == "object",
            "minimum" | "maximum" => matches!(kind, "integer" | "number"),
            "minLength" | "maxLength" => kind == "string",
            "items" | "minItems" | "maxItems" => kind == "array",
            _ => true,
        };
        if !valid {
            return Err(format!(
                "{path}: keyword '{key}' is invalid for type '{kind}'"
            ));
        }
    }
    Ok(())
}

fn validate_number_keyword(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<(), String> {
    if object.get(key).is_some_and(|value| !value.is_number()) {
        return Err(format!("{path}.{key}: must be a number"));
    }
    Ok(())
}

fn validate_size_keyword(object: &Map<String, Value>, key: &str, path: &str) -> Result<(), String> {
    if object
        .get(key)
        .is_some_and(|value| value.as_u64().is_none())
    {
        return Err(format!("{path}.{key}: must be a non-negative integer"));
    }
    Ok(())
}

fn check_order(
    object: &Map<String, Value>,
    minimum: &str,
    maximum: &str,
    path: &str,
    extract: impl Fn(&Value) -> u64,
) -> Result<(), String> {
    if let (Some(minimum_value), Some(maximum_value)) = (object.get(minimum), object.get(maximum)) {
        if extract(minimum_value) > extract(maximum_value) {
            return Err(format!(
                "{path}: {minimum} must not be greater than {maximum}"
            ));
        }
    }
    Ok(())
}

fn validate_instance_at(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let schema = schema.as_object().expect("schema was validated");
    let kind = schema["type"].as_str().expect("type was validated");
    match kind {
        "object" => validate_object(schema, value, path)?,
        "string" => validate_string(schema, value, path)?,
        "integer" => validate_number(schema, value, path, true)?,
        "number" => validate_number(schema, value, path, false)?,
        "boolean" if value.is_boolean() => {}
        "boolean" => return Err(format!("{path}: expected boolean")),
        "array" => validate_array(schema, value, path)?,
        _ => unreachable!("schema type was validated"),
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            return Err(format!(
                "{path}: value is not one of the allowed enum values"
            ));
        }
    }
    Ok(())
}

fn validate_object(schema: &Map<String, Value>, value: &Value, path: &str) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{path}: expected object"))?;
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for name in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !object.contains_key(name) {
            return Err(format!("{path}: missing required property '{name}'"));
        }
    }
    for (name, item) in object {
        let Some(property) = properties.get(name) else {
            if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                return Err(format!(
                    "{path}: additional property '{name}' is not allowed"
                ));
            }
            continue;
        };
        validate_instance_at(property, item, &format!("{path}.{name}"))?;
    }
    Ok(())
}

fn validate_string(schema: &Map<String, Value>, value: &Value, path: &str) -> Result<(), String> {
    let string = value
        .as_str()
        .ok_or_else(|| format!("{path}: expected string"))?;
    let length = string.chars().count() as u64;
    if let Some(minimum) = schema.get("minLength").and_then(Value::as_u64) {
        if length < minimum {
            return Err(format!("{path}: string length is below {minimum}"));
        }
    }
    if let Some(maximum) = schema.get("maxLength").and_then(Value::as_u64) {
        if length > maximum {
            return Err(format!("{path}: string length exceeds {maximum}"));
        }
    }
    Ok(())
}

fn validate_number(
    schema: &Map<String, Value>,
    value: &Value,
    path: &str,
    integer: bool,
) -> Result<(), String> {
    let number = value.as_number().ok_or_else(|| {
        format!(
            "{path}: expected {}",
            if integer { "integer" } else { "number" }
        )
    })?;
    if integer && !is_integer(number) {
        return Err(format!("{path}: expected integer"));
    }
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_number) {
        if compare_numbers(number, minimum).is_lt() {
            return Err(format!("{path}: number is below minimum {minimum}"));
        }
    }
    if let Some(maximum) = schema.get("maximum").and_then(Value::as_number) {
        if compare_numbers(number, maximum).is_gt() {
            return Err(format!("{path}: number exceeds maximum {maximum}"));
        }
    }
    Ok(())
}

fn validate_array(schema: &Map<String, Value>, value: &Value, path: &str) -> Result<(), String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("{path}: expected array"))?;
    let length = items.len() as u64;
    if let Some(minimum) = schema.get("minItems").and_then(Value::as_u64) {
        if length < minimum {
            return Err(format!("{path}: array has fewer than {minimum} items"));
        }
    }
    if let Some(maximum) = schema.get("maxItems").and_then(Value::as_u64) {
        if length > maximum {
            return Err(format!("{path}: array has more than {maximum} items"));
        }
    }
    if let Some(item_schema) = schema.get("items") {
        for (index, item) in items.iter().enumerate() {
            validate_instance_at(item_schema, item, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

fn is_integer(number: &Number) -> bool {
    number.is_i64()
        || number.is_u64()
        || number
            .as_f64()
            .is_some_and(|number| number.is_finite() && number.fract() == 0.0)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Ordering {
    Less,
    Equal,
    Greater,
}

impl Ordering {
    const fn is_lt(self) -> bool {
        matches!(self, Self::Less)
    }

    const fn is_gt(self) -> bool {
        matches!(self, Self::Greater)
    }
}

fn compare_numbers(left: &Number, right: &Number) -> Ordering {
    if let (Some(left), Some(right)) = (left.as_i64(), right.as_i64()) {
        return compare(left, right);
    }
    if let (Some(left), Some(right)) = (left.as_u64(), right.as_u64()) {
        return compare(left, right);
    }
    let left = left
        .as_f64()
        .expect("JSON numbers fit f64 for this comparison");
    let right = right
        .as_f64()
        .expect("JSON numbers fit f64 for this comparison");
    if left < right {
        Ordering::Less
    } else if left > right {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

fn compare<T: Ord>(left: T, right: T) -> Ordering {
    match left.cmp(&right) {
        std::cmp::Ordering::Less => Ordering::Less,
        std::cmp::Ordering::Equal => Ordering::Equal,
        std::cmp::Ordering::Greater => Ordering::Greater,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::{json, Value};

    use super::{validate_instance, validate_schema};
    use crate::{AssistantTool, ExtensionRegistry, HarnessError, Result, ToolContext};

    #[test]
    fn supports_all_runtime_keywords() {
        let schema = json!({
            "type": "object",
            "properties": {
                "label": {"type": "string", "minLength": 2, "maxLength": 4, "enum": ["blue", "red"]},
                "count": {"type": "integer", "minimum": 1, "maximum": 3},
                "ratio": {"type": "number", "minimum": 0.5, "maximum": 2.0},
                "enabled": {"type": "boolean"},
                "steps": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": 1,
                    "maxItems": 2
                }
            },
            "required": ["label", "count", "ratio", "enabled", "steps"],
            "additionalProperties": false
        });
        validate_schema(&schema).unwrap();
        validate_instance(
            &schema,
            &json!({"label":"red","count":2,"ratio":1.5,"enabled":true,"steps":["a"]}),
        )
        .unwrap();
    }

    #[test]
    fn validates_each_supported_keyword() {
        let invalid = [
            (json!({"type":"string"}), json!(1), "expected string"),
            (json!({"type":"integer"}), json!(1.5), "expected integer"),
            (json!({"type":"number","minimum":1}), json!(0.5), "minimum"),
            (json!({"type":"number","maximum":2}), json!(3), "maximum"),
            (json!({"type":"boolean"}), json!("yes"), "expected boolean"),
            (
                json!({"type":"object","properties":{"x":{"type":"string"}}}),
                json!({"x":1}),
                "expected string",
            ),
            (
                json!({"type":"object","required":["x"]}),
                json!({}),
                "required property",
            ),
            (
                json!({"type":"object","additionalProperties":false}),
                json!({"x":1}),
                "additional property",
            ),
            (json!({"type":"string","enum":["x"]}), json!("y"), "enum"),
            (json!({"type":"string","minLength":2}), json!("x"), "below"),
            (
                json!({"type":"string","maxLength":2}),
                json!("abc"),
                "exceeds",
            ),
            (
                json!({"type":"array","items":{"type":"integer"}}),
                json!([1, "x"]),
                "expected integer",
            ),
            (json!({"type":"array","minItems":2}), json!([1]), "fewer"),
            (json!({"type":"array","maxItems":1}), json!([1, 2]), "more"),
        ];
        for (schema, value, message) in invalid {
            let error = validate_instance(&schema, &value).unwrap_err();
            assert!(error.contains(message), "{error}");
        }
    }

    #[test]
    fn rejects_unsupported_and_malformed_schemas() {
        for (schema, message) in [
            (
                json!({"type":"string","pattern":"x"}),
                "unsupported keyword 'pattern'",
            ),
            (
                json!({"type":"array","uniqueItems":true}),
                "unsupported keyword 'uniqueItems'",
            ),
            (json!({"type":"null"}), "unsupported type 'null'"),
            (
                json!({"type":"object","additionalProperties":true}),
                "only false",
            ),
            (
                json!({"type":"string","minimum":1}),
                "invalid for type 'string'",
            ),
            (json!({"type":"integer","minimum":5,"maximum":1}), "minimum"),
        ] {
            let error = validate_schema(&schema).unwrap_err();
            assert!(error.contains(message), "{error}");
        }
    }

    struct UnsupportedTool;

    #[async_trait]
    impl AssistantTool for UnsupportedTool {
        fn name(&self) -> &str {
            "unsupported"
        }
        fn description(&self) -> &str {
            "unsupported schema"
        }
        fn json_schema(&self) -> Value {
            json!({"type":"string","format":"uri"})
        }
        fn requires_confirmation(&self) -> bool {
            false
        }
        async fn invoke(&self, _context: ToolContext, _arguments: Value) -> Result<Value> {
            Ok(json!(null))
        }
    }

    #[test]
    fn registration_rejects_an_unsupported_keyword() {
        let mut registry = ExtensionRegistry::default();
        let error = registry
            .register_assistant_tool(Arc::new(UnsupportedTool))
            .unwrap_err();
        assert!(
            matches!(error, HarnessError::Configuration(ref reason) if reason.contains("unsupported keyword 'format'"))
        );
    }
}
