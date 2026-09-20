#!/usr/bin/env python3
"""haiiie-core's direct-dependency budget.

A budget is only a budget if CI says so. haiiie-core reaches arrow_buffer types
through yesno_core::unstable_arrow's re-export precisely so that it need not
depend on Arrow itself; that only stays true if something refuses the shortcut.
"""
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUDGET = 2
FORBIDDEN = ("tokio", "tonic", "arrow-flight", "arrow-array", "prost")


def main() -> int:
    if not (ROOT / "haiiie-core/Cargo.toml").exists():
        print("  skipped: haiiie-core does not exist yet")
        return 0
    try:
        out = subprocess.run(
            ["cargo", "tree", "-p", "haiiie-core", "-e", "normal", "--depth", "1"],
            cwd=ROOT, capture_output=True, text=True, check=True,
        ).stdout
    except (subprocess.CalledProcessError, FileNotFoundError) as e:
        print(f"  could not run cargo tree: {e}")
        return 1

    deps = [ln for ln in out.splitlines()[1:] if ln.strip()]
    bad = 0
    if len(deps) > BUDGET:
        print(f"  haiiie-core has {len(deps)} direct dependencies, budget is {BUDGET}:")
        for d in deps:
            print(f"    {d}")
        bad = 1
    for name in FORBIDDEN:
        if any(name in d for d in deps):
            print(f"  forbidden direct dependency in haiiie-core: {name}")
            bad = 1
    if not bad:
        print(f"  haiiie-core: {len(deps)}/{BUDGET} direct dependencies, none forbidden")
    return bad


if __name__ == "__main__":
    sys.exit(main())
