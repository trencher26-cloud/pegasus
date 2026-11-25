#!/usr/bin/env python3
"""Differential oracle for Pegasus integer semantics (Phase 0 spec, section 7).

An independent Python model of every integer operator (trap / wrapping /
saturating arithmetic, division and remainder edge cases, shifts, bitwise
operators, `**`) is compared with the compiler + reference interpreter on
random operations. Usage: tools/int_oracle.py <pegasus binary> [count] [seed]
"""
import random, subprocess, sys, tempfile, os

TYPES = {"i8": (True, 8), "i16": (True, 16), "i32": (True, 32), "i64": (True, 64), "i128": (True, 128),
         "u8": (False, 8), "u16": (False, 16), "u32": (False, 32), "u64": (False, 64), "u128": (False, 128)}

def rng(t):
    s, b = TYPES[t]
    return (-(1 << (b - 1)), (1 << (b - 1)) - 1) if s else (0, (1 << b) - 1)

def wrap(v, t):
    s, b = TYPES[t]
    v &= (1 << b) - 1
    if s and v >> (b - 1):
        v -= 1 << b
    return v

def model(op, a, b, t, bt):
    lo, hi = rng(t)
    s, bits = TYPES[t]
    def trap_fit(v):
        return v if lo <= v <= hi else "trap"
    if op in ("+", "-", "*"):
        v = {"+": a + b, "-": a - b, "*": a * b}[op]
        return trap_fit(v)
    if op in ("+%", "-%", "*%"):
        v = {"+%": a + b, "-%": a - b, "*%": a * b}[op]
        return wrap(v, t)
    if op in ("+|", "-|", "*|"):
        v = {"+|": a + b, "-|": a - b, "*|": a * b}[op]
        return max(lo, min(hi, v))
    if op in ("/", "%"):
        if b == 0:
            return "trap"
        q = abs(a) // abs(b)
        if (a < 0) != (b < 0):
            q = -q
        if op == "/":
            return trap_fit(q)
        if s and b == -1:
            return 0  # MIN % -1 = 0
        return a - b * q
    if op in ("<<", ">>"):
        if b >= bits:
            return "trap"
        if op == "<<":
            return wrap(a << b, t)  # shifted-out bits are discarded
        return a >> b  # Python >> is arithmetic for negatives, logical for non-negatives
    if op == "**":
        return trap_fit(a ** b)  # exponents are at most 130 here: exact in Python
    if op in ("&", "|", "^"):
        ua, ub = a & ((1 << bits) - 1), b & ((1 << bits) - 1)
        v = {"&": ua & ub, "|": ua | ub, "^": ua ^ ub}[op]
        return wrap(v, t)
    raise ValueError(op)

def pick(r, t):
    lo, hi = rng(t)
    c = r.random()
    if c < 0.3:
        return r.choice([lo, hi, 0, 1, -1 if lo < 0 else 2, lo + 1, hi - 1])
    if c < 0.6:
        return r.randint(max(lo, -300), min(hi, 300))
    return r.randint(lo, hi)

def main():
    exe = sys.argv[1]
    n = int(sys.argv[2]) if len(sys.argv) > 2 else 600
    r = random.Random(int(sys.argv[3]) if len(sys.argv) > 3 else 1)
    ops = ["+", "-", "*", "/", "%", "+%", "-%", "*%", "+|", "-|", "*|", "<<", ">>", "&", "|", "^", "**"]
    bad = 0
    d = tempfile.mkdtemp()
    for i in range(n):
        t = r.choice(list(TYPES))
        op = r.choice(ops)
        bt = t
        a = pick(r, t)
        if op in ("<<", ">>", "**"):
            bt = r.choice(["u8", "u32", "u64"])
            b = r.choice([0, 1, 2, 3, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, r.randint(0, 200)])
            if op == "**":
                b = r.randint(0, 130)
            b = min(b, rng(bt)[1])
        else:
            b = pick(r, t)
        want = model(op, a, b, t, bt)
        src = (f"fn op(a: {t}, b: {bt}) -> {t} = a {op} b\n"
               f"fn lit_a() -> {t} = {a if a >= 0 else '-' + str(-a)}\n"
               f"fn main() -> {t} = op(lit_a(), {b})\n")
        if a < 0 and a == rng(t)[0]:
            # `-MIN` as a literal is out of range: build MIN as (MIN + 1) - 1.
            src = src.replace(f"fn lit_a() -> {t} = -{-a}", f"fn lit_a() -> {t} = -{-a - 1} - 1")
        p = os.path.join(d, "main.peg")
        open(p, "w").write(src)
        out = subprocess.run([exe, "run", p], capture_output=True, text=True, timeout=30)
        got = out.stdout.strip() if out.returncode == 0 else ("trap" if out.returncode == 101 else f"error({out.returncode}): {out.stderr.strip()[:200]}")
        if str(got) != str(want):
            bad += 1
            if bad <= 10:
                print(f"MISMATCH {t} {a} {op} {b} ({bt}): model {want}, pegasus {got}")
    print(f"{n} operations, {bad} mismatches")
    sys.exit(1 if bad else 0)

main()
