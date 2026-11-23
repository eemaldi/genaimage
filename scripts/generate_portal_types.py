#!/usr/bin/env python3
"""Generate or check portal types using the portal's pinned openapi-typescript."""
import argparse
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if generated types differ")
    args = parser.parse_args()
    portal = ROOT / "apps/portal"
    if not (portal / "package.json").is_file():
        parser.error("apps/portal/package.json does not exist")
    output = portal / "src/lib/api/schema.d.ts"
    if not args.check:
        output.parent.mkdir(parents=True, exist_ok=True)
    command = ["npx", "--no-install", "openapi-typescript", str(ROOT / "docs/api/openapi.json"), "-o", str(output)]
    if args.check:
        command.append("--check")
    return subprocess.run(command, cwd=portal, check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())
