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
        ("OpenAPI structure and route coverage", check_openapi.check),
        ("Portal lockfile", lambda: check_portal_lockfile(portal) if portal_present else []),
        ("Prerequisites", lambda: check_runtime_tooling(portal_present)),
    ]
    for label, check in checks:
        if not run_check(label, check):
            print(f"CI check failed: {label}", file=sys.stderr)
            return 1

    commands = [
        ("Rust formatting", ["cargo", "fmt", "--all", "--", "--check"], ROOT),
        (
            "Rust linting",
            ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
            ROOT,
        ),
        (
            "Rust tests",
            ["cargo", "test", "--workspace", "--all-targets", "--locked"],
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
    ]
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
        print("==> Portal checks skipped (apps/portal/package.json not found)", flush=True)

    for label, command, cwd in commands:
        if not run_command(label, command, cwd):
            print(f"CI command failed: {label}", file=sys.stderr)
            return 1

    print("CI checks passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
