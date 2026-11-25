#!/usr/bin/env python3
"""Regenerate spec/pegasus.ebnf from a Markdown export of the grammar specification.

    python3 tools/extract_ebnf.py spec/grammar.md            # regenerate spec/pegasus.ebnf
    python3 tools/extract_ebnf.py --check spec/grammar.md    # fail if the checked-in file drifted

Every ```ebnf block is copied in document order under a comment naming its
section. Fails if a production is defined twice. Run this in the same change as
any grammar edit, then `tools/corpus.py coverage`.
"""
import re
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HEADER = """(* Pegasus grammar {version} - machine-readable copy.
   Extracted by tools/extract_ebnf.py from the grammar specification document.
   The document stays normative for the newline filter (section 3), ambiguity
   rules (section 7) and error recovery (section 8). CI checks that every
   production here is mapped in tests/grammar/coverage.toml. *)
"""


def main() -> int:
    args = sys.argv[1:]
    check = "--check" in args
    args = [a for a in args if a != "--check"]
    md = Path(args[0]).read_text(encoding="utf-8")
    version = "v0.1.1 (draft)"
    out = [HEADER.format(version=version)]
    h2 = h3 = None
    for m in re.finditer(r"^## ([^\n]+)$|^### ([^\n]+)$|```ebnf\n(.*?)```", md, re.M | re.S):
        if m.group(1):
            h2, h3 = m.group(1).strip(), None
            continue
        if m.group(2):
            h3 = m.group(2).strip()
            continue
        label = h3 if h3 and h3[0].isdigit() else (f"{h2} / {h3}" if h3 else h2)
        out.append(f"(* ===== {label} ===== *)")
        out.append(m.group(3).rstrip())
        out.append("")
    text = "\n".join(out) + "\n"
    names = re.findall(r"^([A-Za-z_][A-Za-z0-9_]*)(?:\[S\])?\s+::=", text, re.M)
    dupes = [n for n, c in Counter(names).items() if c > 1]
    if dupes:
        sys.exit(f"duplicate production definitions: {dupes}")
    target = ROOT / "spec" / "pegasus.ebnf"
    if check:
        current = target.read_text(encoding="utf-8") if target.exists() else ""
        if current != text:
            import difflib
            diff = difflib.unified_diff(current.splitlines(), text.splitlines(),
                                        "spec/pegasus.ebnf (checked in)", "regenerated from spec/grammar.md", lineterm="")
            print("\n".join(list(diff)[:40]))
            sys.exit("spec/pegasus.ebnf is out of sync with spec/grammar.md; run tools/extract_ebnf.py spec/grammar.md")
        print(f"spec/pegasus.ebnf is in sync ({len(names)} productions)")
        return 0
    target.write_text(text, encoding="utf-8")
    print(f"wrote spec/pegasus.ebnf: {len(names)} productions")
    return 0


if __name__ == "__main__":
    sys.exit(main())
