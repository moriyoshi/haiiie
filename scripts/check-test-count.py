#!/usr/bin/env python3
"""Fail the gate when OVERVIEW.md's test count no longer matches the tree.

# Why this one is mechanizable when two sibling detectors were not

Two attempts this week to mechanize a documentation check failed the same way: a
detector for retracted doc comments ( 16 flagged, 8 false ) and one for backlog
entries naming vanished identifiers ( 14 flagged, 14 false, 0 true ). Both were
strictly worse than reading, because the thing they were looking for was
semantic -- whether a sentence still follows, whether a claim is still true.

A count is not semantic. It is arithmetic, it has exactly one correct value, and
re-deriving it is cheaper than reading the sentence that states it. That is the
line: mechanize the arithmetic, read the judgement.

Prompted by yesno-7c, who found a count in one of their entries that had been
wrong twice -- corrected once, then rotted again -- and drew the conclusion that
correcting a count does not stop it rotting, so it has to be re-derived rather
than trusted.

# The counter is static, and that is a choice with a caveat

Counting `#[test]` and `#[tokio::test]` attributes does not run anything, so the
gate pays nothing for it. Validated against the runner on 2026-09-16: 81
attributes plus 8 async equalled the 89 the runner reported, the whole
discrepancy being that `#[tokio::test]` does not match `#[test]`.

What a static count cannot see: a test behind a `cfg`, and `#[ignore]`, which
the runner reports as ignored rather than passed. Neither exists in this tree
today ( the gate log shows 0 ignored ). If one appears, this script and the
sentence it checks are both about *defined* tests, and that is what the doc
should say.
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DOC = ROOT / ".agents/docs/OVERVIEW.md"


def main() -> int:
    plain = async_ = 0
    files = 0
    for f in sorted(ROOT.glob("haiiie-*/**/*.rs")):
        if "target" in f.parts:
            continue
        text = f.read_text()
        p = len(re.findall(r"#\[test\]", text))
        a = len(re.findall(r"#\[tokio::test\]", text))
        if p or a:
            files += 1
        plain += p
        async_ += a
    total = plain + async_

    doc = DOC.read_text()
    # Every match, not the first. A regex that silently takes match zero looks
    # like it checks a sentence and actually checks whichever one happens to
    # come first -- the same failure as matching a gate step's printed label
    # instead of the script it invokes: an instrument checking something
    # adjacent to its subject, reporting confidently either way.
    ms = list(re.finditer(r"\b(\d+) tests\b", doc))
    if not ms:
        print("  OVERVIEW.md states no test count; nothing to check against")
        return 1
    if len(ms) > 1:
        where = ", ".join(str(doc[: m.start()].count("\n") + 1) for m in ms)
        print(f"  OVERVIEW.md states {len(ms)} test counts (lines {where}); ambiguous")
        print("  This check is anchored to one sentence. Re-anchor it rather than")
        print("  letting it pick whichever match comes first.")
        return 1
    m = ms[0]
    claimed = int(m.group(1))

    print(f"  {total} tests defined ({plain} plain, {async_} async) across {files} file(s)")
    if claimed != total:
        line = doc[: m.start()].count("\n") + 1
        print(f"  .agents/docs/OVERVIEW.md:{line}: claims {claimed}, tree has {total}")
        print("  Re-derive rather than adjust by the difference: a count that was")
        print("  corrected once can rot again, and only the tree is authoritative.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
