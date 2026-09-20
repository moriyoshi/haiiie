# Test Layers

Each layer exists to catch something the others structurally cannot. Know which one
your change threatens.

| layer | file | catches |
|---|---|---|
| oracle | `tests/oracle.rs` | wrong answers, against a brute-force scorer |
| path equivalence | `tests/differential.rs` | one kernel disagreeing with another |
| allocation | `tests/allocation.rs` | a non-materializing path decaying into a materializing one |
| durability | `tests/durability.rs` | anything that only works before a reopen |
| determinism | `tests/determinism.rs` | thread-count-dependent results |

## 1. The oracle

A brute-force scorer over `Vec<u64>` with `count_ones()` and the exact comparators,
in the test crate only. Slow and obviously correct. It exists **before** the kernels
it checks -- a differential suite written after an implementation tends to encode that
implementation's bugs.

## 2. Path equivalence: four oracles for the price of one

`Gather`, `DenseScan`, `Inverted-tiled`, `Inverted-CSA`, `Inverted-ripple` and
`Counter-array` are six implementations of one function, with the path **forced**
rather than planned. Five of
them are oracles for each other. This is the layer that makes it safe to add a fast
arm, and the reason the slow arms are never deleted.

## 3. Properties worth naming

* `top_k(filter=F) == oracle_all().filter(F).take(k)` -- the strongest in the suite.
  A bad pruning bound removes a document that this construction keeps.
* Stored `z` planes equal `D - row_weights(forward)` for **every** document. A
  divergence here is a silently wrong answer on one path only.
* `top_k(k)` is a prefix of `top_k(k+1)` is a prefix of a full sort.
* **Chunk-boundary invariance**: shifting every ordinal by a constant -- including one
  that is not a multiple of 65536 -- permutes the result identically. This is the
  classic chunked-index failure and it is invisible to a corpus starting at ordinal 0.
* Degenerate widths: `|Q|` of 0 and D; `|Q|` at `2^j` and `2^j - 1` for every `j <= 13`,
  which is where an off-by-one in the CSA level count hides.

## 4. Generators must be boundary-biased

Uniform random codes never fill an array container and never produce a run container,
so they never exercise the container-expansion path at all. Bias on three axes --
clustered, runny, at the representation ceiling -- as upstream's generators do.

## 5. A passing sabotage has two causes, and it cannot tell you which

A sabotage that **passes** is never reassurance. It means one of two things, and they
are opposite defects wearing the same symptom:

* **Weak injection, sound test.** The fault does not survive the distance between where
  it is injected and where it is observed. Upstream dropped every other prefix from a
  256-bucket occupancy summary and never emptied a bucket, so the summary swallowed it.
* **Weak test, sound injection.** The fault arrives intact and the assertion is too
  loose to notice. Upstream asserted `allocations >= 800` to prove segmentation still
  engaged, then sabotaged the short-circuit to decline everything. It passed: 800 sits
  below **both** the engaged figure ( 1 038 ) and the suppressed one ( 910 ), so the
  threshold guarded nothing.

**Nothing in a passing sabotage distinguishes them.** The only thing that does is
knowing both numbers -- with the fault and without it -- *before* choosing the
threshold. A round number picked under the expected result is not a guard; it is a
number that happens to be true.

The rule: **an assertion threshold must sit strictly between the two measured values.**
If you cannot state what the assertion reads under sabotage, you have not yet designed
the test. And ask what sits between injection and assertion -- in haiiie that is
per-Block centroid and radius, the cardinality-only descent, the planner's statistics,
and any bucketed count.

## 6. A single timing is not a measurement, it is a sample

Run every timed arm **at least three times and compare ranges**, not means. The
spread on this machine is 10-20%, which is wide enough to invert a real 15%
effect in either direction.

This is not a caution, it is a tally. Conclusions that flipped or firmed only
under repetition, all in one day:

* An `is_sorted_by_key` scan that looks free costs ~12% at 2 M operations.
  Upstream established it by running the no-sort baseline three times; single
  numbers read as variance and the ranges barely overlapped.
