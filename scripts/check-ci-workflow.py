#!/usr/bin/env python3
"""Check the assumptions the CI workflow makes about its own checkout layout.

Why this is a gate step rather than something CI would tell us
--------------------------------------------------------------

CI cannot tell us. The workflow has never executed, and both of the defects
this check encodes were found by reading it rather than by running it:

* ``path: ../yesno`` on ``actions/checkout``. The action refuses a path that
  escapes the workspace, so *both jobs* would have failed at their second step,
  every time, for as long as the file existed.
* a global ``RUSTFLAGS: -D warnings``. ``RUSTFLAGS`` reaches path dependencies --
  verified, not assumed: building ``haiiie-core`` under ``-D missing_docs``
  fails inside ``yesno-core``. So it made this repository's CI depend on another
  repository staying warning-free, while adding nothing for our own code, which
  ``cargo clippy --workspace -- -D warnings`` already covers.

The second is the more interesting failure mode, because it would not have
announced itself as a mistake: CI would simply have gone red one day for a
reason nobody here could fix.

What is actually checked
------------------------

The load-bearing invariants are that **the manifest's path dependency and the
workflow's checkout paths have to agree** and that both jobs check out the
exact SHA in `YESNO_REVISION`. The script checks both rather than trusting
the copied values to stay synchronized. A crate at
``haiiie/haiiie-core/`` reaching ``../../yesno/yesno-core`` requires this
repository checked out one level down and the storage engine beside it under
that exact directory name. Rename either and the build breaks in CI only.

Deliberately no YAML parser: the checks are simple enough for text, and a gate
step that needs a library the runner might not have is a step that can fail for
reasons unrelated to what it checks.
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github/workflows/ci.yml"
MANIFEST = ROOT / "haiiie-core/Cargo.toml"
PIN = ROOT / "YESNO_REVISION"


def main() -> int:
    problems: list[str] = []
    text = WORKFLOW.read_text()
    pinned_rev = PIN.read_text().strip()
    # Comments carry the reasoning for these very rules; checking them would
    # make the explanation trip the check it explains.
    raw = text
    body = "\n".join(l for l in text.splitlines() if not l.lstrip().startswith("#"))

    dep = re.search(r'yesno-core\s*=\s*\{\s*path\s*=\s*"([^"]+)"', MANIFEST.read_text())
    if dep is None:
        return fail(["haiiie-core/Cargo.toml has no yesno-core path dependency"])
    parts = dep.group(1).split("/")
    if parts[:2] != ["..", ".."]:
        problems.append(
            f"the path dependency is {dep.group(1)!r}; this check understands only "
            "the '../../<sibling>/...' form and must be updated with it"
        )
    else:
        sibling = parts[2]
        # **Per job, not per file.** Checking that the layout appears *somewhere*
        # passes a workflow where one job has it and another does not, which a
        # sabotage found: renaming the checkout in a single job left the other
        # job's copy to satisfy a whole-file substring search. Every job that
        # builds has to stand up the layout for itself.
        for name, block in jobs(body).items():
            if "cargo " not in block and "gate.sh" not in block:
                continue
            # **Exact values, not substrings.** `path: haiiie` is a substring of
            # `path: haiiie-renamed`, and `path: yesno` of `path: yesnodb`, so a
            # containment test silently accepts exactly the renames this exists
            # to catch. A sabotage found it twice before the third form stuck.
            have = set(re.findall(r"^\s*path:\s*(\S+)\s*$", block, re.M))
            for want, why in (
                (sibling, f"the manifest depends on a sibling checkout named {sibling!r}"),
                (ROOT.name, "the manifest's '../..' requires this repository one level down"),
            ):
                if want not in have:
                    problems.append(
                        f"job {name!r}: {why}, and this job checks out to "
                        f"{sorted(have) or 'nothing'} instead of including {want!r}"
                    )

            # The checkout step, not just the job, must carry the exact pin.
            # A stray matching SHA elsewhere in a job must not certify yesno.
            checkout_steps = [
                step
                for step in re.split(r"(?m)^      - ", block)
                if re.search(r"^\s*repository:\s*moriyoshi/yesnodb\s*$", step, re.M)
            ]
            if len(checkout_steps) != 1:
                problems.append(
                    f"job {name!r}: expected one yesnodb checkout step, "
                    f"found {len(checkout_steps)}"
                )
            else:
                refs = re.findall(r"^\s*ref:\s*(\S+)\s*$", checkout_steps[0], re.M)
                if refs != [pinned_rev]:
                    problems.append(
                        f"job {name!r}: yesnodb checkout ref is {refs or 'absent'}, "
                        f"expected {pinned_rev}"
                    )

    for n, line in enumerate(body.splitlines(), 1):
        if re.search(r"path:\s*\.\.", line):
            problems.append(
                f"line {n}: a checkout path escaping the workspace "
                "(actions/checkout refuses these, so the job cannot run)"
            )

    if re.search(r"^\s*RUSTFLAGS\s*:", body, re.M):
        problems.append(
            "a global RUSTFLAGS is set; it reaches path dependencies, so it makes "
            "this repository's CI depend on yesnodb staying warning-free. Lint our "
            "own crates through clippy in the gate instead"
        )

    # Every cargo or gate invocation runs from the repository's subdirectory.
    for block in re.split(r"\n\s*-\s", body):
        if re.search(r"run:.*(cargo |gate\.sh)", block) and "working-directory:" not in block:
            what = re.search(r"run:\s*(.+)", block)
            problems.append(
                f"step {what.group(1).strip() if what else '?'!r} runs from the "
                "workspace root, where there is no manifest"
            )

    if problems:
        return fail(problems)
    # Say which lines, not just how many. `body` has comment lines stripped, so
    # "54 lines checked" against a 77-line file invites reading it as full
    # coverage when 23 lines were never examined -- and a commented-out step or
    # a `# yamllint disable` directive lives precisely there. The number was
    # always true; the sentence let a reader infer something it did not check.
    total = len(raw.splitlines())
    print(
        f"  CI workflow agrees with the manifest's sibling layout; "
        f"{len(body.splitlines())} of {total} lines checked (comments not examined)"
    )
    return 0


def jobs(body: str) -> dict[str, str]:
    """Split the `jobs:` mapping into one text block per job.

    A two-space-indented `name:` under `jobs:` starts a job; anything more
    indented belongs to it. Crude, and adequate: the alternative is a YAML
    parser, and a gate step that needs a library the runner might not ship is a
    step that can fail for reasons unrelated to what it checks.
    """
    out: dict[str, list[str]] = {}
    current: str | None = None
    in_jobs = False
    for line in body.splitlines():
        if re.match(r"^jobs:\s*$", line):
            in_jobs = True
            continue
        if not in_jobs:
            continue
        if line.strip() and not line.startswith(" "):
            break
        m = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if m:
            current = m.group(1)
            out[current] = []
        elif current is not None:
            out[current].append(line)
    return {k: "\n".join(v) for k, v in out.items()}


def fail(problems: list[str]) -> int:
    for p in problems:
        print(f"  {p}")
    print(f"{len(problems)} problem(s) in .github/workflows/ci.yml")
    return 1


if __name__ == "__main__":
    sys.exit(main())
