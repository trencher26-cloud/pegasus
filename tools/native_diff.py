#!/usr/bin/env python3
"""Differential test: native executables vs the reference interpreter.

For every program, `pegasus run` (interpreter) and `pegasus build` + running
the executable must agree on stdout, exit status and the trap message (the
first line of stderr). With --mlir, the test is three-way: every program is
built by both native paths, the direct LLVM IR emitter (`pegasus-native`)
and the MLIR lowering (`pegasus-mlir lower`, `pegasus build --backend=mlir`);
both executables must match the interpreter, and the two backends must agree
on which programs are outside the native subset. Sources of programs:

  corpus   tests/native/*.peg must all build; tests/run/*.peg must build or
           be refused as "not supported by the native backend yet"
  intops   every integer operator on random operands and types (the same
           space as tools/int_oracle.py), one program per operation
  random   generated programs with functions, `inout` parameters, borrowed
           returns, structs, arrays, loops, branches and mixed overflow modes
  mutated  (--mutated FILE) NUL-separated sources, e.g. a fuzzing corpus:
           whatever the frontend accepts and the backend supports must match

Usage: tools/native_diff.py <pegasus> <pegasus-native> <runtime> [--mlir PEGASUS_MLIR]
       [--intops N] [--random N] [--seed S] [--mutated FILE] [--clang CLANG] [--keep DIR]
"""
import argparse, glob, os, random, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
INT_TYPES = {"i8": (True, 8), "i16": (True, 16), "i32": (True, 32), "i64": (True, 64), "i128": (True, 128),
             "u8": (False, 8), "u16": (False, 16), "u32": (False, 32), "u64": (False, 64), "u128": (False, 128)}


class Runner:
    def __init__(self, a):
        self.a = a
        self.env = dict(os.environ, PEGASUS_NATIVE=a.native, PEGASUS_RT=a.runtime, PEGASUS_CLANG=a.clang)
        if a.mlir:
            self.env["PEGASUS_MLIR"] = a.mlir
        self.backends = ["direct"] + (["mlir"] if a.mlir else [])
        self.dir = a.keep or tempfile.mkdtemp(prefix="pg-native-")
        os.makedirs(self.dir, exist_ok=True)
        self.n = 0

    def compare(self, path):
        """Returns (status, detail): status is 'match', 'unsupported', 'rejected' or 'MISMATCH'."""
        self.n += 1
        i = subprocess.run([self.a.pegasus, "run", path], capture_output=True, text=True, timeout=120)
        if i.returncode not in (0, 101):
            return "rejected", i.stderr.strip()[:300]
        if i.stderr.startswith("trap: step limit reached"):
            # The step limit guards the interpreter only; it is not language
            # semantics, and native code has no such limit (spec/native.md §5).
            return "step-limit", ""
        first = lambda s: s.splitlines()[0] if s else ""
        results = {}
        for be in self.backends:
            exe = os.path.join(self.dir, f"prog{self.n}.{be}" + (".exe" if os.name == "nt" else ""))
            b = subprocess.run([self.a.pegasus, "build", f"--backend={be}", path, exe], capture_output=True,
                               text=True, timeout=300, env=self.env)
            if b.returncode != 0:
                if "not supported by the native backend yet" in b.stderr:
                    results[be] = ("unsupported", b.stderr.strip())
                    continue
                # The linker's own messages (MSVC link.exe) arrive on stdout.
                results[be] = ("MISMATCH", f"{be}: build failed ({b.returncode}): {b.stderr.strip()[:2000]}"
                               f"{chr(10) + b.stdout.strip()[:2000] if b.stdout.strip() else ''}")
                continue
            n = subprocess.run([exe], capture_output=True, text=True, timeout=120)
            if not self.a.keep:
                os.remove(exe)
            if i.stdout == n.stdout and i.returncode == n.returncode and first(i.stderr) == first(n.stderr):
                results[be] = ("match", "")
            else:
                results[be] = ("MISMATCH", f"{be}: interpreter: exit {i.returncode} stdout {i.stdout.strip()[:300]!r} "
                               f"stderr {first(i.stderr)!r}\n  {be}: exit {n.returncode} stdout "
                               f"{n.stdout.strip()[:300]!r} stderr {first(n.stderr)!r}")
        sts = {st for st, _ in results.values()}
        if "MISMATCH" in sts:
            return "MISMATCH", "\n  ".join(d for st, d in results.values() if st == "MISMATCH")
        if sts == {"unsupported"}:
            return "unsupported", results["direct"][1]
        if len(sts) > 1:
            # One backend built the program, the other refused it.
            return "MISMATCH", "the backends disagree on the native subset: " + "; ".join(
                f"{be}: {st} {d[:300]}" for be, (st, d) in results.items())
        return "match", ""


