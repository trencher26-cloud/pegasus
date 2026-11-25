#!/usr/bin/env python3
"""Pegasus grammar corpus tool.

Validates the source corpus under tests/grammar/cases and computes the UTF-8
byte offset of each expected first error from its `near` locator.

    python3 tools/corpus.py check         # validate every case; exit 1 on any problem
    python3 tools/corpus.py fix-offsets   # (re)compute offset= from near= and rewrite headers
    python3 tools/corpus.py stats         # counts by directory, outcome and tag
    python3 tools/corpus.py coverage      # grammar production -> case matrix and gaps

Header format (lines at the very top of a .peg file, each starting with "//@ "):

    //@ id: E-003
    //@ tags: precedence, expr.power
    //@ expect: accept | reject
    //@ shape: (** 2 (** 3 2))            optional; S-expression of the body of `fn t`
    //@ stmts: 2                          optional; statement count in the body of `fn t`
    //@ value: A = "one\\ntwo"             optional; value of the string literal in `const A`
    //@ error: P002 offset=57 near=">"    required for reject; near may end in #N (Nth match)
    //@ warning: W001 offset=91 near="x"  optional, repeatable; every expected warning, exactly
    //@ nl-kept: offset=96 near="\\n    { y }"   optional, repeatable; this NEWLINE must survive the filter
    //@ nl-dropped: offset=40 near="\\n    .x"   optional, repeatable; this NEWLINE must be dropped
    //@ note: free text                   optional, repeatable

`offset` is the UTF-8 byte offset (0-based, from the start of the file) of the
first byte of the token where the first error is reported. `near` is the exact
source text at that position, searched for after the header; escapes \\r \\n
\\t \\\\ \\" \\xHH and \\u{XXXX} are allowed inside it.
"""
from __future__ import annotations

import re
import sys
import tomllib
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "tests" / "grammar"
CASES = CORPUS / "cases"
CATALOGUE = CORPUS / "diagnostics.toml"

PREFIX_BY_DIR = {
    "phase0": "P0",
    "lexical": "L",
    "newline": "N",
    "precedence": "E",
    "ambiguity": "A",
    "items": "I",
    "recovery": "R",
    "parser": "S",
}
HEADER_RE = re.compile(rb"^//@ ([a-z-]+): ?(.*)$")
NL_RE = re.compile(r'^(?:offset=(\d+) )?near="((?:[^"\\]|\\.)*)"(?:#(\d+))?$')
LOCATED_RE = re.compile(r'^([LPW]\d{3})(?: offset=(\d+))? near="((?:[^"\\]|\\.)*)"(?:#(\d+))?$')


def unescape(s: str) -> bytes:
    out = bytearray()
    i = 0
    while i < len(s):
        c = s[i]
        if c != "\\":
            out += c.encode("utf-8")
            i += 1
            continue
        nxt = s[i + 1]
        if nxt == "u":
            end = s.index("}", i)
            out += chr(int(s[i + 3:end], 16)).encode("utf-8")
            i = end + 1
            continue
        if nxt == "x":
            out.append(int(s[i + 2:i + 4], 16))
            i += 4
            continue
        out += {"r": b"\r", "n": b"\n", "t": b"\t", "\\": b"\\", '"': b'"'}[nxt]
        i += 2
    return bytes(out)


def split_header(data: bytes) -> tuple[list[tuple[str, str, int]], int]:
    """Return ([(key, value, line_start_offset)], header_end_offset)."""
    entries = []
    pos = 3 if data.startswith(b"\xef\xbb\xbf") else 0
    while pos < len(data):
        end = data.find(b"\n", pos)
        end = len(data) if end == -1 else end
        line = data[pos:end].rstrip(b"\r")
        m = HEADER_RE.match(line)
        if not m:
            break
        entries.append((m.group(1).decode(), m.group(2).decode("utf-8"), pos))
        pos = end + 1
    return entries, pos


def balanced(s: str) -> bool:
    if "(" not in s and ")" not in s:
        return bool(s) and " " not in s
    depth = 0
    for ch in s:
        depth += ch == "("
        depth -= ch == ")"
        if depth < 0:
            return False
    return depth == 0 and s.startswith("(")


def locate(data: bytes, start: int, near: bytes, nth: int) -> list[int]:
    hits, pos = [], start
    while True:
        i = data.find(near, pos)
        if i == -1:
            return hits
        hits.append(i)
        pos = i + 1


def load_codes() -> set[str]:
    with open(CATALOGUE, "rb") as f:
        cat = tomllib.load(f)
    return set(cat["lexical"]) | set(cat["parser"]) | set(cat.get("warnings", {}))


