#!/usr/bin/env python3
"""Require every yesno path dependency to come from the pinned clean checkout."""

from __future__ import annotations

import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PIN_FILE = ROOT / "YESNO_REVISION"
YESNO = (ROOT / "../yesno").resolve()


def git(*args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(YESNO), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or f"git {' '.join(args)} failed")
    return result.stdout.strip()


def yesno_dependencies(value: object):
    if isinstance(value, dict):
        for name, item in value.items():
            if re.fullmatch(r"yesno-[a-z0-9-]+", name) and isinstance(item, dict):
                yield name, item
            else:
                yield from yesno_dependencies(item)
    elif isinstance(value, list):
        for item in value:
            yield from yesno_dependencies(item)


def main() -> int:
    problems: list[str] = []
    pin = PIN_FILE.read_text().strip()
    if not re.fullmatch(r"[0-9a-f]{40}", pin):
        print("  YESNO_REVISION must contain one full lowercase commit SHA")
        return 1

    checked = 0
    for manifest in sorted(ROOT.glob("haiiie-*/Cargo.toml")):
        with manifest.open("rb") as file:
            document = tomllib.load(file)
        for name, dependency in yesno_dependencies(document):
            checked += 1
            path = dependency.get("path")
            expected = YESNO / name
            actual = (manifest.parent / path).resolve() if isinstance(path, str) else None
            if actual != expected:
                problems.append(
                    f"{manifest.relative_to(ROOT)}: {name} must resolve to "
                    f"{expected}, got {actual or dependency}"
                )
    if checked == 0:
        problems.append("no yesno dependencies were found; inspect the manifests")

    try:
        root = Path(git("rev-parse", "--show-toplevel")).resolve()
        head = git("rev-parse", "HEAD")
        dirty = git("status", "--porcelain", "--untracked-files=normal")
    except RuntimeError as error:
        problems.append(str(error))
    else:
        if root != YESNO:
            problems.append(f"the sibling yesno path resolves to repository {root}")
        if head != pin:
            problems.append(f"yesno HEAD is {head}, pinned revision is {pin}")
        if dirty:
            problems.append("yesno working tree is dirty; path dependencies may differ from the pinned commit")

    if problems:
        for problem in problems:
            print(f"  {problem}")
        return 1
    print(f"  {checked} yesno path dependencies use clean revision {pin[:7]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
