#!/usr/bin/env python3
"""MLIR dialect tests (spec/mlir.md §4).

    mlir_roundtrip.py <pegasus> <pegasus-mlir> <pegasus-native> [--mutated FILE]

1. Corpus round trips. Every program in tests/run and tests/native must
   compile to PIRB (any failure fails the test); in tests/sema/accept, the
   only allowed failure is the explicit "no main" outcome, and those cases
   are listed. Each PIRB goes through `pegasus-mlir roundtrip` (import,
   verify, print, parse, verify, print again: identical text; export of both
   modules: identical PIRB apart from the verified flag) and through the
   full trust chain: `pegasus-mlir import`, `pegasus-mlir export` (writes
   UNVERIFIED PIRB), `pegasus pirb-verify` (the Rust verifier certifies it),
   whose output must be byte-identical to the original PIRB.
2. Every module in backend/mlir/tests/invalid must be rejected by
   `pegasus-mlir verify` with the message on its first line
   (`// expected-error: <text>`).
3. Every module in backend/mlir/tests/semantic_invalid passes MLIR
   verification but is invalid PIR: it must export (as unverified PIRB, flag
   byte 0), `pegasus pirb-verify` must reject it with the message on its
   first line and write no file, and `pegasus-native` must refuse the
   unverified export.
4. With --mutated, programs of a NUL-separated corpus are round-tripped
   through both paths of 1. Programs the frontend rejects, or that have no
   main, are skipped (counted); any other compiler failure fails the test.
"""
import glob, os, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NO_MAIN = "has no `fn main()`"


