# Quality Gate

The checklist a change must pass before it is reported as done.

## 1. Baseline commands

```
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
./scripts/gate.sh
```

`--workspace` is load-bearing. `./scripts/gate.sh` runs all of the above plus the
structural checks and is always current; deferring to it is equally correct.

## 2. Structural checks

| check | what it refuses |
|---|---|
| `check-layout.py` | a source file absent from `ARCHITECTURE.md`'s layout block, or a listed path that no longer exists -- both directions |
| `check-deps.py` | `haiiie-core` exceeding two direct dependencies, or acquiring `tokio` / `tonic` / `arrow-flight` |
| `check-yesno-revision.py` | a yesno path dependency outside the sibling checkout, a different HEAD, or dirty upstream source |
| `check-docs-selfcontained.py` | a source path written into `docs/` |
| `check-slug-citations.py` | a cited backlog slug no longer defined in `TODO.md` or `JOURNAL.md` |
| `check-ci-workflow.py` | the workflow's checkout layout or yesno SHA disagreeing with the manifest and pin |
| `check-test-count.py` | `OVERVIEW.md` claiming a test count the tree does not have |
| `check-cli-flags.py` | a CLI long flag no `docs/` page mentions ( mention, not explanation ) |

A baseline mechanism exists so a list can only shrink. Do not add an entry to make a
change pass.

**Three of these had been omitted from this table** while sitting in `gate.sh`, which is
the same decay they exist to catch, one level up. `check-layout.py` cannot catch it: it
checks `.rs` files against the layout block, so a new script is unlisted silently. The
table is maintained by hand and knowing that is the whole of its warranty.

**What belongs here and what does not.** Every check above re-derives an
objective fact -- a count, a path, a dependency total, or the exact Git revision
and cleanliness of the source Cargo compiles. Two attempts this week to mechanize a
*semantic* one both failed and were withdrawn: a detector for retracted doc comments
( 16 flagged, 8 false ) and one for backlog entries naming vanished identifiers
( 14 flagged, 14 false, and none of the three real cases ). Both were strictly worse
than reading. Mechanize the arithmetic; read the judgement.

## 3. The gate counts its own steps

`gate.sh` asserts it reached `EXPECT_STEPS`. **Update that constant when you add or
remove a step.** Adopted from upstream, where a gate once exited 0 having run less than
half of itself: a run died mid-script, `set -uo pipefail` did not abort ( deliberately,
so every failing check is reported rather than only the first ), `$fail` was still 0,
and the tail never executed. A gate that can pass without running is worse than no gate.

## 4. Exactness conformance

Any change touching a kernel, a bound or the planner must leave the path-equivalence
suite green **with every path forced**. A pruning change must additionally leave
`top_k(filter=F) == oracle_all().filter(F).take(k)` green, because a bad bound produces
a missing result rather than a crash.

## 5. Allocation discipline

Scoring `C` Blocks allocates `O(L)` buffers in total, not `O(C)` and never
`O(C * |Q|)`. Without this test the design decays back into per-operation containers
and nothing red ever appears.

## 6. Before reporting done

* Did you run the baseline commands, not merely intend to?
* Does a `//!` block now explain behaviour that changed?
* Is every new constant accompanied by the measurement that produced it?
* Did any number in your report cross a frame boundary? See `AGENTS.md`.
* Is the finding in `JOURNAL.md`, or only in the chat?
