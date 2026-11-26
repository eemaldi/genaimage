import json
import subprocess
import sys
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(SCRIPTS))

import check_compose_config
import check_torch_wheels


def config(**services):
    return {"services": services}


class ResolvedConfigTests(unittest.TestCase):
    def test_loopback_long_syntax_passes(self):
        ok = config(a={"ports": [{"host_ip": "127.0.0.1", "published": "8190", "target": 8190}]})
        self.assertEqual(check_compose_config.violations(ok), [])

    def test_wildcard_long_syntax_fails(self):
        bad = config(a={"ports": [{"host_ip": "0.0.0.0", "published": "8190", "target": 8190}]})
        self.assertEqual(len(check_compose_config.violations(bad)), 1)

    def test_missing_host_ip_fails(self):
        bad = config(a={"ports": [{"published": "8190", "target": 8190}]})
        self.assertEqual(len(check_compose_config.violations(bad)), 1)

    def test_network_mode_fails(self):
        self.assertEqual(len(check_compose_config.violations(config(a={"network_mode": "host"}))), 1)

    def test_service_without_ports_passes(self):
        self.assertEqual(check_compose_config.violations(config(a={"image": "x"})), [])

    def test_exit_status_reads_stdin(self):
        bad = json.dumps(config(a={"ports": [{"host_ip": "0.0.0.0", "target": 1}]}))
        result = subprocess.run(
            [sys.executable, str(SCRIPTS / "check_compose_config.py")],
            input=bad, text=True, capture_output=True,
        )
        self.assertEqual(result.returncode, 1)


class TorchWheelTests(unittest.TestCase):
    def test_pinned_versions_read_from_dockerfile(self):
        text = "ARG TORCH_VERSION=2.14.1\nARG TORCHVISION_VERSION=0.29.1\n"
        self.assertEqual(
            check_torch_wheels.pinned_versions(text),
            {"torch": "2.14.1", "torchvision": "0.29.1"},
        )

    def test_index_match_requires_exact_version_and_index(self):
        page = '<a href="x">torch-2.14.1+cu130-cp312-cp312-manylinux_2_28_x86_64.whl</a>'
        self.assertTrue(check_torch_wheels.index_has(page, "torch", "2.14.1", "cu130"))
        self.assertFalse(check_torch_wheels.index_has(page, "torch", "2.14.1", "rocm7.2"))
        self.assertFalse(check_torch_wheels.index_has(page, "torch", "2.14.10", "cu130"))


if __name__ == "__main__":
    unittest.main()
