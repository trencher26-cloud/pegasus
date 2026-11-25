#!/usr/bin/env python3
"""Verify that byte-sensitive files survived git checkout unaltered.

Run in CI after checkout, on every platform. The Windows job checks out with
core.autocrlf=true, so this also proves that .gitattributes keeps the corpus
cases and the golden files byte-exact (the R6.1 defect).
"""
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parent.parent
LEX = ROOT / "tests/grammar/cases/lexical"
CHECKS = {
    "L-014_crlf_file.peg": lambda b: b.count(b"\r\n") >= 4,
    "L-018_lone_cr.peg": lambda b: b"\r " in b,
    "L-016_byte_order_mark.peg": lambda b: b.startswith(b"\xef\xbb\xbf"),
    "L-017_invalid_utf8.peg": lambda b: b"\xff" in b,
    "L-030_raw_string_crlf.peg": lambda b: b'r"one\r\ntwo"' in b,
    "L-029_raw_string_lf.peg": lambda b: b"\r" not in b and b'r"one\ntwo"' in b,
    "L-031_lone_cr_in_raw_string.peg": lambda b: b'r"one\rtwo"' in b,
    "L-032_lone_cr_in_block_comment.peg": lambda b: b"/* one\rtwo */" in b,
}


def main() -> int:
    bad = [n for n, ok in CHECKS.items() if not ok((LEX / n).read_bytes())]
    # Cases without a deliberate CR must have none: a checkout that converted
    # LF to CR LF would shift every byte offset in the corpus.
    cr_ok = {"L-014_crlf_file.peg", "L-018_lone_cr.peg", "L-030_raw_string_crlf.peg",
             "L-031_lone_cr_in_raw_string.peg", "L-032_lone_cr_in_block_comment.peg"}
    for p in sorted((ROOT / "tests/grammar/cases").rglob("*.peg")):
        if p.name not in cr_ok and b"\r" in p.read_bytes():
            bad.append(str(p.relative_to(ROOT)))
    # Golden files are LF-only text with explicit escapes; CR means conversion.
    goldens = ROOT / "tests/grammar/goldens/candidates"
    for p in sorted(goldens.iterdir()):
        if p.suffix in (".tokens", ".filtered") and b"\r" in p.read_bytes():
            bad.append(str(p.relative_to(ROOT)))
    if bad:
        print("bytes were altered on checkout:", *bad, sep="\n  ")
        return 1
    print("corpus and golden bytes intact")
    return 0


if __name__ == "__main__":
    sys.exit(main())
