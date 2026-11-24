#!/usr/bin/env python3

import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote

if __package__:
    from . import check_openapi
else:
    import check_openapi


ROOT = Path(__file__).resolve().parent.parent
MARKDOWN_LINK = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")


def markdown_files():
    return sorted(
        path
        for path in ROOT.rglob("*.md")
        if ".git" not in path.parts and "node_modules" not in path.parts
    )


def check_relative_links():
    errors = []

    for markdown in markdown_files():
        text = markdown.read_text(encoding="utf-8")
        for match in MARKDOWN_LINK.finditer(text):
            target = match.group(1).strip()
            if target.startswith("<") and target.endswith(">"):
                target = target[1:-1].strip()
            if not target or target.startswith(("#", "http://", "https://", "mailto:", "tel:", "data:")):
                continue

            target = target.split(maxsplit=1)[0].strip("\"'")
            target_path = unquote(target.split("#", 1)[0])
            if not target_path:
                continue

            resolved = (markdown.parent / target_path).resolve()
            if not resolved.exists():
                relative_markdown = markdown.relative_to(ROOT)
                errors.append(f"{relative_markdown}: missing relative link target '{target}'")

    return errors


def check_skill_mirrors():
    errors = []
    copilot_root = ROOT / ".agents" / "skills"
    claude_root = ROOT / ".claude" / "skills"

    if not copilot_root.is_dir() or not claude_root.is_dir():
        return ["project skill directories are missing"]

    def files(root):
        return {
            path.relative_to(root).as_posix(): path.read_bytes()
            for path in root.rglob("*")
            if path.is_file()
            and "__pycache__" not in path.parts
            and path.suffix not in {".pyc", ".pyo"}
        }

    copilot_files = files(copilot_root)
    claude_files = files(claude_root)

    copilot_names = set(copilot_files)
    claude_names = set(claude_files)
    for name in sorted(copilot_names - claude_names):
        errors.append(f".claude/skills is missing mirrored file '{name}'")
    for name in sorted(claude_names - copilot_names):
        errors.append(f".agents/skills is missing mirrored file '{name}'")
    for name in sorted(copilot_names & claude_names):
        if copilot_files[name] != claude_files[name]:
            errors.append(f"mirrored skill file differs: '{name}'")

    for skill_file in sorted(copilot_root.glob("*/SKILL.md")):
        text = skill_file.read_text(encoding="utf-8")
        if not text.startswith("---\n"):
            errors.append(f"{skill_file.relative_to(ROOT)}: missing YAML frontmatter")
            continue

        frontmatter_end = text.find("\n---\n", 4)
        if frontmatter_end == -1:
            errors.append(f"{skill_file.relative_to(ROOT)}: unterminated YAML frontmatter")
            continue

        frontmatter = text[4:frontmatter_end]
        name = re.search(r"^name:\s*(.+?)\s*$", frontmatter, re.MULTILINE)
        description = re.search(r"^description:\s*(.+?)\s*$", frontmatter, re.MULTILINE)
        if not name:
            errors.append(f"{skill_file.relative_to(ROOT)}: missing frontmatter 'name'")
        elif name.group(1) != skill_file.parent.name:
            errors.append(
                f"{skill_file.relative_to(ROOT)}: frontmatter name '{name.group(1)}' "
                f"does not match directory '{skill_file.parent.name}'"
            )
        if not description:
            errors.append(f"{skill_file.relative_to(ROOT)}: missing frontmatter 'description'")

    return errors


