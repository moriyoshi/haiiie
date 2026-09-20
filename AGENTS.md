# Documents for both humans and coding agents

* [README.md](./README.md) ... the human-facing description of the project: what haiiie is, what it returns, and an honest list of what is not built. It owns that description -- do not restate it in `.agents/docs/` and let the two drift.

# Documents for coding agents

* [./.agents/docs/OVERVIEW.md](./.agents/docs/OVERVIEW.md) ... what haiiie is, what it deliberately is not, and the milestone gates.
* [./.agents/docs/ARCHITECTURE.md](./.agents/docs/ARCHITECTURE.md) ... repository layout, key space, the two scoring paths, and the design policies that constrain changes.
* [./.agents/docs/QUALITY_GATE.md](./.agents/docs/QUALITY_GATE.md) ... the checklist a change must pass before it is reported as done.
* [./.agents/docs/TESTING.md](./.agents/docs/TESTING.md) ... the test layers, what each one catches that the others structurally cannot, and what may not go in them.
* [./.agents/docs/JOURNAL.md](./.agents/docs/JOURNAL.md) ... findings, insights, and review history. Append-only.
* [./.agents/docs/LTM/INDEX.md](./.agents/docs/LTM/INDEX.md) ... long-term memory index for durable project knowledge.
* [./.agents/docs/TODO.md](./.agents/docs/TODO.md) ... open backlog. Check and update when picking up or finishing work.
* Historical design notes and upstream prescriptions are archived in [JOURNAL.md](./.agents/docs/JOURNAL.md); use `ARCHITECTURE.md` for what exists and `TODO.md` for queued work.

This harness is adopted from [yesnodb](../yesno), which haiiie is built on. Where a
rule below is stated without a haiiie-specific reason, the reason is in that repository's
`AGENTS.md` and the rule was kept because it transfers. Rules that did **not** transfer
were dropped rather than copied: haiiie has no Bazel, no PostgreSQL ABI, no `roaring`
oracle, and no C ABI.

# Rules and protocols

## General

* Before changing anything in `haiiie-core/src/`, read `./.agents/docs/ARCHITECTURE.md`.
* **haiiie's promise is exactness.** Every kernel returns the true top-k under the
  binary code distance. A change that makes something faster by making it approximate
  is not an optimization here, it is a different product -- and it needs `OVERVIEW.md`
  amended and a user-visible name, not a quiet threshold.
* Tuning constants carry their derivation in a comment beside them. If you cannot say
  what measurement produced a number, it is not ready to be a constant.

## File Management

* Work summaries go under `./.agents/docs`, never `/tmp`.
* Scratch scripts, corpora, profiling output and generated fixtures go under
  `./.agents-workspace/tmp`, which is gitignored.
* Do not leave built binaries, flamegraphs or `perf.data` in the version-controlled tree.
* Never delete user files without permission. Only safe to delete: files YOU created in
  THIS session under `./.agents-workspace/tmp/`.

## Research and Measurement Code

* **Do not add code to `haiiie-core/src/` to study a question the crate does not answer
  at runtime.** Instruments, cost models and encoding simulations are *research*, and
  research does not ship. `src/` is production; a `pub` item there is public API and a
  semver promise you then have to keep for something nothing calls.
* Where it goes instead: a one-off question to a standalone crate under
  `./.agents-workspace/tmp/` with a path dependency; a measurement a gate should keep
  running to `tests/`; a kernel timing to `benches/`.
* **The finding is the deliverable, not the instrument.** Record the numbers *and the
  construction that produced them*. A number recorded without its construction cannot
  be re-derived, only re-measured.
* **Dead public API is not free.** Upstream removed a `backing()` method from a trait
  whose only caller was its own test and which cost a full index scan to return one of
  three enum values. One test is not a caller.

## Numbers That Cross A Boundary

**Nothing in a number's presentation carries its evaluation frame.** A plausible figure
from a well-informed source reads exactly like a checked one. Three figures broke on
this project's first day for that reason alone -- see `JOURNAL.md`, 2026-09-13.

* When a number crosses between components, between documents, or between sessions,
  **the frame travels with it explicitly**: which surface produced it, and which surface
  consumes it.
* "Per Block, inside the scoring kernel" and "per operand, in yesnodb's planner" are
  different frames wearing the same word. So are "materializing a container" and
  "resident bytes".
* A figure arriving without its frame is a claim to be re-derived, not a fact. This
  applies to figures in our own documents, and to figures we send upstream.
* The remedy in every case so far was reading the code rather than trusting the figure,
  including when the figure was our own.

## Citations Decay Mechanically

A comment that says "see `some-slug` in TODO.md" keeps reading as though the
reasoning is written down, long after the entry has been consolidated away. The
decay needs no carelessness: consolidating a JOURNAL entry deletes its slug,
every citation of it survives untouched, and nothing links the two. Upstream
swept for this and found **25 dangling citations, two of which were the only
surviving record of a real design gap**.

* `scripts/check-slug-citations.py` fails the gate on a citation whose slug is
  no longer defined in `TODO.md` or `JOURNAL.md`.
