#!/usr/bin/env python3
"""Tests for tools/check_toolchain.py with fake clang executables (review
finding 3 of PIR0.4: a clang reporting the pinned version but another
commit passed). Run: python3 tools/test_check_toolchain.py"""
import hashlib, json, os, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from toolchain_lock import llvm_pin  # noqa: E402

PIN = llvm_pin()
V, C = PIN["version"], PIN["commit"]
OTHER = "0123456789abcdef0123456789abcdef01234567"
failures = 0


def fake(dirname, first_line):
    """A fake clang printing `first_line` for --version, at <dir>/bin."""
    b = os.path.join(dirname, "bin")
    os.makedirs(b, exist_ok=True)
    if os.name == "nt":
        p = os.path.join(b, "clang.cmd")
        open(p, "w").write(f"@echo {first_line}\n@echo Target: x86_64-pc-windows-msvc\n")
    else:
        p = os.path.join(b, "clang")
        open(p, "w").write(f"#!/bin/sh\necho '{first_line}'\necho 'Target: x86_64-unknown-linux-gnu'\n")
        os.chmod(p, 0o755)
    return p


def check(name, clang, args, ok, needle):
    global failures
    r = subprocess.run([sys.executable, os.path.join(HERE, "check_toolchain.py"), clang] + args,
                       capture_output=True, text=True)
    text = r.stdout + r.stderr
    good = (r.returncode == 0) == ok and needle in text
    print(("ok   " if good else "FAIL ") + name)
    if not good:
        failures += 1
        print("     exit", r.returncode, text.strip())


with tempfile.TemporaryDirectory() as t:
    pinned = fake(os.path.join(t, "pinned"), f"clang version {V} (https://github.com/llvm/llvm-project.git {C})")
    other = fake(os.path.join(t, "other"), f"clang version {V} (https://github.com/llvm/llvm-project.git {OTHER})")
    distro = fake(os.path.join(t, "distro"), f"Ubuntu clang version {V} (1ubuntu1)")
    wrongv = fake(os.path.join(t, "wrongv"), f"clang version 17.0.6 (https://github.com/llvm/llvm-project.git {C})")
    check("pinned version and commit pass", pinned, [], True, "matches toolchain.lock")
    check("pinned version, other commit fails", other, [], False, "was built from llvm-project " + OTHER)
    check("other commit fails even with --allow-unpinned", other, ["--allow-unpinned"], False, OTHER)
    check("no commit reported fails strictly", distro, [], False, "cannot be verified")
    check("no commit reported passes --allow-unpinned with a warning", distro, ["--allow-unpinned"], True,
          "is NOT enforced")
    check("wrong version fails", wrongv, [], False, "reports version 17.0.6")
    check("wrong version fails with --allow-unpinned", wrongv, ["--allow-unpinned"], False, "17.0.6")
    check("--require-stamp without a stamp fails", pinned, ["--require-stamp"], False, "no provisioning stamp")
    stamp = os.path.join(t, "pinned", "pegasus-toolchain.json")
    real = os.path.realpath(pinned)
    digest = hashlib.sha256(open(real, "rb").read()).hexdigest()
    json.dump({"commit": C, "version": V, "clang_sha256": digest}, open(stamp, "w"))
    check("stamp with the binary's hash passes", pinned, ["--require-stamp"], True, "binary hash verified")
    json.dump({"commit": C, "version": V, "clang_sha256": "0" * 64}, open(stamp, "w"))
    check("stamp with another binary's hash fails", pinned, ["--require-stamp"], False, "does not match the SHA-256")
    json.dump({"commit": OTHER, "version": V, "clang_sha256": digest}, open(stamp, "w"))
    check("stamp for another commit fails", pinned, ["--require-stamp"], False, "stamp records " + OTHER)
    check("missing clang fails", os.path.join(t, "nope", "clang"), [], False, "FAIL")

print(f"{failures} failures")
sys.exit(1 if failures else 0)
