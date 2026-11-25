#!/usr/bin/env python3
"""MLIR dialect tests (spec/mlir.md §4).

    mlir_roundtrip.py <pegasus> <pegasus-mlir> [--mutated FILE]

1. Every corpus program with a `main` (tests/run, tests/native,
   tests/sema/accept) is written as PIRB and passed through
   `pegasus-mlir roundtrip`: import, verify, print, parse, verify, print
   again (identical text), and export both the imported and the reparsed
   module (each byte-identical to the input PIRB).
2. Every file in backend/mlir/tests/invalid must be rejected by
   `pegasus-mlir verify`, with the message named on its first line
   (`// expected-error: <text>`).
3. With --mutated, every program of a NUL-separated corpus that the frontend
   accepts is round-tripped too.
"""
import glob, os, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def main():
    pegasus, tool = sys.argv[1], sys.argv[2]
    mutated = sys.argv[sys.argv.index("--mutated") + 1] if "--mutated" in sys.argv else None
    work = tempfile.mkdtemp(prefix="pg-mlir-")
    bad, ok, skipped = [], 0, 0

    def roundtrip(name, src_path):
        nonlocal ok, skipped
        pirb = os.path.join(work, f"m{ok + skipped + len(bad)}.pirb")
        r = subprocess.run([pegasus, "pirb", src_path, pirb], capture_output=True, text=True)
        if r.returncode != 0:
            skipped += 1  # no `main` (library-only accept cases) or rejected
            return
        r = subprocess.run([tool, "roundtrip", pirb], capture_output=True, text=True)
        if r.returncode == 0:
            ok += 1
        else:
            bad.append(f"{name}: {(r.stdout + r.stderr).strip()[-600:]}")

    for d in ("run", "native", "sema/accept"):
        for p in sorted(glob.glob(os.path.join(ROOT, "tests", d, "*.peg"))):
            roundtrip(os.path.relpath(p, ROOT), p)
    corpus_ok = ok
    if mutated:
        progs = open(mutated, "rb").read().split(b"\0")
        for k, prog in enumerate(progs):
            p = os.path.join(work, f"mut{k}.peg")
            open(p, "wb").write(prog)
            roundtrip(f"mutated #{k}", p)
    invalid = 0
    for p in sorted(glob.glob(os.path.join(ROOT, "backend", "mlir", "tests", "invalid", "*.mlir"))):
        first = open(p, encoding="utf-8").readline()
        want = first.split("expected-error:", 1)[1].strip() if "expected-error:" in first else None
        r = subprocess.run([tool, "verify", p], capture_output=True, text=True)
        text = r.stdout + r.stderr
        if r.returncode == 0 or not want or want not in text:
            bad.append(f"{os.path.relpath(p, ROOT)}: expected rejection with '{want}', got exit {r.returncode}: "
                       f"{text.strip()[-400:]}")
        else:
            invalid += 1
    for b in bad[:10]:
        print("FAIL " + b)
    print(f"round trips: {corpus_ok} corpus" + (f", {ok - corpus_ok} mutated" if mutated else "")
          + f" ok ({skipped} skipped: no main or rejected); invalid modules rejected: {invalid}; failures: {len(bad)}")
    sys.exit(1 if bad else 0)


main()
