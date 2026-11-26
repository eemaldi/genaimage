import json
import sys

LOOPBACK = "127.0.0.1"


def violations(config):
    found = []
    for name, service in config.get("services", {}).items():
        if "network_mode" in service:
            found.append(f"{name}: network_mode is not allowed")
        for port in service.get("ports", []):
            if port.get("host_ip") != LOOPBACK:
                found.append(f"{name}: port {port.get('published')} is bound to {port.get('host_ip')!r}")
    return found


def main():
    found = violations(json.load(sys.stdin))
    for line in found:
        print(line, file=sys.stderr)
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())
