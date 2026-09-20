#!/usr/bin/env bash
#
# Every check, in one command.
#
#   ./scripts/gate.sh          # the routine gate
#
# ---------------------------------------------------------------------------
# Why the step counter exists
# ---------------------------------------------------------------------------
#
# `set -uo pipefail` deliberately does not abort on the first failure: the gate
# reports *every* failing check rather than stopping at one. The cost of that
# choice is that a run which dies partway through still has `fail` at 0 and
# still reaches no verdict of its own -- so it can exit 0 having run half of
# itself. Upstream hit exactly this. A gate that can pass without running is
# worse than no gate, so the run counts its steps and refuses a short count.
#
# Adopted from yesnodb's scripts/gate.sh.
set -uo pipefail
cd "$(dirname "$0")/.."

fail=0
steps_run=0
# How many `step` calls the run must reach. **Update when adding or removing a
# step** -- a mismatch is reported, not silently tolerated.
EXPECT_STEPS=11

step() {
    steps_run=$((steps_run + 1))
    printf '\n\033[1m== %s\033[0m\n' "$1"
}

check() {
    if "$@"; then
        printf '   ok\n'
    else
        printf '\033[31m   FAILED\033[0m\n'
        fail=1
    fi
}

verdict() {
    local want=$1
    if [[ $steps_run -ne $want ]]; then
        printf '\n\033[31m   INCOMPLETE: ran %d of %d steps\033[0m\n' "$steps_run" "$want"
        printf '   The run stopped early. Its verdict below covers only what ran.\n'
        fail=1
    fi
    [[ $fail -eq 0 ]] && printf '\n\033[32mgate passed\033[0m\n' || printf '\n\033[31mgate failed\033[0m\n'
    exit $fail
}

have_crates() { [[ -n "$(find . -name Cargo.toml -not -path './.agents-workspace/*' -print -quit)" ]]; }

step "yesno revision and clean path dependencies"
check python3 scripts/check-yesno-revision.py

step "rustfmt"
if have_crates; then check cargo fmt --check; else printf '   skipped: no crate yet\n'; fi

step "clippy (-D warnings, all targets, all features, whole workspace)"
if have_crates; then
    check cargo clippy --workspace --all-targets --all-features -- -D warnings
else printf '   skipped: no crate yet\n'; fi

step "tests (workspace)"
if have_crates; then check cargo test --workspace; else printf '   skipped: no crate yet\n'; fi

step "ARCHITECTURE.md layout matches the tree"
check python3 scripts/check-layout.py

step "haiiie-core's dependency budget"
check python3 scripts/check-deps.py

step "docs/ is self-contained"
check python3 scripts/check-docs-selfcontained.py

step "OVERVIEW.md's test count matches the tree"
check python3 scripts/check-test-count.py

step "every CLI flag is mentioned in docs/"
check python3 scripts/check-cli-flags.py

step "every cited backlog slug is still defined"
check python3 scripts/check-slug-citations.py

step "the CI workflow's layout matches the manifest"
check python3 scripts/check-ci-workflow.py

verdict "$EXPECT_STEPS"
