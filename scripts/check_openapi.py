"""Check OpenAPI structure and coverage of the server's registered routes."""

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
METHODS = {"get", "post", "put", "delete", "patch", "head", "options", "trace"}
ROUTE = re.compile(
    r'\.route\(\s*"([^"]+)"\s*,\s*'
    r'((?:get|post|put|delete|patch|head|options)\([^()]+\)'
    r'(?:\.(?:get|post|put|delete|patch|head|options)\([^()]+\))*)\s*\)',
    re.MULTILINE,
)


def registered_routes(route_dir):
    routes = set()
    files = sorted(route_dir.glob("*.rs"))
    modules = {file.stem for file in files if file.name != "mod.rs"}
    merged_modules = set()
    for file in files:
        text = file.read_text(encoding="utf-8")
        matches = list(ROUTE.finditer(text))
        merges = re.findall(r'\.merge\s*\(\s*(\w+)::routes\(\)\s*\)', text)
        if len(merges) != len(re.findall(r'\.merge\s*\(', text)) or (merges and file.name != "mod.rs"):
            raise ValueError(f"Unsupported router merge in {file}")
        if len(merges) != len(set(merges)):
            raise ValueError(f"Duplicate router merge in {file}")
        merged_modules.update(merges)
        # Fail closed when routing syntax changes rather than silently missing an endpoint.
        if len(matches) != len(re.findall(r'\.route\s*\(', text)) or re.search(
            r'\.(?:route_service|nest|nest_service)\s*\(', text
        ):
            raise ValueError(f"Unsupported route registration in {file}")
        for match in matches:
            path, handlers = match.groups()
            for method in re.findall(r'\b(get|post|put|delete|patch|head|options)\s*\(', handlers):
                route = ("/api/v1" + path, method)
                if route in routes:
                    raise ValueError(f"Duplicate registered route {route}")
                routes.add(route)
                if method == "get":
                    routes.add((route[0], "head"))
    if merged_modules != modules:
        raise ValueError("Route modules and registered router merges differ")
    return routes


def validate_spec(spec, routes):
    errors = []
    if spec.get("openapi") != "3.1.0":
        errors.append("expected OpenAPI 3.1.0")
    if not isinstance(spec.get("info"), dict) or not all(spec["info"].get(k) for k in ("title", "version")):
        errors.append("missing info title or version")
    paths = spec.get("paths", {})
    if not isinstance(paths, dict) or not paths:
        return errors + ["missing paths"]
    operations = set()
    ids = set()
    for path, item in paths.items():
        if not path.startswith("/") or not isinstance(item, dict):
            errors.append(f"invalid path item: {path}")
            continue
        for method, operation in item.items():
            if method not in METHODS:
                continue
            operations.add((path, method))
            if not isinstance(operation, dict):
                errors.append(f"invalid operation: {method} {path}")
                continue
            operation_id = operation.get("operationId")
            if not operation_id or operation_id in ids:
                errors.append(f"missing or duplicate operationId: {method} {path}")
            ids.add(operation_id)
            if not isinstance(operation.get("responses"), dict) or not operation["responses"]:
                errors.append(f"missing responses: {method} {path}")
            params = operation.get("parameters", []) + item.get("parameters", [])
            path_params = {p.get("name") for p in params if p.get("in") == "path" and p.get("required") is True}
            if path_params != set(re.findall(r'\{([^}]+)\}', path)):
                errors.append(f"invalid path parameters: {method} {path}")
    for route in sorted(routes - operations):
        errors.append(f"registered route missing from spec: {route}")
    for route in sorted(operations - routes):
        errors.append(f"spec operation not registered: {route}")

    def check_refs(value):
        if isinstance(value, dict):
            ref = value.get("$ref")
            if ref:
                node = spec
                try:
                    if not ref.startswith("#/"):
                        raise KeyError(ref)
                    for part in ref[2:].split("/"):
                        node = node[part.replace("~1", "/").replace("~0", "~")]
                except (KeyError, TypeError):
                    errors.append(f"unresolved reference: {ref}")
            for child in value.values():
                check_refs(child)
        elif isinstance(value, list):
            for child in value:
                check_refs(child)

    check_refs(spec)
    return errors


def check():
    try:
        spec = json.loads((ROOT / "docs/api/openapi.json").read_text(encoding="utf-8"))
        server_lib = (ROOT / "crates/genaimage-server/src/lib.rs").read_text(encoding="utf-8")
        nests = re.findall(r'\.nest\s*\(\s*"/api/v1"\s*,\s*routes::api\(\)\s*\)', server_lib)
        if len(nests) != 1 or len(re.findall(r'\.(?:route|route_service|nest|nest_service|merge)\s*\(', server_lib)) != 1:
            return ["Unsupported top-level server route registration"]
        routes = registered_routes(ROOT / "crates/genaimage-server/src/routes")
        return validate_spec(spec, routes)
    except (OSError, ValueError) as exc:
        return [str(exc)]