* Moving a sort ahead of a bucketing loop: single runs said worse everywhere,
  three runs said **better at 4 shards and worse at 16** -- so it was a tuning
  knob rather than a fix, and shipping it would have been shipping a knob.
* Our own translation loop: a single sample would have supported "the residual
  is partly ours". Three runs per arm put the push ranges **overlapping** and
  the commit ranges disjoint by 4x, which settled it in one direction.

The reporting rule that follows: quote the **range**, and say how many runs. A
mean with no spread behind it is not reportable, and this project has spent a
week correcting numbers that were quoted as single values.

## 5a. Print the achieved parameter, never the requested one

An instrument that echoes its own inputs is reporting on its author's intent. One
that reports what it actually got is reporting on the system, and the two are
indistinguishable in the output.

Upstream hit this on 2026-09-16: an arm labelled `dirty = 40 000` measured about
**10 500**, because the automatic checkpoint policy fired during the load and the
explicit checkpoint saw only the remainder. It was caught solely because the
instrument printed the count it *received*. Printing the requested figure -- the
obvious way to write it -- would have produced a 40 000 row that was a duplicate
of the 10 000 row, and a curve that looked flat across a range with no data in it.

The rule is cheap and general: **whatever a benchmark varies, it must measure and
report, not restate.** Corpus size after ingest, dirty count at the moment of the
operation, thread count actually spawned, filter selectivity as admitted rather
than as intended. Wherever the system can quietly decline to honour a parameter,
the echoed version of that parameter is a lie with the shape of evidence.

This sits beside section 5 rather than in section 6 because it is the same family
as a sabotage that does not inject: the harness reports success at doing
something it did not do.

## 5b. A neutral label can hide the case, not just the parameter

Section 5a catches a parameter the system quietly declined to honour. This is its
sibling and needs stating separately, because "print what you got" does not catch
it: here the arm got exactly what it asked for, and the variable that mattered was
not the parameter at all.

An arm labelled `overwrite` rewrote every document **with its own code**. That is
an overwrite by any definition, the label was accurate, and the code did what it
said. But the quantity under test was a writer that emits only changed bits, for
which rewriting a document with its own code is the **empty-diff best case** --
and reporting it would have put the headline at 24x to 54x where a realistic
update is about 5x.

Nothing in the name, the code or the output distinguished the two. What surfaced
it was the result looking too good beside a mechanism that explained exactly why
it would.

So, whenever a change's benefit depends on a property of the *data* rather than
on the operation performed:

* **Name the case, not the operation.** `same code` and `changed` are arms;
  `overwrite` is a category containing both, and a category is not a measurement.
* **Ask what the best case for this change looks like, and whether the fixture is
  accidentally it.** A corpus rewritten from its own seed is the natural way to
  build an update benchmark and is also, for anything difference-based, the
  degenerate case.

## 6a. A result you did not see is a bound, not an absence

Section 6 is about reading a measurement as **more** informative than its
construction allows. This is the same error running the other way, and it needs
a different check because none of the questions in section 6 catch it.

Before reporting that a change made no difference, answer two questions:

1. **What effect size would I have expected?** Not "some"; a number, derived
   from whatever is already known about the mechanism.
2. **Could this measurement have seen it?** Compare that number to the spread
   from section 6.

If the answer to (2) is yes and nothing appeared, the result is not "no
difference" and not "below the noise floor" -- it is **an upper bound on the
effect, and a bound is a finding**. Report it as one, with the number.

The case this came from: a checkpoint's durability syncs were removed from an
exclusive region, upstream measured that region at ~17 ms, and our own stall was
85-165 ms -- so the expected effect was 10-20%, or 10-20 ms on a 100 ms stall,
far above this machine's spread. Measured, it moved **under 1.5% across four
thread counts**. That was filed as "below the noise floor" and it was nothing of
the kind: it bounded the syncs' contribution at under about 2 ms, which is the
single most informative number the whole exchange produced, and it went
unreported for a day because a null result read as a non-result.

