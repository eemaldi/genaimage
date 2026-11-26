import re
import sys
import urllib.request

PACKAGES = ("torch", "torchvision")
DEFAULTS = {
    "TORCH_VERSION": "torch",
    "TORCHVISION_VERSION": "torchvision",
}


def pinned_versions(dockerfile):
    return {
        package: re.search(rf"^ARG {arg}=(\S+)$", dockerfile, re.MULTILINE).group(1)
        for arg, package in DEFAULTS.items()
    }


def index_has(page, package, version, index):
    return re.search(rf"{package}-{re.escape(version)}\+{re.escape(index)}-cp312-cp312-manylinux\w*_x86_64\.whl", page)


def main(dockerfile_path, indexes):
    versions = pinned_versions(open(dockerfile_path, encoding="utf-8").read())
    missing = []
    for index in indexes:
        for package in PACKAGES:
            url = f"https://download.pytorch.org/whl/{index}/{package}/"
            page = urllib.request.urlopen(url, timeout=30).read().decode()
            if not index_has(page, package, versions[package], index):
                missing.append(f"{package}=={versions[package]} on {index}")
    for line in missing:
        print(f"missing wheel: {line}", file=sys.stderr)
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1], sys.argv[2:]))
