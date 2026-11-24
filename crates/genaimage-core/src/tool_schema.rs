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
    validate_supported_numbers(schema, "$")?;
    validate_schema_at(schema, "$")
}

pub(crate) fn validate_instance(schema: &Value, arguments: &Value) -> Result<(), String> {
    validate_schema(schema).map_err(|error| format!("Tool JSON schema is invalid: {error}"))?;
    validate_supported_numbers(arguments, "$")?;
    validate_instance_at(schema, arguments, "$")
}

fn validate_supported_numbers(value: &Value, path: &str) -> Result<(), String> {
    match value {
        Value::Number(number) if Decimal::try_parse(number).is_none() => Err(format!(
            "{path}: unsupported arbitrary-precision number '{number}'"
        )),
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                validate_supported_numbers(value, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        Value::Object(values) => {
            for (key, value) in values {
                validate_supported_numbers(value, &format!("{path}.{key}"))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
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
        if !allowed
            .iter()
            .any(|candidate| json_values_equal(candidate, value))
        {
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
    Decimal::parse(number).is_integer()
}

fn json_values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            compare_numbers(left, right) == Ordering::Equal
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| json_values_equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, left)| {
                    right
                        .get(key)
                        .is_some_and(|right| json_values_equal(left, right))
                })
        }
        _ => left == right,
    }
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

    const fn reverse(self) -> Self {
        match self {
            Self::Less => Self::Greater,
            Self::Equal => Self::Equal,
            Self::Greater => Self::Less,
        }
    }
}

fn compare_numbers(left: &Number, right: &Number) -> Ordering {
    Decimal::parse(left).compare(&Decimal::parse(right))
}

#[derive(Debug)]
struct Decimal {
    negative: bool,
    digits: String,
    exponent: i64,
    zero: bool,
}

impl Decimal {
    fn parse(number: &Number) -> Self {
        Self::try_parse(number).expect("numeric representation was checked")
    }

    fn try_parse(number: &Number) -> Option<Self> {
        let source = number.to_string();
        let (negative, unsigned) = source
            .strip_prefix('-')
            .map(|value| (true, value))
            .unwrap_or((false, source.as_str()));
        let (mantissa, exponent) =
            if let Some((mantissa, exponent)) = unsigned.split_once(['e', 'E']) {
                (mantissa, exponent.parse::<i64>().ok()?)
            } else {
                (unsigned, 0_i64)
            };
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let mut digits = format!("{whole}{fraction}");
        let mut exponent = exponent.checked_sub(i64::try_from(fraction.len()).ok()?)?;
        let leading = digits.bytes().take_while(|byte| *byte == b'0').count();
        digits.drain(..leading);
        let zero = digits.is_empty();
        if zero {
            digits.push('0');
            return Some(Self {
                negative: false,
                digits,
                exponent: 0,
                zero,
            });
        }
        let trailing = digits
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'0')
            .count();
        if trailing != 0 {
            digits.truncate(digits.len() - trailing);
            exponent = exponent.checked_add(i64::try_from(trailing).ok()?)?;
        }
        exponent.checked_add(i64::try_from(digits.len()).ok()?)?;
        Some(Self {
            negative,
            digits,
            exponent,
            zero,
        })
    }

    fn is_integer(&self) -> bool {
        self.zero || self.exponent >= 0
    }

    fn compare(&self, other: &Self) -> Ordering {
        match (self.zero, other.zero) {
            (true, true) => return Ordering::Equal,
            (true, false) => {
                return if other.negative {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, true) => {
                return if self.negative {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, false) => {}
        }
        if self.negative != other.negative {
            return if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let magnitude = self.compare_magnitude(other);
        if self.negative {
            magnitude.reverse()
        } else {
            magnitude
        }
    }

    fn compare_magnitude(&self, other: &Self) -> Ordering {
        let left_position = self
            .exponent
            .saturating_add(i64::try_from(self.digits.len()).expect("JSON number length fits i64"));
        let right_position = other.exponent.saturating_add(
            i64::try_from(other.digits.len()).expect("JSON number length fits i64"),
        );
        if left_position != right_position {
            return if left_position < right_position {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let width = self.digits.len().max(other.digits.len());
        for index in 0..width {
            let left = self.digits.as_bytes().get(index).copied().unwrap_or(b'0');
            let right = other.digits.as_bytes().get(index).copied().unwrap_or(b'0');
            if left != right {
                return if left < right {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
        }
        Ordering::Equal
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
    fn compares_numeric_limits_without_loss_of_precision() {
        let precise_max = json!({"type":"integer","maximum":9007199254740992.0});
        validate_instance(&precise_max, &json!(9007199254740992_u64)).unwrap();
        let error = validate_instance(&precise_max, &json!(9007199254740993_u64)).unwrap_err();
        assert!(error.contains("maximum"), "{error}");

        for (schema, valid, invalid) in [
            (
                json!({"type":"integer","minimum":i64::MIN}),
                json!(i64::MIN),
                serde_json::from_str::<Value>("-9223372036854775809").unwrap(),
            ),
            (
                json!({"type":"integer","maximum":i64::MAX}),
                json!(i64::MAX),
                json!(u64::MAX),
            ),
            (
                json!({"type":"integer","maximum":u64::MAX}),
                json!(u64::MAX),
                json!(serde_json::Number::from_f64(1e30).unwrap()),
            ),
            (json!({"type":"number","minimum":-0.0}), json!(0), json!(-1)),
            (json!({"type":"integer","minimum":1.5}), json!(2), json!(1)),
            (json!({"type":"integer","maximum":1.5}), json!(1), json!(2)),
        ] {
            validate_instance(&schema, &valid).unwrap();
            assert!(validate_instance(&schema, &invalid).is_err());
        }
    }

    #[test]
    fn enum_uses_recursive_json_schema_numeric_equality() {
        let schema = json!({
            "type":"array",
            "enum":[[1, {"nested":[1, 2.0]}]]
        });
        validate_instance(&schema, &json!([1.0, {"nested":[1.0, 2]}])).unwrap();
        assert!(validate_instance(&schema, &json!([1, {"nested":[1, 3]}])).is_err());
    }

    #[test]
    fn rejects_unrepresentable_arbitrary_precision_numbers() {
        let schema = json!({"type":"number"});
        let number = serde_json::from_str::<Value>(
            "1e999999999999999999999999999999999999999999999999999999999999",
        )
        .unwrap();
        let error = validate_instance(&schema, &number).unwrap_err();
        assert!(
            error.contains("unsupported arbitrary-precision number"),
            "{error}"
        );
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
