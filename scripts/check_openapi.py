"""Perform lightweight structural checks on the generated OpenAPI 3.1 document."""

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
METHODS = {"get", "post", "put", "delete", "patch", "head", "options", "trace"}
SCHEMA_TYPES = {"null", "boolean", "object", "array", "number", "integer", "string"}


def validate_spec(spec):
    errors = []
    if spec.get("openapi") != "3.1.0":
        errors.append("expected OpenAPI 3.1.0")
    if not isinstance(spec.get("info"), dict) or not all(spec["info"].get(k) for k in ("title", "version")):
        errors.append("missing info title or version")
    paths = spec.get("paths", {})
    if not isinstance(paths, dict) or not paths:
        return errors + ["missing paths"]
    ids = set()
    for path, item in paths.items():
        if not path.startswith("/") or not isinstance(item, dict):
            errors.append(f"invalid path item: {path}")
            continue
        for method, operation in item.items():
            if method not in METHODS:
                continue
            if not isinstance(operation, dict):
                errors.append(f"invalid operation: {method} {path}")
                continue
            operation_id = operation.get("operationId")
            if not operation_id or operation_id in ids:
                errors.append(f"missing or duplicate operationId: {method} {path}")
            ids.add(operation_id)
            responses = operation.get("responses")
            if not isinstance(responses, dict) or not responses:
                errors.append(f"missing responses: {method} {path}")
            else:
                for status, response in responses.items():
                    if not isinstance(response, dict) or ("$ref" not in response and not isinstance(response.get("description"), str)):
                        errors.append(f"response missing description: {method} {path} {status}")
            params = operation.get("parameters", []) + item.get("parameters", [])
            path_params = {p.get("name") for p in params if p.get("in") == "path" and p.get("required") is True}
            if path_params != set(re.findall(r'\{([^}]+)\}', path)):
                errors.append(f"invalid path parameters: {method} {path}")

    def check_schema(schema):
        if isinstance(schema, bool):
            return
        if not isinstance(schema, dict):
            errors.append("schema must be an object or boolean")
            return
        if "type" in schema:
            types = schema["type"] if isinstance(schema["type"], list) else [schema["type"]]
            if not types or any(not isinstance(kind, str) or kind not in SCHEMA_TYPES for kind in types):
                errors.append(f"invalid schema type: {schema['type']!r}")
        for key in ("properties", "patternProperties", "$defs", "dependentSchemas"):
            for child in schema.get(key, {}).values():
                check_schema(child)
        for key in ("items", "additionalProperties", "unevaluatedProperties", "propertyNames", "not", "if", "then", "else", "contains"):
            if key in schema:
                check_schema(schema[key])
        for key in ("allOf", "anyOf", "oneOf", "prefixItems"):
            for child in schema.get(key, []):
                check_schema(child)

    def visit(value):
        if isinstance(value, dict):
            ref = value.get("$ref")
            if ref:
                node = spec
                try:
                    if not isinstance(ref, str) or not ref.startswith("#/"):
                        raise KeyError(ref)
                    for part in ref[2:].split("/"):
                        node = node[part.replace("~1", "/").replace("~0", "~")]
                except (KeyError, TypeError):
                    errors.append(f"unresolved reference: {ref}")
            if "schema" in value:
                check_schema(value["schema"])
            for child in value.values():
                visit(child)
        elif isinstance(value, list):
            for child in value:
                visit(child)

    for schema in spec.get("components", {}).get("schemas", {}).values():
        check_schema(schema)
    visit(spec)
    return errors


def check():
    try:
        spec = json.loads((ROOT / "docs/api/openapi.json").read_text(encoding="utf-8"))
        return validate_spec(spec)
    except (OSError, ValueError, TypeError, AttributeError) as exc:
        return [str(exc)]
