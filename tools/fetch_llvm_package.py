#!/usr/bin/env python3
"""Fetch, verify and unpack the prebuilt LLVM/MLIR package pinned in
toolchain-packages.lock.json (spec/native.md §7.1).

    fetch_llvm_package.py --platform P --dest DIR [--download-dir DIR] [--no-attestation]

1. The package must be pinned for the platform and match toolchain.lock's
   source pin (commit, version, release); otherwise fail.
2. Download every part from the release; each part's size and SHA-256
   must equal the lock; the reassembled archive's SHA-256 must equal the
   lock.
3. Provenance: every part must carry a GitHub build-provenance attestation
   from the builder workflow at the ref the lock records
   (`gh attestation verify --signer-workflow --source-ref`).
   Mandatory; --no-attestation exists only for local use and says loudly
   that provenance was NOT verified.
4. Unpack with Python's `data` filter (no absolute paths, no `..`, no links
   leaving the destination) into an empty directory.
5. check_toolchain.py --require-stamp on the unpacked clang: version,
   commit, stamp and binary hash.
"""
import argparse, os, shutil, subprocess, sys, tarfile, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from toolchain_packages import package_entry, sha256_file  # noqa: E402

EXE = ".exe" if os.name == "nt" else ""


def download(url, path):
    req = urllib.request.Request(url, headers={"User-Agent": "pegasus-toolchain-fetch"})
    with urllib.request.urlopen(req, timeout=600) as r, open(path, "wb") as f:
        shutil.copyfileobj(r, f, 1 << 20)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--platform", required=True)
    ap.add_argument("--dest", required=True)
    ap.add_argument("--download-dir")
    ap.add_argument("--no-attestation", action="store_true")
    ap.add_argument("--from-dir", help="take the parts from a local directory instead of the release (tests)")
    a = ap.parse_args()
    pin, e = package_entry(a.platform)
    dl = os.path.abspath(a.download_dir or a.dest + "-download")
    os.makedirs(dl, exist_ok=True)
    base = f"https://github.com/{e['repository']}/releases/download/{e['tag']}"
    paths = []
    for part in e["parts"]:
        p = os.path.join(dl, part["name"])
        if not (os.path.exists(p) and os.path.getsize(p) == part["size"] and sha256_file(p) == part["sha256"]):
            if a.from_dir:
                shutil.copyfile(os.path.join(a.from_dir, part["name"]), p)
            else:
                print(f"downloading {part['name']} ({part['size']} bytes)", flush=True)
                download(f"{base}/{part['name']}", p)
        if os.path.getsize(p) != part["size"] or sha256_file(p) != part["sha256"]:
            sys.exit(f"FAIL: {part['name']}: size or SHA-256 differs from toolchain-packages.lock.json")
        paths.append(p)
    if a.no_attestation:
        print("WARNING: provenance attestations NOT verified (--no-attestation)")
    else:
        for p in paths:
            r = subprocess.run(["gh", "attestation", "verify", p, "--repo", e["repository"],
                                "--signer-workflow", e["signer_workflow"], "--source-ref", e["source_ref"]],
                               capture_output=True, text=True)
            if r.returncode != 0:
                sys.exit(f"FAIL: provenance attestation of {os.path.basename(p)} did not verify:\n"
                         f"{(r.stdout + r.stderr).strip()}")
        print(f"provenance verified: {len(paths)} part(s) attested by {e['signer_workflow']} at {e['source_ref']}")
    archive = os.path.join(dl, e["archive"])
    with open(archive, "wb") as out:
        for p in paths:
            with open(p, "rb") as f:
                shutil.copyfileobj(f, out, 1 << 20)
    if sha256_file(archive) != e["sha256"]:
        sys.exit("FAIL: reassembled archive SHA-256 differs from toolchain-packages.lock.json")
    dest = os.path.abspath(a.dest)
    if os.path.exists(dest) and os.listdir(dest):
        sys.exit(f"FAIL: {dest} is not empty")
    os.makedirs(dest, exist_ok=True)
    try:
        with tarfile.open(archive, "r:xz") as t:
            t.extractall(dest, filter="data")
    except tarfile.FilterError as err:
        sys.exit(f"FAIL: unsafe archive member refused: {err}")
    os.remove(archive)
    clang = os.path.join(dest, "bin", "clang" + EXE)
    r = subprocess.run([sys.executable, os.path.join(HERE, "check_toolchain.py"), clang, "--require-stamp"])
    if r.returncode != 0:
        sys.exit(r.returncode)
    print(f"unpacked {e['archive']} ({pin['release']}, {pin['commit'][:12]}) into {dest}")


main()
