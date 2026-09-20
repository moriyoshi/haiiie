# Auditing Quiet Paths

Distilled from `JOURNAL.md`, 2026-09-16, and from correspondence with the yesnodb
session over the same two days. Both repositories ran every method here; where the two
results differ the difference is stated, because a method that worked once worked on one
tree.

## The problem

A differential suite tests that answers are correct. It therefore covers, exhaustively,
every path that **produces** an answer -- and covers a path that **declines** to produce
one only if somebody thought of it. Refusals, fallbacks, baselines, recovery paths and
reported statistics are the residue.

Five findings in two days lived there, and none was reachable by reading the diff that
introduced it:

* a fallback that clears a document's row blindly, which no test had ever entered,
  leaving a silently wrong row on a plain sequence of public API calls;
* a reported statistic that had never been non-zero, with two suites asserting it equals
  zero and nothing asserting it could move;
* a public filter variant with no test anywhere, reachable over the wire;
* five of eight error variants never constructed, one of which was hiding an arithmetic
  overflow that panics in debug and corrupts data in release;
* a baseline mechanism that could not work, whose set had always been empty.

## The rule

**Any mechanism whose quiet state is the only one ever observed is untested by
construction, and the passing case is identical whether it works or not.** The test is to
make the quiet state noisy on purpose and check that the number moves. Upstream's
formulation; it found the worst defect on either side.

## The method

1. **Enumerate.** Prefer a bounded, enumerable set over hand-placed counters. An error
   enum is the ideal case: a temporary `impl Drop` counts every instance at one point,
   whatever happens to it.
2. **Run everything and read the zeros.** Instrumentation is temporary and reverted --
   research does not ship.
3. **Classify by reading.** This is the step that does not automate.
4. **Close the gaps, then re-run the instrument.** "The test passes" and "the branch ran"
   are different claims and only the second was ever in question.

### The four outcomes, and only the first is work

| outcome | what to do |
|---|---|
| untested by accident | write the test |
| unreachable by construction | write nothing -- a fixture would assert what the code forbids |
| not provokable on this host | record it; a `cfg`-gated guard has no test on this target |
| no producer anywhere | a semver question, not a testing one |

The sweep produces the arithmetic for all four and distinguishes none of them.

### Two constraints on the instrument

**Scope is part of the result.** A variant with no producer in one crate can have an
ordinary producer one crate over. Both sides made this error on different axes: upstream
too narrow by process ( four variants covered by integration binaries with their own
counters ), here too narrow by crate ( `Io` has no producer in the engine and an obvious
one in the float store ). State the scope in the sentence reporting the number.

**The `Drop` probe is a survey, not a standing check.** `Drop` forbids moving out of the
type, so the probe stops compiling as soon as any code destructures a variant by value.
Its job is to generate the list and then be deleted. Re-verification comes from the test
asserting the variant, which is better evidence than a counter and survives the probe
being removed.

## The companion rule: mechanize the arithmetic, read the judgement

Three detectors in three days settled where mechanization pays. Two were withdrawn: one
for retracted doc comments ( 16 flagged, 8 false ) and one for backlog entries naming
vanished identifiers ( 14 flagged, 14 false, none of the three real cases ). Both were
strictly worse than reading, because "does this claim still follow" is semantic. A count
is arithmetic, has exactly one correct value, and re-deriving it is cheaper than reading
the sentence that states it -- so a test-count check is exact, free, and now gate step 9.

The two failures were not evidence against mechanizing. They were evidence about which
half is mechanizable, and it took a third case to see the split.

## Read the instrument's output as a sentence

A separate axis from whether the check is sound and whether the subject is checkable, and
**no control catches it, because the control passes**. Four instances in one week, none a
wrong computation: a gate-step audit matching labels rather than the scripts they invoke
( 13 of 20, all false ); a sabotage harness printing `CAUGHT` for its control arm; a
counter printing `12 fn across 6 files` where the 6 counted files holding *blocks*; a
gate step printing `54 lines checked` against a 77-line file. Name the noun the number
attaches to, name what was excluded, and give a control a label sharing no word with a
caught arm.

## What the week says about attention

Every catch came from a mechanism -- clippy, the slug checker, an injected counter, a
sabotage arm, a duplicate-attribute warning. Every miss came from someone who had just
finished thinking carefully about that exact failure: a doc-comment insertion bug
committed hours after sweeping 200 items for it; a backticked script stem written into
the paragraph explaining that backticked script stems fail; twice.

The common shape is that attention was on the **subject** and the error was in the
**scaffolding** -- doc placement, a control label, a backtick, a comment's example. Both
sessions observed the direction; neither dataset supports the mechanism, and upstream's
caveat stands: nearly everything written that week was scaffolding, so the denominator is
unknown.

The conclusion is not that judgement is unreliable. It is that a documented trap does not
prevent its own recurrence, and that the cheapest thing to mechanize is the part nobody
is looking at.