Upstream made that argument **against their own change**, which is the other half
of why it is here. The expected effect size was computable from a figure quoted
in our own backlog two hours earlier; nobody had to find new evidence, only to
divide.

## 6b. A test's power is a measurement

A sabotage that passes has three causes, not two ( section 5 ): a weak injection, a weak
test, or a sabotage that is not a defect. Section 5 assumes you ran one. **This is about
tests whose sabotage has never been run at all.**

`churn.rs` was written to catch a storage-engine defect, disclosed in its own header that
it had not been shown to catch it, and did not. It was then shrunk from 619 seconds to 24
on the reasoning that the trigger was its density sweep rather than its volume -- a
sentence written into the file before any run existed to support it, and exactly
backwards. Calibrated against a reversed fix, 40 000 documents catch the defect and
25 000 do not.

So:

* **Calibrate before shrinking.** A test's runtime is worth reducing only once its power
  is known; before that you are optimizing an unknown quantity, and the reduction that
  looks like hygiene is the one that empties it.
* **Put the calibration table in the file.** Not the conclusion -- the runs. Anyone who
  later finds the test slow then has to re-measure rather than re-reason, which is what
  stops the next well-meant shrink.
* **A disclosure is not a mitigation.** "Not sabotage-verified" in a header is honest and
  still leaves something in the suite that reads as a safeguard and is not one. Where the
  sabotage genuinely cannot be run -- the fix lives in a tree we may not edit -- vendor a
  copy and run it there.

## 6c. Revert a sabotage by construction, not by a step that can be skipped

A sabotage loop that patches, runs, then restores leaves the tree **damaged** if
anything between the patch and the restore does not complete. That is not
hypothetical: a sabotage of the scan's cursor rebuild was applied on 2026-09-14
in a loop that hit its two-minute timeout before restoring, and the deletion
stayed in `search.rs` for a day. Every backup taken afterwards captured the
damaged file as though it were the good one, and **every gate run in between was
green** -- because the site it removed is one nothing tests, which is why it was
being sabotaged.

Two rules follow:

* **Restore first, not last.** Begin each iteration by restoring from the
  pristine copy, so an interrupted run cannot carry damage into the next one.
  Better still, patch a copy of the tree rather than the tree.
* **Diff against the pristine copy when the loop ends**, and fail loudly if
  anything differs. The loop knows what it changed; nothing else does.

The deeper point is the one that made this survive: **a sabotage is only safe to
leave running unattended where the site is covered.** An uncovered site is
exactly where a stuck sabotage is invisible, and exactly where sabotage is most
worth doing. The practice is most dangerous where it is most valuable.

## 6d. A control validates the instrument, not the reference

Sections 6a to 6c are about instruments that cannot fail visibly, and the defence
is a control: prove the thing could have found something before believing it
found nothing. That defence is sound and it has a hole.

**A control says nothing about whether the reference is sound.** An audit that
compares the tree against a baseline has two ways to lie, and a control catches
only one of them:

* the *instrument* is broken -- a pathspec matching nothing, a pattern matching
  comments, a formatter printing a literal. Caught by a control.
* the *baseline* is wrong -- captured after the fault it is supposed to detect.
  **Not caught by anything**, because the instrument works perfectly and reports
  truthfully on a reference that already contains the damage.

Both happened on 2026-09-15. Ours: `git diff` against the index showed every
deletion accounted for, and the index had been staged *after* a stuck sabotage
landed, so it held the damaged form as its idea of pristine. What exposed it was
noticing that a known repair appeared on the wrong side of the diff -- an
accident, not a procedure. Upstream's was worse and by the same mechanism: their
only commit had been amended by a third session, so their reference was captured
by someone else entirely and no moment in their own history could be pointed at.

The fix is not a better baseline. It is **no baseline**:

> Audit for the artifacts the fault would have left, not for difference from a
> reference. Sweep for the forms that must be present and the forms that must be
> absent, and carry a control for each direction.

That is checkable without trusting any prior state. It is also why the site-by-
site check that actually settled our sweep -- forms that must be present, forms
that must be absent, a nonsense expectation to prove absence is reportable -- is
the durable form and the `git diff` was the decorative one.