def main():
    pegasus, tool, native = sys.argv[1], sys.argv[2], sys.argv[3]
    mutated = sys.argv[sys.argv.index("--mutated") + 1] if "--mutated" in sys.argv else None
    work = tempfile.mkdtemp(prefix="pg-mlir-")
    bad = []
    counts = {"corpus": 0, "mutated": 0, "no_main": [], "mutated_rejected": 0, "mutated_no_main": 0}
    serial = [0]

    def path(ext):
        serial[0] += 1
        return os.path.join(work, f"m{serial[0]}{ext}")

    def run(args):
        return subprocess.run(args, capture_output=True, text=True)

    def roundtrip(name, pirb):
        r = run([tool, "roundtrip", pirb])
        if r.returncode != 0:
            bad.append(f"{name}: roundtrip: {(r.stdout + r.stderr).strip()[-600:]}")
            return False
        mlir, unverified, verified = path(".mlir"), path(".unverified.pirb"), path(".pirb")
        for step in ([tool, "import", pirb, mlir], [tool, "export", mlir, unverified],
                     [pegasus, "pirb-verify", unverified, verified]):
            r = run(step)
            if r.returncode != 0:
                bad.append(f"{name}: {os.path.basename(step[0])} {step[1]}: {(r.stdout + r.stderr).strip()[-600:]}")
                return False
        if open(unverified, "rb").read()[8] != 0:
            bad.append(f"{name}: pegasus-mlir export set the verified flag")
            return False
        if open(verified, "rb").read() != open(pirb, "rb").read():
            bad.append(f"{name}: the certified export differs from the original PIRB")
            return False
        return True

    def compile_(src):
        pirb = path(".pirb")
        r = run([pegasus, "pirb", src, pirb])
        return pirb, r

    # 1. Corpus.
    for d in ("run", "native", "sema/accept"):
        for p in sorted(glob.glob(os.path.join(ROOT, "tests", d, "*.peg"))):
            name = os.path.relpath(p, ROOT)
            pirb, r = compile_(p)
            if r.returncode != 0:
                if d == "sema/accept" and NO_MAIN in r.stderr:
                    counts["no_main"].append(name)
                else:
                    bad.append(f"{name}: does not compile to PIRB (exit {r.returncode}): {r.stderr.strip()[-400:]}")
                continue
            counts["corpus"] += roundtrip(name, pirb)

    # 2. Structurally invalid modules.
    invalid = 0
    for p in sorted(glob.glob(os.path.join(ROOT, "backend", "mlir", "tests", "invalid", "*.mlir"))):
        name = os.path.relpath(p, ROOT)
        first = open(p, encoding="utf-8").readline()
        want = first.split("expected-error:", 1)[1].strip() if "expected-error:" in first else None
        r = run([tool, "verify", p])
        text = r.stdout + r.stderr
        if r.returncode == 0 or not want or want not in text:
            bad.append(f"{name}: expected rejection with '{want}', got exit {r.returncode}: {text.strip()[-400:]}")
        else:
            invalid += 1

    # 3. MLIR-valid, PIR-invalid modules: never certified, never consumed.
    semantic = 0
    for p in sorted(glob.glob(os.path.join(ROOT, "backend", "mlir", "tests", "semantic_invalid", "*.mlir"))):
        name = os.path.relpath(p, ROOT)
        first = open(p, encoding="utf-8").readline()
        want = first.split("expected-error:", 1)[1].strip() if "expected-error:" in first else None
        unverified, verified = path(".unverified.pirb"), path(".pirb")
        r1 = run([tool, "verify", p])
        r2 = run([tool, "export", p, unverified])
        if r1.returncode != 0 or r2.returncode != 0:
            bad.append(f"{name}: must pass MLIR verification and export: {(r1.stderr + r2.stderr).strip()[-400:]}")
            continue
        if open(unverified, "rb").read()[8] != 0:
            bad.append(f"{name}: export set the verified flag")
            continue
        r3 = run([pegasus, "pirb-verify", unverified, verified])
        if r3.returncode != 1 or not want or want not in r3.stderr or os.path.exists(verified):
            bad.append(f"{name}: pirb-verify must refuse it with '{want}' and write nothing; got exit "
                       f"{r3.returncode}: {r3.stderr.strip()[-400:]}")
            continue
        r4 = run([native, unverified, path(".ll")])
        if r4.returncode == 0 or "not marked verified" not in r4.stderr:
            bad.append(f"{name}: pegasus-native must refuse the unverified export; got exit {r4.returncode}")
            continue
        semantic += 1

    # 4. Mutated programs.
    if mutated:
        progs = open(mutated, "rb").read().split(b"\0")
        for k, prog in enumerate(progs):
            src = path(".peg")
            open(src, "wb").write(prog)
            chk = run([pegasus, "check", src])
            if chk.returncode == 1 and "error[" in chk.stderr:
                counts["mutated_rejected"] += 1
                continue
            if chk.returncode != 0:
                bad.append(f"mutated #{k}: pegasus check failed unexpectedly (exit {chk.returncode}): "
                           f"{chk.stderr.strip()[-300:]}")
                continue
            pirb, r = compile_(src)
            if r.returncode != 0:
                if NO_MAIN in r.stderr:
                    counts["mutated_no_main"] += 1
                else:
                    bad.append(f"mutated #{k}: accepted by check but does not compile (exit {r.returncode}): "
                               f"{r.stderr.strip()[-300:]}")
                continue
            counts["mutated"] += roundtrip(f"mutated #{k}", pirb)

    for b in bad[:10]:
        print("FAIL " + b)
    print(f"corpus round trips: {counts['corpus']} (each also certified by pirb-verify, byte-identical)")
    print(f"tests/sema/accept without main (skipped): {len(counts['no_main'])}"
          + (": " + ", ".join(os.path.basename(n) for n in counts["no_main"]) if counts["no_main"] else ""))
    if mutated:
        print(f"mutated round trips: {counts['mutated']} (rejected by the frontend: {counts['mutated_rejected']}, "
              f"no main: {counts['mutated_no_main']})")
    print(f"invalid modules rejected: {invalid}; semantic-invalid modules refused by pirb-verify and the "
          f"backend: {semantic}; failures: {len(bad)}")
    if counts["corpus"] < 30:
        print(f"FAIL only {counts['corpus']} corpus round trips (expected at least 30)")
        sys.exit(1)
    sys.exit(1 if bad else 0)


main()
