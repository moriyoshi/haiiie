#!/usr/bin/env python3
"""Every backlog slug cited from the tree must still be defined somewhere.

# The decay is mechanical, not careless

Consolidating a JOURNAL entry into long-term memory deletes its slug. Every
citation of that slug -- in a module comment, in another document -- survives
untouched, and nothing links the two. So the comment goes on reading as though
the reasoning is written down somewhere, long after it is not. Upstream swept
for this and found 25 dangling citations, two of which were the only surviving
record of a real design gap.

Nothing about a stale citation looks different from a live one, which is why
this is a gate step rather than something review is expected to notice.

# What to do when it fires, in order

Ranked, because the obvious repair is the weakest one and an unranked message
walks a reader straight to it:

1. **Restate the reasoning at the citation site and delete the pointer.** This
   removes the citation from this check's domain rather than satisfying it.

   The evidence is **four of four**, and the sample size matters: upstream
   revisited four sites they had repaired by repointing and found every one
   already carried self-sufficient prose beside the slug, so the pointer was
   pure navigation. That sample is drawn from citations written by people who
   happened to explain themselves, and it is **silent on the harder case** -- a
   citation that is only a pointer, "see `some-slug`" with no argument beside
   it. There the reasoning must be recovered from the entry before it can be
   restated, and if the entry was consolidated away it may not be recoverable.

   The ranking still holds; do not read it as a claim about frequency.
2. **Repoint**, when the reasoning genuinely lives elsewhere and is too long to
   restate.
3. **Restore the entry**, only when the item is genuinely still open. This is
   the weakest fix: it makes the slug resolve without making anything truer.

# What this check structurally cannot do

It fires on a dangling pointer, which is **after** the entry is gone -- exactly
the point at which restating may no longer be possible. The intervention that
would work sits in the consolidation step: check citations *before* removing an
entry, not after. That is a check on a process rather than on a tree, neither
this repository nor upstream has it, and it is recorded here as the shape of the
gap rather than implemented on the strength of four data points.

# Scope, and why the allowlist starts empty

A slug is defined by bolding it in TODO.md or JOURNAL.md, and cited by putting
it in backticks anywhere in the tree. Both example forms are written here
without backticks on purpose: this file is inside the globs it scans, so an
illustrative example would be indistinguishable from a real citation and would
fail the check it documents. Three or more segments,
so ordinary hyphenated vocabulary (`yesno-core`, `arrow-buffer`) is not a
candidate. ALLOWED holds the few multi-segment terms that are genuinely not
slugs; it starts empty and additions should be deliberate, exactly as the
docs-self-contained baseline works.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFINING = [ROOT / ".agents/docs/TODO.md", ROOT / ".agents/docs/JOURNAL.md"]

# Multi-segment hyphenated terms that are vocabulary, not backlog slugs.
#
# Additions are deliberate and each needs a reason. A name here is a name the
# check will never inspect again, so a slug accidentally added would be a
# citation that can dangle forever without anyone hearing about it.
ALLOWED: set[str] = {
    # A crates.io package, named in TODO where it is recorded as a build
    # dependency. Three hyphenated segments, so it looks exactly like a slug.
    "protoc-bin-vendored",
}

DEFINE = re.compile(r"\*\*([a-z][a-z0-9]*(?:-[a-z0-9]+)+)\*\*")
CITE = re.compile(r"`([a-z][a-z0-9]*(?:-[a-z0-9]+){2,})`")


def scanned_files():
    # `scripts/` is in the globs on purpose. It was outside them, and the
    # consequence was that a bad example written into this very file was
    # invisible while the same example in a journal entry died in seconds.
    # Upstream's equivalent check scans its own tooling and that is exactly why
    # the backticked example in its exclusion comment was caught immediately --
    # same class of file, opposite configuration, and the stricter one caught
    # its author. A check that cannot see its own source is asking to be the
    # one place the rule does not apply.
    for pat in ("haiiie-*/src/**/*.rs", "haiiie-*/tests/**/*.rs",
                ".agents/docs/**/*.md", "scripts/**/*.py"):
        yield from sorted(ROOT.glob(pat))


def main() -> int:
    defined = set()
    for d in DEFINING:
        if d.exists():
            defined |= set(DEFINE.findall(d.read_text()))

    dangling, cited = [], 0
    for f in scanned_files():
        for i, line in enumerate(f.read_text().splitlines(), 1):
            for slug in CITE.findall(line):
                if slug in ALLOWED:
                    continue
                cited += 1
                if slug not in defined:
                    dangling.append(f"{f.relative_to(ROOT)}:{i}: `{slug}`")

    # A backticked three-segment name is ambiguous between a backlog slug and a
    # script stem: the stems of this script and the CI-workflow one both parse
    # as citations and both dangle, and the generic message then offers three
    # fixes of which none is the right one. (Named in prose rather than in
    # backticks, for the reason in the docstring.) Derived from the directory rather than a
    # hand-kept list, because a hand-kept list is exactly what goes stale and
    # this check exists because of that failure. Reported rather than excluded:
    # naming a script with its extension is the convention everywhere else in
    # the tree, so the citation is still wrong, just wrong in a different way.
    stems = {f.stem for f in (ROOT / "scripts").glob("*.py")}
    script_named = [d for d in dangling if d.rsplit("`", 2)[-2] in stems]
    real = [d for d in dangling if d not in script_named]
    for d in real:
        print(f"  cited but not defined in TODO.md or JOURNAL.md: {d}")
    for d in script_named:
        print(f"  names a script, not a backlog slug: {d} -- write it as `<stem>.py`")
    if dangling:
        if not real:
            print(f"{len(script_named)} script name(s) written as a slug")
            return 1
        print(f"{len(real)} dangling citation(s)")
        print("  Fixes, best first:")
        print("    1. restate the reasoning at the citation site, delete the pointer")
        print("    2. repoint, if the reasoning is real and too long to restate")
        print("    3. restore the entry, only if the item is genuinely still open")
        return 1
    print(f"  {cited} slug citation(s), all defined; {len(defined)} slug(s) on record")
    return 0


if __name__ == "__main__":
    sys.exit(main())
