import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
COMPOSE_FILES = sorted(ROOT.glob("compose*.yaml"))
PORT_ENTRY = re.compile(r'^\s+- "?([^"\s]+)"?\s*$')
EXTERNAL_IMAGE = re.compile(r"^\s+image:\s*(\S+)")


def port_entries(text):
    in_ports = False
    for line in text.splitlines():
        if re.match(r"^\s+ports:\s*$", line):
            in_ports = True
        elif in_ports and (match := PORT_ENTRY.match(line)):
            yield match.group(1)
        else:
            in_ports = False


class ComposeTests(unittest.TestCase):
    def test_compose_files_exist(self):
        self.assertIn(ROOT / "compose.yaml", COMPOSE_FILES)

    def test_every_published_port_binds_to_loopback(self):
        published = 0
        for path in COMPOSE_FILES:
            for entry in port_entries(path.read_text(encoding="utf-8")):
                published += 1
                self.assertTrue(entry.startswith("127.0.0.1:"), f"{path.name}: {entry}")
        self.assertGreaterEqual(published, 3)

    def test_genaimage_port_is_published_symmetrically(self):
        text = (ROOT / "compose.yaml").read_text(encoding="utf-8")
        self.assertIn('"127.0.0.1:${GENAIMAGE_PORT:-8190}:${GENAIMAGE_PORT:-8190}"', text)
        self.assertIn("GENAIMAGE_SERVER_PORT: ${GENAIMAGE_PORT:-8190}", text)

    def test_host_networking_is_never_used(self):
        for path in COMPOSE_FILES:
            self.assertNotIn("network_mode", path.read_text(encoding="utf-8"), path.name)

    def test_third_party_images_are_pinned_by_digest(self):
        for path in COMPOSE_FILES:
            for line in path.read_text(encoding="utf-8").splitlines():
                match = EXTERNAL_IMAGE.match(line)
                if match and "/ollama/" in match.group(1):
                    self.assertRegex(match.group(1), r"@sha256:[0-9a-f]{64}$", path.name)

    def test_comfyui_dockerfile_base_is_pinned_by_digest(self):
        text = (ROOT / "docker" / "comfyui" / "Dockerfile").read_text(encoding="utf-8")
        self.assertRegex(text, r"python:[\w.-]+@sha256:[0-9a-f]{64}")
        self.assertRegex(text, r"COMFYUI_COMMIT=[0-9a-f]{40}")

    def test_no_model_is_configured_by_default(self):
        text = (ROOT / "compose.yaml").read_text(encoding="utf-8")
        self.assertNotRegex(text, r"GENAIMAGE_LLM_(TEXT|VISION)_MODEL:[ \t]*\S")


if __name__ == "__main__":
    unittest.main()
