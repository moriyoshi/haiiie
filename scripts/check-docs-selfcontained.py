#!/usr/bin/env python3
"""docs/ must never reference a source path.

docs/ holds standing, human-facing documents. The tree moves underneath them and
a path written into prose goes stale silently, with nothing to catch it. State
the result, not the location. This is the opposite of the rule for .agents/docs/,
which is supposed to name source.

The baseline exists so the list can only shrink: a new reference fails, and so
does a baselined entry that has been fixed but not removed.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
BASELINE: set[str] = set()
PAT = re.compile(r"\b(haiiie-[a-z]+/(src|tests|benches|examples)/[\w/.-]+|scripts/[\w.-]+)")


def main() -> int:
    if not DOCS.exists():
        print("  docs/ does not exist yet")
        return 0
    found, bad = set(), 0
    for md in sorted(DOCS.rglob("*.md")):
        for i, line in enumerate(md.read_text().splitlines(), 1):
            for m in PAT.finditer(line):
                # The baseline key omits the line number **deliberately**. It
                # used to be `file:line:path`, which made a baselined exception
                # break the moment anything was inserted above it: the entry
                # went stale and the reference reappeared unbaselined, failing
                # twice for an edit that changed nothing. A baseline that breaks
                # when the file is edited is not a baseline, and `QUALITY_GATE.md`
                # promises this list can only shrink. Never fired, because the
                # set has always been empty -- found by injecting a baselined
                # entry and watching it fail anyway.
                #
                # The line still appears in the message, where it helps a reader
                # and costs nothing: what is reported and what is keyed on are
                # different things.
                key = f"{md.relative_to(ROOT)}:{m.group(1)}"
                found.add(key)
                if key not in BASELINE:
                    print(f"  source path in docs/: {md.relative_to(ROOT)}:{i}: {m.group(1)}")
                    bad = 1
    for stale in sorted(BASELINE - found):
        print(f"  baselined but no longer present, remove it: {stale}")
        bad = 1
    if not bad:
        print(f"  docs/ self-contained; {len(BASELINE)} baselined reference(s) remain")
    return bad


if __name__ == "__main__":
    sys.exit(main())
