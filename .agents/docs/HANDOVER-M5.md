# Handover: the last M5 task

**Update 2026-09-20:** the maintainer selected option 2 below. Offline compaction and
ordinal reclamation are implemented under M5, with a durable ID mapping and explicit
acknowledgement before reopening. The text below is the incoming handover, not the
current backlog; use `TODO.md` and the appended `JOURNAL.md` entries for validation.

Written 2026-09-20 for a session picking this up cold. Everything here is
checkable from the repository; nothing depends on the conversation that produced
it.

## Read these first

`AGENTS.md` ( `CLAUDE.md` is a symlink to it ) is binding. The three rules most
likely to bite on this task:

* **`../yesno` is not ours to change.** Do not run its gates, edit its source, or
  commit in its tree. Reading it is expected and encouraged.
* **Scratch goes under `./.agents-workspace/tmp`, never `/tmp`.** A measurement
  run lost its write-ahead log to a routine `/tmp` cleanup and the failure looked
  convincingly like a scale limit. Research crates live there as standalone
  cargo projects with a path dependency and an empty `[workspace]` table.
* **`./scripts/gate.sh` before reporting anything done.** Ten steps. It counts
  its own steps and the tree's tests, so adding a test means updating the count
  in `.agents/docs/OVERVIEW.md` -- the gate will tell you the number.

Then `.agents/docs/TODO.md` for the backlog and `JOURNAL.md` ( append-only ) for
how each number was arrived at.

## State

M0 through M8 are built. M9 was a prescription to upstream and was **declined**
on 2026-09-19 -- see `TODO.md`, and the reasoning is worth reading because the
argument failed on an unchecked premise rather than on its merits.

**M5 has exactly one item open: `weight-sorted-ordinal-assignment`.** Eleven of
its twelve are closed.

## What is already done on that item

Everything that is *work*:

* **Measured.** Assigning ids in code-weight order is worth **189x** on documents
  given an exact score for Jaccard and cosine -- 67 672 against 358 at 524 288
  documents, D=256, k=10. It was 40x until 2026-09-19, when the scan began
  carrying its refinement threshold between blocks; that improved the unsorted
  case 1.9x and the sorted case 8.8x, so the gap widened rather than closed.
  The instrument is `.agents-workspace/tmp/refine`.
* **Documented.** `docs/data-modeling.md` carries the table, not the adjective
  "narrow", because a reader cannot act on an adjective.
* **Observable at runtime.** `Plan::block_weight_spread` reports the mean of
  `w_max - w_min` over blocks carrying statistics, shown only for the ratio
  metrics, compared against `dims`: a spread near `dims / 2` means ids are in no
  weight order. So a caller paying the 189x can see it. Pinned by `m5_stats.rs`
  and carried over the wire ( `remote_equals_embedded.rs` ).

## What is left, and it is a scoping decision rather than code

**haiiie cannot own this at ingest.** Ids are caller-assigned, dense and stable,
and reordering them is exactly the ordinal recycling the design forbids -- a
reused ordinal whose old posting bits survive scores the new document by the old
code, silently and with the right count.

A **compaction** could own it. But compaction is also the mechanism that would
reclaim retired ids, so the two want building together or not at all, and the
original plan schedules compaction under ingest and deletes rather than under
M5. M5's gate says "clustered ordinals", which is satisfied as far as it can be
without that mechanism.

Three options, and this is a **maintainer decision** rather than something to
resolve by building:

1. **Close M5.** Declare its share done and file compaction-with-id-reclamation
   as its own item outside M5. This is the reading that matches where the plan
   puts compaction, and it is what the outgoing session would have recommended.
2. **Build compaction**, and M5 stays open until it lands. Rewrite a region with
   fresh clustered ordinals, rebuild `STAT`, commit atomically, release old
   ordinals. It needs its own correctness story: a compaction that gets ordinals
   wrong rescores the whole corpus silently, which is the worst failure shape
   this engine has.
3. **Leave it open** as a standing reminder that the largest single factor in
   ratio-query cost sits outside the engine's control.

Do **not** make haiiie silently reorder anything. The measurement does not
justify it and the entry says so.

## Other open work, ranked, if M5 is closed out

1. **`filtered-queries-stop-scaling-at-four-million`** -- the most valuable open
   item. Filtered queries are linear to about four million documents and bend
   after it ( `N^1.140`, then `N^1.254` ) while unfiltered stays at 1.04 to 1.08.
   Diagnosed on 2026-09-19: blocks visited grow **exactly** linearly and the
   per-block cost grows 1.30x, so it is costlier blocks and not more of them. At
   87.8 microseconds to score 64 admitted documents per block, the 8 KiB
   container read is happening **once per admitted document**, not once per
   block. What is unexplained is only the 1.30x -- locality is the obvious
   candidate and is unmeasured.
2. Eight threads measured 0.47x serial for Jaccard. The effect predated the
   threshold work ( 0.81x at baseline ): parallel blocks lose the running score
   floor and refinement contends on the storage layer's per-chunk mutex.
3. **`ci-has-never-run`** -- the workflow is written and has executed nowhere.
4. The public `DocId` tuple bypasses its constructor guard, so the write boundary
   validates the dimension-dependent maximum id before row-address arithmetic;
   otherwise release multiplication can wrap into an unrelated row.

## Conventions that are easy to violate

* No emoji anywhere in the tree.
* Half-width parentheses with a space inside, in repo-authored prose.
* Append to `JOURNAL.md`; never edit an existing entry, unless it was never true.
* A citation like `` `some-slug` `` must resolve to a bolded slug in `TODO.md` or
  `JOURNAL.md`, and the gate checks it. Write a script's name with its `.py`, or
  it parses as a slug and dangles.
* Never weaken a test to make a change pass. One pinned assertion **was**
  changed on 2026-09-19 ( `m6_parallel.rs`, work-equality across thread counts );
  the reasoning is in `JOURNAL.md` and the bar it had to clear is high.
