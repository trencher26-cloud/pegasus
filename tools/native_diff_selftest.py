#!/usr/bin/env python3
"""Self-test of tools/native_diff.py's outcome policy (review of PR #4).

    native_diff_selftest.py

Runs native_diff.main() in-process with subprocess.run replaced by a fake
toolchain (no compiler, no executables), and checks which outcomes fail the
differential test. Each scenario perturbs one program; the real corpus
paths (tests/native, tests/run) are used. In particular: a tests/run
program the interpreter suddenly rejects must fail native_differential, so
coverage cannot shrink while CTest stays green.
"""
import contextlib, io, os, subprocess, sys, tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import native_diff  # noqa: E402

REAL_RUN = subprocess.run
UNSUPPORTED = "error: not supported by the native backend yet: floating-point types"


def fake_toolchain(perturb):
    """perturb(kind, args) -> CompletedProcess or None (default behaviour).
    kind is 'run', 'build-direct', 'build-mlir', 'exe' or 'check'."""

    def run(args, **kw):
        args = [str(a) for a in args]
        if len(args) >= 2 and args[1] == "run":
            kind = "run"
        elif len(args) >= 2 and args[1] == "check":
            kind = "check"
        elif len(args) >= 3 and args[1] == "build":
            kind = "build-" + args[2].split("=", 1)[1]
        else:
            kind = "exe"
        r = perturb(kind, args)
        if r is None:
            r = subprocess.CompletedProcess(args, 0, "42\n", "")
        if kind.startswith("build") and r.returncode == 0:
            open(args[-1], "w").close()  # the "executable" native_diff removes
        return r

    return run


def scenario(name, perturb, extra=(), expect_fail=True, expect_text=None):
    tmp = tempfile.mkdtemp(prefix="pg-diff-self-")
    mutated = os.path.join(tmp, "corpus.bin")
    with open(mutated, "wb") as f:
        f.write(b"fn main() -> u32 { 1 }\0fn helper() -> u32 { 2 }\0")
    sys.argv = ["native_diff.py", "pegasus", "pegasus-native", "rt", "--mlir", "pegasus-mlir",
                "--intops", "2", "--random", "2", "--seed", "1", "--mutated", mutated, *extra]
    out = io.StringIO()
    subprocess.run = fake_toolchain(perturb)
    try:
        with contextlib.redirect_stdout(out):
            native_diff.main()
        code = 0
    except SystemExit as e:
        code = e.code or 0
    finally:
        subprocess.run = REAL_RUN
    text = out.getvalue()
    ok = (code != 0) == expect_fail and (expect_text is None or expect_text in text)
    print(("ok   " if ok else "FAIL ") + f"{name}: exit {code}")
    if not ok:
        print("     " + text.strip().replace("\n", "\n     ")[-1500:])
    return ok


def cp(code, out="", err=""):
    return subprocess.CompletedProcess([], code, out, err)


def first(dirname):
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    return sorted(f for f in os.listdir(os.path.join(root, "tests", dirname)) if f.endswith(".peg"))[0]


def on(kind_wanted, file_part, result):
    return lambda kind, args: result if kind == kind_wanted and file_part in args[-1 if kind == "run" else -2] else None


RUN0, NATIVE0 = first("run"), first("native")
results = [
    scenario("everything matches", lambda k, a: None, expect_fail=False),
    # The regression the review asked for: an accepted tests/run program
    # that `pegasus run` now rejects.
    scenario("tests/run program rejected by the interpreter",
             on("run", RUN0, cp(1, err="error[E0001]: simulated rejection")), expect_text="[run]"),
    scenario("tests/native program rejected by the interpreter",
             on("run", NATIVE0, cp(1, err="error[E0001]: simulated rejection")), expect_text="[native]"),
    scenario("tests/run program outside the native subset (both backends)",
             lambda k, a: cp(1, err=UNSUPPORTED) if k.startswith("build") and RUN0 in a[-2] else None,
             expect_fail=False),
    scenario("tests/native program outside the native subset",
             lambda k, a: cp(1, err=UNSUPPORTED) if k.startswith("build") and NATIVE0 in a[-2] else None),
    scenario("backends disagree on the native subset",
             lambda k, a: cp(1, err=UNSUPPORTED) if k == "build-mlir" and RUN0 in a[-2] else None,
             expect_text="disagree"),
    scenario("MLIR executable differs from the interpreter",
             lambda k, a: cp(0, "41\n") if k == "exe" and ".mlir" in a[0] else None, expect_text="mlir:"),
    scenario("generated program outside the native subset",
             lambda k, a: cp(1, err=UNSUPPORTED) if k.startswith("build") and "rand0" in a[-2] else None,
             expect_text="[random]"),
    scenario("generated program stopped by the step limit",
             lambda k, a: cp(101, err="trap: step limit reached") if k == "run" and "rand0" in a[-1] else None,
             expect_fail=False),
    scenario("mutated program without main",
             lambda k, a: cp(1, err="error: the package has no `fn main()` without parameters to run")
             if k == "run" and "mut1" in a[-1] else None, expect_fail=False),
    scenario("mutated program rejected by the interpreter after `pegasus check`",
             lambda k, a: cp(1, err="error: simulated interpreter failure") if k == "run" and "mut0" in a[-1] else None,
             expect_text="[mutated]"),
]
print(f"native_diff self-test: {results.count(False)} failures of {len(results)} scenarios")
sys.exit(0 if all(results) else 1)
