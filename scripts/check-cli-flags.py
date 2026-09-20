#!/usr/bin/env python3
"""Fail the gate when a CLI long flag is not mentioned anywhere in `docs/`.

# Why this is a check and not a reading task

It is a set difference: the long flags the binaries accept, against the strings
`docs/` contains. Exact, one correct answer, cheaper to re-derive than to read
-- the same test that earned the test-count check a gate step, and the same one
that refused it to two semantic detectors that were withdrawn.

It earned its place immediately. `--threads` was added to the server and very
nearly shipped undocumented in the same session, and `--server` -- the flag that
says which server the client talks to -- had never been documented at all,
because every example in the guides relies on the default and the default
happens to match the server's.

# What it does NOT check, stated because the name oversells it

That a flag is **mentioned**, not that it is documented. A flag named once in an
unrelated code block passes. Nothing here can tell an explanation from an
occurrence, and pretending otherwise would make this a step that stops meaning
anything.

Nor does it check the reverse direction. A doc naming a flag that no longer
exists is the other half of this rot and is not caught here: `--foo` in prose is
indistinguishable from `--foo` in an example for a different tool, and the false
positives would outnumber the findings. Read for that one.
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
ARG = re.compile(r"#\[arg\(long[^\)]*\)\]\s*\n\s*([a-z_][a-z0-9_]*)\s*:")


def main() -> int:
    flags: dict[str, set[str]] = {}
    for f in sorted((ROOT / "haiiie-cli/src").rglob("*.rs")):
        for m in ARG.finditer(f.read_text()):
            flags.setdefault(m.group(1).replace("_", "-"), set()).add(f.name)
    if not flags:
        print("  no CLI long flags found; the extractor has stopped matching")
        return 1

    docs = "".join(p.read_text() for p in sorted((ROOT / "docs").glob("*.md")))
    missing = sorted(n for n in flags if f"--{n}" not in docs)

    print(f"  {len(flags)} CLI long flag(s), {len(flags) - len(missing)} mentioned in docs/")
    if missing:
        for n in missing:
            print(f"  not mentioned in docs/: --{n}  ({', '.join(sorted(flags[n]))})")
        print("  A flag a user cannot discover is a feature that is not shipped.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