One practical rider, from upstream: **print the matches, not the count.** A tally
of zero and a tally of four are equally unreadable; four matches you can see are
four you can recognise as a pre-existing constant or a comment. Both of our false
positives were caught by looking at what matched.

## 6e. Read the instrument's output as a sentence and ask what it claims

A control cannot catch this one, **because the control passes.** A control asks whether
the instrument can find things. It says nothing about whether the sentence the
instrument prints describes what it found -- and that is a third axis, separate from
whether the check is sound and separate from whether the subject is checkable at all.

Every instrument failure in the week of 2026-09-16 was on this axis, and none was a
wrong computation:

* a gate-step audit reporting **13 of 20 undocumented** -- it had matched step *labels*
  against prose rather than the *scripts* those steps invoke. The number was real; what
  it counted was not what the label said. Matching scripts reported 0.
* a sabotage harness printing `CAUGHT` for its **control** arm. The comparison
  `failed == expect_fail` was correct; the word was wrong for a no-injection arm, so a
  correct control read as a caught sabotage.
* a counter printing `12 fn across 6 files`, where the 6 counted files holding *blocks*.
* a gate step printing `54 lines checked` against a **77-line** file, because comment
  lines were stripped before counting. True, and it invited a reader to conclude the
  file was fully examined when 23 lines were never looked at.

The last two are the same shape as `5b`: a number attached to the wrong noun. What makes
this a separate section is the first sentence -- there is no arm you can add to catch it.
The audit is reading the output as prose and asking what a reader would take it to mean,
which is the same move `5c` asks for on comments and this document asks for on oracles.

The remedy is cheap and worth applying at the moment an instrument is written, since that
is when the author knows what was measured: **name the noun the number attaches to, and
name what was excluded.** `54 of 77 lines checked (comments not examined)` costs nine
words and cannot be misread. And give a control a label that shares no word with a
caught arm -- `passes (control held)` rather than anything that reads like a verdict.

## 7. What does not belong here

* **A test that reimplements a kernel and checks its own answer.** It tests the
  reimplementation. The admission question is: *does a change to `haiiie-core` fail this
  test in the way its assertion is phrased?*
* **A benchmark masquerading as a test.** Benchmarks are not a gate; only allocation
  budgets block a change.
* **An assertion on cardinality alone.** Two different wrong answers have the same
  count, which is exactly how a missing kernel arm stays invisible.

## 5c. Two kinds of wrong comment, and only one is mechanizable

Upstream drew this distinction after both trees turned up a bad comment on the
same day, and it changes what can be defended against:

* An **unlicensed impossibility** is a *judgement* failure: a conclusion that
  does not follow from its premise. "The old code is not known without reading it
  back, so clearing cannot be narrowed" -- true clause, false conclusion, no
  visible seam. Nothing mechanical checks this, because licensing is a judgement
  about whether one sentence follows from another. All the convention buys is
  that a bad one *reads* wrong, which is weaker than a check and appears to be
  the only defence available.

* A **superseded measurement** is a *maintenance* failure: the sentence was true
  when written and a later edit falsified it, with nothing connecting the two.
  This half **is** partly mechanizable, and the rule is cheap:

> A comment carrying a figure or a complexity class is a re-read candidate
> whenever the code it sits on changes.

Swept on 2026-09-16 across the four files edited that day. Thirty-two
figure-carrying comments, two stale -- both quoting measurements taken before
`Writer::put` learned to emit only the difference, and both on functions that
change had touched. Neither was old: they had been written hours earlier.

**Age is not the variable.** A comment is authoritative from the moment it is
written until something goes looking, and that interval is set by the sweep, not
by the calendar. An audit that comes back clean is clean *on the axis it swept*.

One thing the corrections bought that the errors were hiding: forced to state the
current cost honestly, the useful fact turned out to be the distinction between
re-putting a document's own code and changing it -- 24x against 5x. The false
figure was concealing a real one, which is a reason to rewrite rather than
annotate, on top of the ordering argument in section 5b.
