"""Shared reader for toolchain.lock's [llvm] section."""
import os, re, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def llvm_pin(path=None):
    text = open(path or os.path.join(ROOT, "toolchain.lock"), encoding="utf-8").read()
    if "[llvm]" not in text:
        sys.exit("toolchain.lock: no [llvm] section")
    sec = text[text.index("[llvm]"):]
    nxt = re.search(r"^\[(?!llvm\])", sec[1:], re.M)
    if nxt:
        sec = sec[: nxt.start() + 1]
    get = lambda k: (re.search(rf'^{k} = "([^"]*)"', sec, re.M) or [None, None])[1]
    pin = {k: get(k) for k in ("source", "release", "commit", "version", "projects")}
    if not pin["commit"] or not re.fullmatch(r"[0-9a-f]{40}", pin["commit"]):
        sys.exit("toolchain.lock: [llvm] commit must be a full 40-hex commit id")
    if pin["release"] != f"llvmorg-{pin['version']}":
        sys.exit("toolchain.lock: [llvm] release tag must be llvmorg-<version>")
    if not pin["source"] or not pin["projects"]:
        sys.exit("toolchain.lock: [llvm] needs source and projects")
    return pin