def rng(t):
    s, b = INT_TYPES[t]
    return (-(1 << (b - 1)), (1 << (b - 1)) - 1) if s else (0, (1 << b) - 1)


def lit(v, t):
    lo = rng(t)[0]
    if v == lo and v < 0:
        return f"(-{-v - 1}{t} - 1{t})"
    return f"{v}{t}" if v >= 0 else f"(-{-v}{t})"


def pick(r, t):
    lo, hi = rng(t)
    c = r.random()
    if c < 0.3:
        return r.choice([lo, hi, 0, 1, -1 if lo < 0 else 2, lo + 1, hi - 1])
    if c < 0.6:
        return r.randint(max(lo, -300), min(hi, 300))
    return r.randint(lo, hi)


def intop_program(r):
    ops = ["+", "-", "*", "/", "%", "+%", "-%", "*%", "+|", "-|", "*|", "<<", ">>", "&", "|", "^", "**"]
    t = r.choice(list(INT_TYPES))
    op = r.choice(ops)
    bt = t
    a = pick(r, t)
    if op in ("<<", ">>", "**"):
        bt = r.choice(["u8", "u16", "u32", "u64", "u128"])  # unsigned shift amounts (T003)
        b = r.choice([0, 1, 2, 3, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, r.randint(0, 200)])
        if op == "**":
            bt = r.choice(["u8", "u32", "u64", "u128"])  # `**` needs an unsigned exponent (T003)
            b = r.choice([r.randint(0, 130), 0, 1, 2, rng(bt)[1]])
        b = max(rng(bt)[0], min(b, rng(bt)[1]))
    else:
        b = pick(r, t)
    # Operands come through parameters so nothing is constant-folded away.
    return (f"fn op(a: {t}, b: {bt}) -> {t} = a {op} b\n"
            f"fn main() -> {t} = op({lit(a, t)}, {lit(b, bt)})\n")


