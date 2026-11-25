"""Shared definitions for the prebuilt LLVM/MLIR toolchain packages
(spec/native.md §7.1). The source pin stays in toolchain.lock; the packages
built from it are pinned in toolchain-packages.lock.json, which records
every package's SHA-256 (whole archive and each part) and the builder
workflow whose provenance attestation must verify."""
import hashlib, json, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
from toolchain_lock import llvm_pin  # noqa: E402

PACKAGES_LOCK = os.environ.get("PEGASUS_PACKAGES_LOCK") or os.path.join(ROOT, "toolchain-packages.lock.json")
PLATFORMS = ("linux-x86_64", "windows-x86_64")
FORMAT = 1
# GitHub release assets are limited to 2 GiB each.
PART_SIZE = int(os.environ.get("PEGASUS_PACKAGE_PART_SIZE") or 1900 * 1024 * 1024)
BUILDER_WORKFLOW = ".github/workflows/toolchain.yml"


def release_tag(pin):
    return f"toolchain-llvm-{pin['version']}-{pin['commit'][:12]}"


def archive_name(pin, platform):
    return f"pegasus-llvm-{pin['version']}-{pin['commit'][:12]}-{platform}.tar.xz"


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def load_packages(path=None):
    p = path or PACKAGES_LOCK
    if not os.path.exists(p):
        return {"format": FORMAT, "packages": {}}
    data = json.load(open(p, encoding="utf-8"))
    if data.get("format") != FORMAT:
        sys.exit(f"{p}: unsupported format {data.get('format')!r}")
    return data


def package_entry(platform, path=None, lock=None):
    """The pinned package for `platform`, checked against the source pin.
    Exits if it is missing or inconsistent: packages are mandatory."""
    pin = llvm_pin(lock)
    data = load_packages(path)
    e = data.get("packages", {}).get(platform)
    if not e:
        sys.exit(f"FAIL: toolchain-packages.lock.json pins no package for {platform} "
                 f"(run the toolchain builder workflow and record its manifest)")
    for k in ("commit", "version", "release"):
        if e.get(k) != pin[k]:
            sys.exit(f"FAIL: package {platform} records {k} {e.get(k)!r}; toolchain.lock pins {pin[k]!r}")
    if e.get("tag") != release_tag(pin) or e.get("archive") != archive_name(pin, platform):
        sys.exit(f"FAIL: package {platform}: tag or archive name does not follow the source pin")
    if not e.get("sha256") or not e.get("parts") or not e.get("repository") or not e.get("signer_workflow"):
        sys.exit(f"FAIL: package {platform}: sha256, parts, repository and signer_workflow are mandatory")
    return pin, e