def process(fix: bool) -> int:
    codes = load_codes()
    problems: list[str] = []
    seen: dict[str, Path] = {}
    files = sorted(CASES.rglob("*.peg"))
    for path in files:
        rel = path.relative_to(ROOT)
        data = path.read_bytes()
        entries, body_start = split_header(data)
        keys = Counter(k for k, _, _ in entries)
        get = {k: v for k, v, _ in entries}
        def bad(msg: str) -> None:
            problems.append(f"{rel}: {msg}")
        for k in ("id", "tags", "expect"):  # "warning" may repeat; others at most once
            if keys[k] != 1:
                bad(f"needs exactly one '{k}' line")
        unknown = set(keys) - {"id", "tags", "expect", "shape", "stmts", "error", "warning", "nl-kept", "nl-dropped", "note", "value"}
        if unknown:
            bad(f"unknown header keys {sorted(unknown)}")
        cid = get.get("id", "")
        prefix = PREFIX_BY_DIR.get(path.parent.name)
        if prefix is None:
            bad(f"unknown case directory '{path.parent.name}'")
        elif not re.fullmatch(rf"{prefix}-\d{{3}}", cid):
            bad(f"id '{cid}' must look like {prefix}-NNN")
        if cid in seen:
            bad(f"duplicate id {cid} (also {seen[cid].relative_to(ROOT)})")
        seen[cid] = path
        if path.stem.split("_")[0] != cid:
            bad(f"file name must start with its id '{cid}_'")
        if not [t for t in get.get("tags", "").split(",") if t.strip()]:
            bad("tags must not be empty")
        expect = get.get("expect")
        if expect not in ("accept", "reject"):
            bad("expect must be 'accept' or 'reject'")
        if "shape" in get and not balanced(get["shape"]):
            bad("shape is not a balanced S-expression")
        if "stmts" in get and not get["stmts"].isdigit():
            bad("stmts must be an integer")
        if "value" in get and not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]* = "(?:[^"\\]|\\.)*"', get["value"]):
            bad('value must look like NAME = "escaped text"')
        if expect == "accept" and "error" in get:
            bad("accepted case must not have an error line")
        if expect == "reject" and keys["error"] != 1:
            bad("rejected case needs exactly one error line")
            continue
        # Locate every error/warning line; compute (and in fix mode, rewrite) offsets.
        located = [(k, v, start) for k, v, start in entries if k in ("error", "warning", "nl-kept", "nl-dropped")]
        rewrites = []
        for k, v, start in located:
            is_nl = k.startswith("nl-")
            m = (NL_RE if is_nl else LOCATED_RE).match(v)
            if not m:
                bad(f"malformed {k} line: {v}")
                continue
            if is_nl:
                code, (offset, near_raw, nth) = None, (m.group(1), m.group(2), int(m.group(3) or 1))
                if not near_raw.startswith("\\n") and not near_raw.startswith("\\r\\n"):
                    bad(f"{k}: near must start at the line end it asserts about (\\n or \\r\\n)")
            else:
                code, offset, near_raw, nth = m.group(1), m.group(2), m.group(3), int(m.group(4) or 1)
                want_prefix = "W" if k == "warning" else "LP"
                if code[0] not in want_prefix:
                    bad(f"{k} line has a {code[0]} code; errors use L/P codes, warnings W codes")
                if code not in codes:
                    bad(f"{k} code {code} is not in diagnostics.toml")
            near = unescape(near_raw)
            hits = locate(data, body_start, near, nth)
            if len(hits) < nth:
                bad(f"near={near_raw!r} occurs {len(hits)} time(s) after the header; need #{nth}")
                continue
            if nth == 1 and len(hits) > 1 and (m.group(3) if is_nl else m.group(4)) is None:
                bad(f"near={near_raw!r} is ambiguous ({len(hits)} matches); add #N")
                continue
            computed = hits[nth - 1]
            suffix = f"#{nth}" if (m.group(3) if is_nl else m.group(4)) else ""
            new_line = (f'//@ {k}: offset={computed} near="{near_raw}"' if is_nl
                        else f'//@ {k}: {code} offset={computed} near="{near_raw}"') + suffix
            rewrites.append((start, new_line))
            if not fix:
                if offset is None:
                    bad("offset missing; run fix-offsets")
                elif int(offset) != computed:
                    bad(f"offset={offset} but near={near_raw!r} is at {computed}")
        if fix and rewrites:
            for start, new_line in sorted(rewrites, reverse=True):   # back to front keeps starts valid
                end = data.index(b"\n", start)
                cr = data[end - 1:end] == b"\r"
                data = data[:start] + new_line.encode() + (b"\r" if cr else b"") + data[end:]
            path.write_bytes(data)
    if problems:
        print("\n".join(problems))
        print(f"\n{len(problems)} problem(s) in {len(files)} case(s)")
        return 1
    print(f"OK: {len(files)} case(s), {len(seen)} unique id(s)")
    return 0


def stats() -> int:
    by_dir, by_expect, by_tag = Counter(), Counter(), Counter()
    for path in sorted(CASES.rglob("*.peg")):
        entries, _ = split_header(path.read_bytes())
        get = {k: v for k, v, _ in entries}
        by_dir[path.parent.name] += 1
        by_expect[get.get("expect", "?")] += 1
        for t in get.get("tags", "").split(","):
            if t.strip():
                by_tag[t.strip().split(".")[0]] += 1
    for title, c in (("directory", by_dir), ("outcome", by_expect), ("tag family", by_tag)):
        print(f"By {title}:")
        for k, v in sorted(c.items()):
            print(f"  {k:<14} {v}")
    return 0


