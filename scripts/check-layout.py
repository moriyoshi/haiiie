#!/usr/bin/env python3
"""ARCHITECTURE.md's layout block must match the tree, in both directions.

A source file absent from the block is a documentation gap; a listed path that
no longer exists is a stale document. Both fail. This is why adding a module
costs a documentation edit -- which is the point, not a side effect.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOC = ROOT / ".agents/docs/ARCHITECTURE.md"
BLOCK = re.compile(r"<!-- BEGIN LAYOUT.*?-->\s*```text\n(.*?)```", re.S)


def listed() -> set[str]:
    m = BLOCK.search(DOC.read_text())
    if not m:
        sys.exit("no LAYOUT block in ARCHITECTURE.md")
    out = set()
    for line in m.group(1).splitlines():
        line = line.strip()
        if line:
            out.add(line.split()[0])
    return out


def main() -> int:
    paths = listed()
    bad = 0

    for p in sorted(paths):
        target = ROOT / p
        if not target.exists():
            print(f"  listed but missing: {p}")
            bad += 1

    # Every Rust source file must be named by some listed path or its directory.
    for src in sorted(ROOT.glob("haiiie-*/src/**/*.rs")):
        rel = src.relative_to(ROOT).as_posix()
        if not any(rel == p or rel.startswith(p.rstrip("/") + "/") for p in paths):
            print(f"  in the tree but not in ARCHITECTURE.md: {rel}")
            bad += 1

    if bad:
        print(f"{bad} layout mismatch(es)")
        return 1
    n = len(list(ROOT.glob("haiiie-*/src/**/*.rs")))
    print(f"  {len(paths)} paths listed, all present; {n} source file(s) covered")
    return 0


if __name__ == "__main__":
    sys.exit(main())