* When it fires, the fixes are ranked and the obvious one is the weakest:
  **restate the reasoning at the citation site and delete the pointer** ( which
  removes it from the check's domain rather than satisfying it ); repoint only
  if the reasoning is real and too long to restate; restore the entry only if
  the item is genuinely still open. Making a slug resolve makes nothing truer.
* This is the same disease as an unchecked number ( above ): something that
  reads like evidence, sits next to the code, and is not.
* **The same decay reaches design decisions, and no check here sees it.** Had we
  reshaped the key space to dodge an upstream crash, nothing would have recorded
  why the schema was that shape -- a schema has no rationale field, so the loss
  would have been invisible in a way a stale comment at least is not. Stated as
  a known limit rather than covered by a rule nobody could enforce, because
  pretending otherwise is how a gate acquires a step that stops meaning anything.

## Self-Contained `docs/`

* **Documents under `docs/` must never reference source paths.** They are standing,
  human-facing documents; the tree moves underneath them and a path written into prose
  goes stale silently. State the *result*, not the location.
* This is the opposite of the rule for `.agents/docs/`, which is *supposed* to name
  source -- `ARCHITECTURE.md` carries a layout block that `scripts/check-layout.py`
  verifies against the tree in both directions.

## Building

* Plain `cargo` is the entry point. There is no agent cargo wrapper.
* Format with `cargo fmt --all` before running the gate.
* `haiiie-core` has a **CI-enforced direct-dependency budget of two** ( `yesno-core`,
  `thiserror` ). It must not acquire `tokio`, `tonic` or `arrow-flight`. It reaches
  `arrow_buffer` types through `yesno_core::unstable_arrow`'s re-export precisely so it
  need not depend on Arrow itself. `scripts/check-deps.py` enforces this.
* yesno crates use sibling path dependencies; `Cargo.lock` does not pin
  their source. `YESNO_REVISION` and `scripts/check-yesno-revision.py` make
  the full pinned SHA and a clean sibling checkout a gate requirement, and CI
  checks out that SHA. Update the pin only with a recorded upstream verdict
  and a haiiie integration check. `yesno-core::unstable_arrow` is semver-exempt
  upstream; quarantine every use in one module. A green haiiie gate does not
  substitute for an upstream gate verdict.

## Testing

See `./.agents/docs/TESTING.md` for the layers. The rules that bind:

* **Never weaken an oracle, loosen a property, or raise an allocation budget to make a
  failing test pass.** If the test is genuinely wrong, say so explicitly and record the
  reasoning in `JOURNAL.md`.
* **The slow, obvious implementation is never deleted because a fast one exists.** The
  brute-force scorer is the oracle for every kernel, and the kernels are oracles for
  each other.
* **Calibrate a sabotage to survive the summarization between it and the observable.**
  Upstream injected a fault that a 256-bucket occupancy summary swallowed, so the test
  passed and read as toothless when the injection was simply too weak. haiiie has the
  same hazard at per-Block statistics, the cardinality-only descent, and the planner.
* **Reopen before asserting durability.** A live handle answers from the memtable and
  would hide a persistence bug.
* Proptest failures persist to `tests/*.proptest-regressions`. Commit new seeds; never
  delete them to go green.
* Benchmarks are not a gate. A benchmark regression is a `JOURNAL.md` finding; only an
  allocation-test failure blocks a change.

## Local Lint Gate

Before reporting any Rust change as done -- and this applies to subagents -- run:

```
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

* **`--workspace` is load-bearing, not decoration.** Deferring to `./scripts/gate.sh` is
  equally correct and always current.
* A tool and its instructions are two implementations of the same check. If you fix one,
  fix the other.
* When delegating to a subagent, instruct it to run this gate and include the result in
  its report.

## Unsafe Code

* Prefer `bytemuck` over hand-written transmutes.
* A new `unsafe` block needs ( a ) a `// SAFETY:` comment stating the invariant, ( b ) a
  `JOURNAL.md` note, and ( c ) a property test that would fail if the invariant were
  violated.

## Python

* With a `pyproject.toml`, use `uv run pytest ...` and `uv run python ...`.
* Without one, never run a bare `pip install` outside a venv; use `uv venv` + `uv pip`.

## Shell Pitfalls ( prezto defaults )

The user's shell sets aliases and options that break non-interactive scripts:

* `cp src dst` prompts when `dst` exists. `rm -f dst` first.
* `cat > file <<'EOF'` fails with `file exists` ( `NO_CLOBBER` ). `rm -f file` first, or use `tee`.
* `rm file` prompts. Use `rm -f`.

## Working With Upstream ( yesnodb )

* haiiie consumes yesnodb; **nothing in `../yesno` is ours to change.** Do not run its
  gates, edit its source, or commit in its tree.
* Something that is set algebra rather than similarity belongs upstream. File a
  prescription; do not build it here and call it done. See `JOURNAL.md`, 2026-09-13.
* A prescription is held until it carries a working implementation and a measured
  benchmark. Upstream's conventions ask for evidence, not an API taken on faith.
* Treat an upstream branch as unproven until its own gate is green, whatever the cargo
  side says.

## Git Workflow

* Neither `git checkout` nor `git restore`. Another agent may be working in the same checkout.
* Never make discretionary commits.

## Documentation

* Write work summaries into one of the existing documents.
* **Append to `JOURNAL.md`; do not edit existing entries.** The exception is an entry
  that was never true -- a wrong account is not worth preserving as history, so correct
  it in place and say that you did.
* Module-level `//!` comments carry the *why*. When you change behaviour a `//!` block
  explains, update it in the same change. A stale rationale comment is worse than none.
* No emoji anywhere in the tree.
* For repo-authored documentation, never use full-width parentheses ( `（` `）` ). Use
  half-width parentheses ( `(` `)` ) with a half-width space before an open and after a
  close parenthesis when adjacent to a non-whitespace character.