class Gen:
    """Random well-typed programs in the native subset (one integer type T
    per program, `usize` loop counters)."""

    def __init__(self, r):
        self.r = r
        self.t = r.choice(["i32", "i64", "u8", "u32", "u64", "i8", "i16", "u16", "i128", "u128"])
        self.n = r.randint(2, 5)  # array length
        self.funcs = []  # (name, kind, sig) of generated functions

    def lit(self):
        return lit(pick(self.r, self.t), self.t)

    def expr(self, vars_, d=0):
        r = self.r
        if d > 2 or r.random() < 0.3:
            c = r.random()
            if vars_ and c < 0.6:
                return r.choice(vars_)
            return self.lit()
        c = r.random()
        if c < 0.55:
            op = r.choice(["+%", "-%", "*%", "+|", "-|", "*|", "&", "|", "^", "+%", "-%", "^",
                           "+", "-", "*", "/", "%"])
            return f"({self.expr(vars_, d + 1)} {op} {self.expr(vars_, d + 1)})"
        if c < 0.65:
            sh = r.choice(["<<", ">>"])
            bits = INT_TYPES[self.t][1]
            amt = r.choice([0, 1, 3, bits - 1, r.randint(0, bits - 1)] + ([bits] if r.random() < 0.1 else []))
            return f"({self.expr(vars_, d + 1)} {sh} {amt}u32)"
        if c < 0.75:
            return f"(if {self.cond(vars_, d + 1)} {{ {self.expr(vars_, d + 1)} }} else {{ {self.expr(vars_, d + 1)} }})"
        calls = [f for f in self.funcs if f[1] == "pure"]
        if calls and c < 0.9:
            name = r.choice(calls)[0]
            return f"{name}({self.expr(vars_, d + 1)}, {self.expr(vars_, d + 1)})"
        return f"({self.expr(vars_, d + 1)} +% {self.lit()})"

    def cond(self, vars_, d=0):
        op = self.r.choice(["<", "<=", ">", ">=", "==", "!="])
        # Parenthesized: after `ident <`, rule B's bounded scan may read a
        # comparison followed by a block as generic arguments (DEFERRED P-9).
        return f"({self.expr(vars_, d + 1)} {op} {self.expr(vars_, d + 1)})"

    def block(self, vars_, arrs, depth, indent):
        r, ind = self.r, "    " * indent
        out = []
        local = list(vars_)
        for _ in range(r.randint(1, 4)):
            c = r.random()
            if c < 0.25:
                v = f"v{r.randint(0, 10**6)}"
                out.append(f"{ind}var {v}: {self.t} = {self.expr(local)}")
                local.append(v)
            elif c < 0.45 and local:
                v = r.choice(local)
                if v.startswith("p."):
                    out.append(f"{ind}{v} = {self.expr(local)}")
                elif not v.startswith("arg_"):
                    out.append(f"{ind}{v} = {self.expr(local)}")
            elif c < 0.55 and arrs:
                a = r.choice(arrs)
                k = f"k{r.randint(0, 10**6)}"
                out.append(f"{ind}var {k}: usize = {r.randint(0, self.n + (1 if r.random() < 0.15 else -1))}")
                out.append(f"{ind}{a}[{k}] = {self.expr(local)}")
                local.append(f"{a}[{k} % {self.n}]")
            elif c < 0.65 and depth < 2:
                out.append(f"{ind}if {self.cond(local)} {{")
                out += self.block(local, arrs, depth + 1, indent + 1)
                out.append(f"{ind}}} else {{")
                out += self.block(local, arrs, depth + 1, indent + 1)
                out.append(f"{ind}}}")
            elif c < 0.75 and depth < 2:
                i = f"i{r.randint(0, 10**6)}"
                out.append(f"{ind}var {i}: usize = 0")
                out.append(f"{ind}while ({i} < {r.randint(1, 6)}) {{")
                out += self.block(local, arrs, depth + 1, indent + 1)
                out.append(f"{ind}    {i} += 1")
                out.append(f"{ind}}}")
            else:
                muts = [f for f in self.funcs if f[1] == "inout"]
                cands = [v for v in local if not v.startswith("arg_") and "[" not in v]
                if muts and cands:
                    v = r.choice(cands)
                    root = v.split(".")[0]
                    # Exclusivity (O003): the other argument must not read
                    # the exclusively borrowed variable.
                    rest = [x for x in local if x.split(".")[0] != root]
                    out.append(f"{ind}{r.choice(muts)[0]}(&{v}, {self.expr(rest)})")
                elif cands:
                    out.append(f"{ind}{r.choice(cands)} = {self.expr(local)}")
        return out

    def program(self):
        r, T, n = self.r, self.t, self.n
        src = [f"struct P {{\n    a: {T}\n    b: {T}\n}}\n",
               f"fn first(p: P) -> in {T} = p.a\n",
               f"fn pick(p: P, q: P, c: bool) -> in P = if c {{ p }} else {{ q }}\n",
               f"fn elem(xs: [{T}; {n}], i: usize) -> in {T} = xs[i % {n}]\n"]
        for k in range(r.randint(2, 5)):
            name = f"f{k}"
            if r.random() < 0.35:
                body = self.block(["x", "arg_v"], [], 1, 1)
                body = [l.replace("arg_v", "v") for l in body]
                src.append(f"fn {name}(x: inout {T}, v: {T}) {{\n" + "\n".join(body) +
                           f"\n    x = x +% v\n}}\n")
                self.funcs.append((name, "inout", None))
            else:
                body = self.block(["acc", "arg_a", "arg_b"], [], 1, 1)
                body = [l.replace("arg_a", "a").replace("arg_b", "b") for l in body]
                src.append(f"fn {name}(a: {T}, b: {T}) -> {T} {{\n    var acc: {T} = {self.expr(['a', 'b'])}\n"
                           + "\n".join(body) + "\n    acc\n}\n")
                self.funcs.append((name, "pure", None))
        body = ["    var acc: " + T + " = " + self.lit(),
                f"    var p = P {{ a: {self.lit()}, b: {self.lit()} }}",
                f"    var arr = [{', '.join(self.lit() for _ in range(n))}]"]
        body += self.block(["acc", "p.a", "p.b"], ["arr"], 0, 1)
        body.append(f"    let q = P {{ a: p.b, b: acc }}")
        body.append(f"    let c = {self.cond(['acc', 'p.a'])}")
        result = ["acc", "p.a", "p.b", "arr", "first(p)", "first(pick(p, q, c))", f"elem(arr, {r.randint(0, 9)})"]
        rt = ", ".join([T, T, T, f"[{T}; {n}]", T, T, T])
        src.append(f"fn main() -> ({rt}) {{\n" + "\n".join(body) + f"\n    ({', '.join(result)})\n}}\n")
        return "\n".join(src)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("pegasus")
    ap.add_argument("native")
    ap.add_argument("runtime")
    ap.add_argument("--intops", type=int, default=0)
    ap.add_argument("--random", type=int, default=0)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--clang", default="clang")
    ap.add_argument("--keep")
    ap.add_argument("--mutated")
    ap.add_argument("--mlir", help="pegasus-mlir: also build with the MLIR lowering (three-way test)")
    a = ap.parse_args()
    run = Runner(a)
    bad, counts = [], {}

    def tally(kind, st, detail, path, src=None):
        counts.setdefault(kind, {}).setdefault(st, 0)
        counts[kind][st] += 1
        if st == "MISMATCH" or (kind == "native" and st != "match") or (kind in ("random", "intops") and st == "rejected"):
            bad.append(f"[{kind}] {path}: {st}: {detail}" + (f"\n----\n{src}----" if src else ""))

    for p in sorted(glob.glob(os.path.join(ROOT, "tests", "native", "*.peg"))):
        st, d = run.compare(p)
        tally("native", st, d, os.path.relpath(p, ROOT))
    for p in sorted(glob.glob(os.path.join(ROOT, "tests", "run", "*.peg"))):
        st, d = run.compare(p)
        tally("run", st, d, os.path.relpath(p, ROOT))
    r = random.Random(a.seed)
    work = tempfile.mkdtemp(prefix="pg-diff-src-")
    for k in range(a.intops):
        src = intop_program(r)
        p = os.path.join(work, f"op{k}.peg")
        open(p, "w").write(src)
        st, d = run.compare(p)
        tally("intops", st, d, f"op{k}", src)
    for k in range(a.random):
        src = Gen(r).program()
        p = os.path.join(work, f"rand{k}.peg")
        open(p, "w").write(src)
        st, d = run.compare(p)
        tally("random", st, d, f"rand{k}", src)
    if a.mutated:
        progs = open(a.mutated, "rb").read().split(b"\0")
        for k, prog in enumerate(progs):
            p = os.path.join(work, f"mut{k}.peg")
            open(p, "wb").write(prog)
            chk = subprocess.run([a.pegasus, "check", p], capture_output=True)
            if chk.returncode != 0:
                continue
            try:
                st, d = run.compare(p)
            except subprocess.TimeoutExpired:
                st, d = "timeout", ""
            tally("mutated", st, d, f"mut{k}", prog.decode("utf-8", "replace"))
    for line in bad[:10]:
        print(line)
    for kind, c in counts.items():
        print(f"{kind}: " + ", ".join(f"{v} {k}" for k, v in sorted(c.items())))
    print("backends: " + " and ".join(run.backends) + f"; {len(bad)} failures")
    sys.exit(1 if bad else 0)


main()