COVERAGE = CORPUS / "coverage.toml"
NOTATION = {"list"}   # grammar notation macros (section 1), not productions


def grammar_problems(ebnf: str) -> list[str]:
    """Duplicate definitions and undefined symbols in spec/pegasus.ebnf."""
    text = re.sub(r"\(\*.*?\*\)", " ", ebnf, flags=re.S)          # comments
    head_re = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)(?:\[S\])?\s+::=", re.M)
    names = head_re.findall(text)
    out = [f"production '{n}' is defined {c} times in spec/pegasus.ebnf"
           for n, c in Counter(names).items() if c > 1]
    body = re.sub(r'"(?:[^"\\]|\\.)*"|\'(?:[^\'\\]|\\.)*\'', " ", text)  # terminals
    body = re.sub(r"\[\^[^\]]*\]", " ", body)                     # character classes
    body = re.sub(r"\[[+~?]?S\]", " ", body)                        # [S] parameters
    body = head_re.sub(" ", body)
    used = set(re.findall(r"[A-Za-z_][A-Za-z0-9_]*", body))
    out += [f"symbol '{u}' is used but not defined in spec/pegasus.ebnf"
            for u in sorted(used - set(names) - NOTATION)]
    return out


def coverage() -> int:
    """Check the production -> case matrix: every listed ID exists, every case
    is used somewhere, and report productions or rules below their targets."""
    with open(COVERAGE, "rb") as f:
        cov = tomllib.load(f)
    ids: dict[str, str] = {}
    for path in sorted(CASES.rglob("*.peg")):
        entries, _ = split_header(path.read_bytes())
        get = {k: v for k, v, _ in entries}
        ids[get.get("id", "")] = get.get("expect", "?")
    problems, used = [], set()
    rows = []
    ebnf = (ROOT / "spec" / "pegasus.ebnf").read_text(encoding="utf-8")
    problems.extend(grammar_problems(ebnf))
    productions = set(re.findall(r"^([A-Za-z_][A-Za-z0-9_]*)(?:\[S\])?\s+::=", ebnf, re.M))
    mapped = set(cov["lexical"]) | set(cov["syntax"])
    for name in sorted(productions - mapped):
        problems.append(f"production '{name}' in spec/pegasus.ebnf is missing from [lexical]/[syntax]")
    for name in sorted(mapped - productions):
        problems.append(f"'{name}' is not a production in spec/pegasus.ebnf; move it to [categories]")
    for section in ("lexical", "filter", "syntax", "categories", "rules"):
        target = cov["targets"][section]
        for name, case_ids in cov[section].items():
            dup = sorted({c for c in case_ids if case_ids.count(c) > 1})
            if dup:
                problems.append(f"{section}.{name}: case id(s) listed more than once {dup}")
            case_ids = list(dict.fromkeys(case_ids))
            missing = [c for c in case_ids if c not in ids]
            if missing:
                problems.append(f"{section}.{name}: unknown case id(s) {missing}")
            used.update(case_ids)
            pos = sum(1 for c in case_ids if ids.get(c) == "accept")
            neg = sum(1 for c in case_ids if ids.get(c) == "reject")
            rows.append((section, name, pos, neg, target))
    unused = sorted(set(ids) - used)
    width = max(len(n) for _, n, *_ in rows)
    print(f"{'production':<{width + 9}} pos  neg  target")
    gaps = 0
    for section, name, pos, neg, target in rows:
        flag = ""
        if pos + neg == 0:
            flag = "  NO CASES"
        elif pos + neg < target:
            flag = f"  below target ({target - pos - neg} more)"
        gaps += bool(flag)
        print(f"{section + '.' + name:<{width + 9}} {pos:>3}  {neg:>3}  {target:>4}{flag}")
    print(f"\n{len(productions)} grammar productions ({len(productions & mapped)} mapped); "
          f"{len(rows)} entries in all; {gaps} below target; {len(ids)} cases, {len(unused)} not mapped")
    if unused:
        problems.append(f"cases not mapped to any production: {unused}")
    zero = [n for _, n, p, q, _ in rows if p + q == 0]
    if zero:
        problems.append(f"productions with no cases: {zero}")
    if problems:
        print("\n".join(problems))
        return 1
    return 0


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "check"
    if cmd == "check":
        sys.exit(process(fix=False))
    if cmd == "fix-offsets":
        # Writing offset=N changes the header length and so shifts the body;
        # repeat until every stored offset matches (converges in 2-3 rounds).
        import contextlib, io
        for _ in range(6):
            with contextlib.redirect_stdout(io.StringIO()):
                process(fix=True)
                if process(fix=False) == 0:
                    break
        sys.exit(process(fix=False))
    if cmd == "stats":
        sys.exit(stats())
    if cmd == "coverage":
        sys.exit(coverage())
    sys.exit(f"unknown command {cmd!r}")