def check_skill_lock():
    lock_file = ROOT / "skills-lock.json"
    if not lock_file.is_file():
        return []

    try:
        lock = json.loads(lock_file.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return [f"skills-lock.json: invalid JSON: {exc}"]

    if not isinstance(lock.get("skills"), dict):
        return ["skills-lock.json: 'skills' must be an object"]
    return []


def rust_tokens(source):
    """Tokenise enough Rust to check paths and visibility outside comments and literals."""
    tokens = []
    position = 0
    line = 1
    while position < len(source):
        start = position
        char = source[position]
        if source.startswith("//", position):
            end = source.find("\n", position)
            position = len(source) if end < 0 else end
        elif source.startswith("/*", position):
            position += 2
            depth = 1
            while depth and position < len(source):
                if source.startswith("/*", position):
                    depth += 1
                    position += 2
                elif source.startswith("*/", position):
                    depth -= 1
                    position += 2
                else:
                    position += 1
            if depth:
                raise ValueError(f"line {line}: unterminated block comment")
        else:
            raw = re.match(r'(?:br|r)(#*)"', source[position:])
            quoted = re.match(r'(?:b)?"', source[position:])
            character = re.match(r"(?:b)?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'", source[position:])
            if raw:
                delimiter = '"' + raw.group(1)
                end = source.find(delimiter, position + raw.end())
                if end < 0:
                    raise ValueError(f"line {line}: unterminated raw string")
                position = end + len(delimiter)
            elif quoted:
                position += quoted.end()
                while position < len(source) and source[position] != '"':
                    if source[position] == "\\":
                        position += 1
                    position += 1
                if position >= len(source):
                    raise ValueError(f"line {line}: unterminated string")
                position += 1
            elif character:
                position += character.end()
            elif char.isspace():
                position += 1
            else:
                identifier = re.match(r"(?:r#)?[a-zA-Z_][a-zA-Z_0-9]*", source[position:])
                if identifier:
                    text = identifier.group()
                    if text.startswith("r#"):
                        text = text[2:]
                    position += identifier.end()
                elif source.startswith(("::", "->"), position):
                    text = source[position:position + 2]
                    position += 2
                else:
                    text = char
                    position += 1
                tokens.append((text, line))
        line += source.count("\n", start, position)
    return tokens


def check_cli_client_boundary():
    source = ROOT / "crates" / "genaimage" / "src"
    local_modules = {
        "serve": "pub ( crate ) async fn execute ( cli : & Cli , portal_dev_url : Option < & str > ) -> ExitCode {".split(),
        "config_local": "pub ( crate ) fn execute ( cli : & Cli , force : bool ) -> ExitCode {".split(),
    }
    forbidden = {"genaimage_core", "genaimage_comfyui", "genaimage_server"}
    errors = []
    for path in sorted(source.rglob("*.rs")):
        relative = path.relative_to(ROOT)
        local = path.parent == source and path.stem in local_modules
        main = path == source / "main.rs"
        try:
            tokens = rust_tokens(path.read_text(encoding="utf-8"))
        except ValueError as error:
            errors.append(f"{relative}: {error}")
            continue
        words = [token for token, _ in tokens]
        public_dispatches = 0
        in_use = False
        attribute_depth = 0
        for index, (word, line) in enumerate(tokens):
            if word == "[" and (attribute_depth or
                                words[max(0, index - 1):index] == ["#"] or
                                words[max(0, index - 2):index] == ["#", "!"]):
                attribute_depth += 1
            elif word == "]" and attribute_depth:
                attribute_depth -= 1
            if word == "use":
                in_use = True
            elif word == ";":
                in_use = False
            if not local and word in forbidden:
                errors.append(f"{relative}:{line}: client module references a local-only crate")
            # Reject all file-inclusion macros, including aliases and computed paths.
            if (not local and word in {"include", "include_str", "include_bytes"}
                    and (in_use or words[index + 1:index + 2] == ["!"])):
                errors.append(f"{relative}:{line}: client modules must not use include macros")
            if (not local and attribute_depth and word == "path"
                    and words[index + 1:index + 2] == ["="]):
                errors.append(f"{relative}:{line}: client modules must not use path attributes")
            local_reference = (word in local_modules and (
                in_use or (index > 0 and words[index - 1] in {"mod", "::"})
                or words[index + 1:index + 2] == ["::"]
            ))
            server_function = local and words[max(0, index - 2):index + 2] == [
                "genaimage_server", "::", "serve", "("
            ]
            if local_reference and not server_function:
                declaration = (main and index > 0 and words[index - 1] == "mod"
                               and words[index + 1:index + 2] == [";"]
                               and (index < 2 or words[index - 2] not in {"pub", ")"}))
                call = (main and (index == 0 or words[index - 1] != "::")
                        and words[index + 1:index + 4] == ["::", "execute", "("])
                if not declaration and not call:
                    errors.append(f"{relative}:{line}: only main may call local dispatch functions")
            if local and word == "macro_export":
                errors.append(f"{relative}:{line}: local modules must not export macros")
            if local and word == "pub":
                signature = local_modules[path.stem]
                if words[index:index + len(signature)] == signature:
                    public_dispatches += 1
                else:
                    errors.append(f"{relative}:{line}: local modules may expose only their dispatch function")
        if local and public_dispatches != 1:
            errors.append(f"{relative}: expected exactly one narrow public dispatch function")
    return errors


def run_command(label, command, cwd=ROOT):
    print(f"==> {label}", flush=True)
    environment = os.environ.copy()
    cargo_bin = str(Path.home() / ".cargo" / "bin")
    if cargo_bin not in environment.get("PATH", "").split(os.pathsep):
        environment["PATH"] = cargo_bin + os.pathsep + environment.get("PATH", "")
    try:
        completed = subprocess.run(
            command,
            cwd=cwd,
            env=environment,
            check=False,
        )
    except OSError as exc:
        print(f"Failed to start {label}: {exc}", file=sys.stderr, flush=True)
        return False
    return completed.returncode == 0


def check_runtime_tooling(portal_present=False):
    errors = []
    commands = ["cargo", "rustfmt"]
    if portal_present:
        commands.extend(["node", "npm"])
    for command in commands:
        locations = [
            shutil.which(command),
            str(Path.home() / ".cargo" / "bin" / command)
            if command in {"cargo", "rustfmt"}
            else None,
        ]
        if not any(location and Path(location).is_file() for location in locations):
            suffix = (
                " (required when apps/portal/package.json exists)"
                if command in {"node", "npm"}
                else ""
            )
            errors.append(f"required command is not installed: {command}{suffix}")
    return errors


def check_portal_lockfile(portal):
    if not any((portal / name).is_file() for name in ("package-lock.json", "npm-shrinkwrap.json")):
        return [
            "apps/portal requires package-lock.json or npm-shrinkwrap.json for npm ci"
        ]
    return []


def run_check(label, check):
    print(f"==> {label}", flush=True)
    errors = check()
    if errors:
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return False
    return True


def main():
    portal = ROOT / "apps" / "portal"
    portal_present = (portal / "package.json").is_file()
    checks = [
        ("Relative links", check_relative_links),
        ("Skill mirrors", check_skill_mirrors),
        ("Skill lock", check_skill_lock),
        ("OpenAPI structure", check_openapi.check),
        ("CLI client boundary", check_cli_client_boundary),
        ("Portal lockfile", lambda: check_portal_lockfile(portal) if portal_present else []),
        ("Prerequisites", lambda: check_runtime_tooling(portal_present)),
    ]
    for label, check in checks:
        if not run_check(label, check):
            print(f"CI check failed: {label}", file=sys.stderr)
            return 1

    commands = []
    if portal_present:
        commands.extend(
            [
                ("Portal dependencies", ["npm", "ci"], portal),
                ("Portal API types", [sys.executable, "scripts/generate_portal_types.py", "--check"], ROOT),
                ("Portal linting", ["npm", "run", "lint"], portal),
                ("Portal tests", ["npm", "test"], portal),
                ("Portal build", ["npm", "run", "build"], portal),
            ]
        )
    else:
        print("==> Portal checks skipped; Rust checks use --no-default-features", flush=True)

    # The standard checkout exercises the embedded portal, including in debug tests.
    feature_flags = [] if portal_present else ["--no-default-features"]
    commands.extend([
        ("Rust formatting", ["cargo", "fmt", "--all", "--", "--check"], ROOT),
        (
            "Rust linting",
            ["cargo", "clippy", "--workspace", "--all-targets", "--locked", *feature_flags, "--", "-D", "warnings"],
            ROOT,
        ),
        (
            "Rust tests",
            ["cargo", "test", "--workspace", "--all-targets", "--locked", *feature_flags],
            ROOT,
        ),
        (
            "CI runner tests",
            [sys.executable, "-m", "unittest", "discover", "-s", "scripts/tests"],
            ROOT,
        ),
        (
            "QA helper tests",
            [
                sys.executable,
                "-m",
                "unittest",
                "discover",
                "-s",
                ".agents/skills/generated-media-qa/tests",
            ],
            ROOT,
        ),
        (
            "Manifest helper tests",
            [
                sys.executable,
                "-m",
                "unittest",
                "discover",
                "-s",
                ".agents/skills/media-qc-delivery/tests",
            ],
            ROOT,
        ),
    ])
    for label, command, cwd in commands:
        if not run_command(label, command, cwd):
            print(f"CI command failed: {label}", file=sys.stderr)
            return 1

    print("CI checks passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
