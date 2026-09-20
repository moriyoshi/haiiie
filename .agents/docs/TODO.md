# Project To-Dos

Active backlog only. Once an item is addressed, move its work summary into
`JOURNAL.md` and remove it from here.

## Open Items

- [ ] **current-writer-query-tail-attribution** ( 2026-10-03 ): P5 now
  measures real changed-code overwrites on the current pinned code. Qualified
  one- and eight-thread cells with policy checkpoints gave p99 25.24 and
  21.97 ms at 2,097,152 documents; the dated eight-thread p99 was 85.74 ms
  on an earlier implementation. The historical same-code writer is now an
  idempotent re-put control, not an update workload. To attribute the remaining
  tail, repeat changed-code writes with checkpoints deferred and pair per-query
  latency with commit/checkpoint spans on a quiet host. Do not infer a
  checkpoint cause from the three checkpoints per qualified P5 cell alone.
  Construction: `.agents-workspace/tmp/remeasure-20261003/REPORT-p5.md`.

- [ ] **secure-writable-sidecar** ( 2026-10-03 ): The operator's `spec.plugin`
  mounts only its Unix socket, while generated secure topology requires client
  mTLS for yesnod Flight and control. Haiiie's peer writer and control worker
  cannot use client certificates, so the draft Helm chart's secure profile
  opens a preinitialized index read-only. A fresh secure cluster cannot be
  initialized through this sidecar. Make credential projection and the client
  transport agree before advertising secure peer writes or checkpoint control.

- [ ] **sidecar-chart-cluster-proof** ( 2026-10-03 ): The draft chart renders
  against the operator CRD, but has not been installed against a running
  operator. Verify fresh-index creation, follower catch-up, promotion routing,
  readiness through rebootstrap, and the sidecar image in a real Kubernetes
  cluster before treating it as a deployment recipe.

- [ ] **served-peer-query-overhead** ( measured 2026-10-03 ): P4 on clean
  yesno b712a03 and archived haiiie ed930ae code found 0 full-hit mismatches
  in 19,200 comparisons over separate-process arena/inline peer modes, Run
  and bitmap-heavy fixtures, D=64/256, natural/wide queries and one/eight
  threads. On the qualified D=256 natural run-shaped one-thread cells,
  embedded took 0.38-0.40 ms, arena 2.39-2.64 ms, and inline 1.72-1.89 ms.
  A fresh bitmap-heavy D=256 rerun passed both quiet gates in all eight cells
  and found zero full-hit mismatches. On natural one-thread queries, embedded
  took 0.40-0.48 ms, arena 2.95-3.25 ms and inline 3.79-4.90 ms; arena beat
  inline by about 1.3x. Wide 265-lane one-thread ranges overlapped at
  7.32-8.04 and 7.52-7.87 ms. The original P4 report excludes seven noisy
  cells; the later rerun supplies this arm. The former 5b and c2 frames are
  in the 2026-09-29/30 JOURNAL and must not be merged with P4. Attribute the
  lane-shape-dependent transport cost, measure more than four blocks,
  concurrent queries, and live arena resident high-water before tuning the
  channel. The default 536,870,912-byte arena
  is an advertised mapping span, not measured resident memory. Construction:
  `.agents-workspace/tmp/remeasure-20261003/REPORT-p4.md`.

- [ ] **upstream-flight-bitvector-repin** ( 2026-10-02 ): yesno's published
  c7524a4 adds a compact Flight bitvector response and dense-span lending,
  but the later 644ca48 commit fixes a filtered-ticket path that could
  return unfiltered bits. It is only in the local upstream history at this
  check. Haiiie does not request the bitvector response; still, do not move
  its deployment pin beyond b712a03 until the fix is published and the
  resulting revision passes the haiiie gate. A clean c7524a4 snapshot did
  pass that gate, which does not exercise the optional bitvector response.

- [ ] **served-peer-packed-residual-ingest** ( 2026-09-29, narrowed 2026-10-03 ):
  Flight `PUT_APPLY` has no `PatchChunk`, so the peer correctly emits point
  operations. P3's pinned 96,903-document COCO residual comparison measured
  embedded packed writer at 0.035-0.040 s, embedded point at 2.62-2.70 s,
  and peer Flight point at 5.85-10.27 s across four quiet runs; the peer
  writer remains variable even within one session. All four peer top-10 ID
  files matched embedded byte-for-byte. Attribute the Flight point cost and
  variance, then prototype a bounded patch transport and measure writer,
  checkpoint and recovery with matched commits before filing a new upstream
  prescription. No approximate fallback is acceptable. Construction:
  `.agents-workspace/tmp/remeasure-20261003/REPORT-p3.md`.

Accuracy research note: the maintainer ruled out retained per-document floats.
The 2026-09-20 JOURNAL entry "Float-free decoder experiment: recall improves, bitmap
execution loses" records the completed standalone experiment and its construction.
Residual codes improve held-out COCO recall; rank training adds a small gain, while
the exact bitmap prototype loses to packed-code scanning. This does not establish
novelty or near-float accuracy at the tested high-dimensional storage budgets.
Production work remains unscheduled. A further study would need stronger optimized
quantization controls and a measured compressed-store execution win; learned
correction-bit selection and joint encoder/executor training remain untested.
The maintainer subsequently authorized a complete encoder/decoder redesign; the
JOURNAL entry "Encoder redesign is unconstrained by an installed format" proposes
sparse binary document codes and sparse small-integer query weights. A first
unsupervised fixed-activity COCO ablation is recorded in the 2026-09-22 JOURNAL:
2-bit query weights help, but the best tested sparse arm reaches only 20.02%
float-oracle recall@10, with a broad positive-score union. Every unfiltered
float top-10 neighbor was in that union, so ordering is the bottleneck. A
single-seed straight-through pairwise training follow-up ( 2026-09-22 JOURNAL )
raised recall on the small validation index but lost recall on the larger held-out
index, and increased positive candidates. A later size-matched, document-disjoint
validation check also failed to show a test gain. A 4,096-fit-query listwise
follow-up ( 2026-09-22 JOURNAL ) rejected every trained checkpoint on its larger
validation index, including an exploratory popularity-penalty arm and a
query-only ablation. An oracle-guided per-query search on 64 labeled queries
found 56.41% recall with the same fixed document codes and at most 32 integer
query weights, versus 23.91% for the shared query encoder on that exact slice.
Those labels also selected the weights, so this is a capacity witness, not a
serving result. The 2026-09-23 fit-only distillation and cross-index controls
resolved that witness: none of 31 learned linear query encoders beat the initial
encoder on validation, and oracle weights that doubled recall on the document
half which selected them produced no gain on a disjoint document half for the
same queries. Arbitrary per-query weight distillation is therefore not the next
experiment. The 2026-09-23 grouped categorical control supplies stable shared
features and exact eight-bit bitmap weights: its 64-byte full table reaches 70.43%
test recall, while a 32-category truncation reaches 57.66%. The existing 72-byte
residual decoder remains stronger at 76.66%. On the unfiltered 44,356-document
frame, yesno's fused persisted eight-plane evaluation takes 5.66-5.86 ms for the
truncated table and 26.20-26.42 ms for the full table, versus 1.26-1.27 ms for a
packed-code scan; the one-view fixture plus shared codebook occupies about 155.8
allocated bytes per document. The upstream checkout was dirty and unproven, so
these figures apply only to its fingerprinted state. The 2026-09-23 filtered and
bit-sliced follow-up rejects both remaining bitmap execution paths. Owner-targeted
persisted scoring takes 0.206-0.210 ms for 694 admitted documents versus
0.0183-0.0191 ms for packed gather. A matched 64-byte bit-sliced code performs
exact full-index top-k in 3.99-4.00 ms versus 2.10-2.12 ms packed and loses more
under scattered filters. Its smaller 16-category alphabet reaches 68.44% recall,
below both the 70.43% categorical control and 76.66% residual control. Do not ask
upstream for a materializing bitmap-integer JIT from this evidence. The subsequent
structured-code study rejects the pure norm-free route: the best 64-byte OPQ arm
reaches 72.54%. Rate-sharing the same 64 bytes between 500 residual signs and a
12-bit integer reciprocal norm reaches 76.15% on seed 7 and averages 76.42% across
three fixed encoder seeds, versus 76.76% for 512 signs plus a uint64 norm at 72
bytes. The three-seed paired difference is -0.35 points with a conditional
[-0.58, -0.10] point bootstrap interval. This is a new accuracy/bytes control, not parity.
An exact yesno decomposition uses 27 two's-complement intersection-count planes on the
selected model. It allocates 9,580,896 bytes of dense count vectors at 44,356
documents and makes fourteen payload passes. Adding the 12 exact norm planes
through the current API allocates 13,839,072 bytes and makes twenty passes.
Measured unfiltered complete top-10 takes 2.65-2.94 ms for resident packed
scoring, 3.93-3.97 ms for reopened persisted borrowed-row scoring, and
12.44-12.57 ms for the 39-plane score path. Sparse candidates widen the gap, so
count decomposition is only an exact oracle. The existing borrowed bitmap-row
surface is the useful persisted complement: it makes one pass over owner chunks,
returns no dense vectors and is only 1.35-1.49 times slower than resident packed
scoring unfiltered, but it does not beat the resident control and scattered
candidates expose whole-chunk amplification. No new fused weighted-top-k
terminal is prescribed for the embedded path; it would move the same loop
without a demonstrated gain. The frozen 64-byte codec has now passed its
new-split confirmation: three encoders retrained on a new same-size document
split average 76.31% test recall@10 versus 76.71% for the 72-byte control, with
all 1,024 new evaluation queries excluded from codec selection. This closes the
accuracy research gate. The production codec component is now built: it persists
the 500-sign decoder and norm quantizer under a content-derived model identity,
encodes the 64-byte boundary, rejects unbounded integer models, prepares exact
integer queries, and matches a separate scalar oracle. Core index metadata can
bind that identity and preserves it across reopen. The filter-aware embedded scan
is now integrated: core owns admission, borrowed row traversal, retry isolation and
exact integer top-k, while the prepared residual query supplies the row math after
checking the model identity. gRPC now loads that bound model, encodes float
documents without retaining them, exposes exact residual queries and refuses
ambiguous or binary interpretation of model-bound rows. The residual scan now
uses an exact shared-snapshot parallel path for unfiltered indexes at the measured
32,768-live-row crossover, capped at the measured eight-worker width; selective
queries stay serial. The supported native offline fitter now reproduces that construction with an
explicit seed and source fingerprint; its held-out validation is recorded in the
2026-09-23 JOURNAL entry. The incompatible SimHash plus retained-float rerank
surface has been removed; residual ingest is the sole float-to-code product path.
The 2026-09-21 expression-math prescription is delivered under
`../yesno/.agents-workspace/tmp/haiiie-expression-math-prescription-20260921/`.
It measures nested-map normalization and native/selective exact count kernels
against upstream's fused evaluator. Upstream integration and sparse-code accuracy
remain separate follow-ups; the new JOURNAL entry records the measurement frame.

### Storage-layout follow-up

- [ ] **dense-forward-read-profile**: on a 1,048,576-document D=256 binary
  fixture, a 2026-09-24 peer probe measured forced DenseScan at 16.98 ms,
  resident popcount at 2.55 ms and forced Inverted at 4.15 ms. A 2026-09-27
  quiet-window probe on the same fixture measured upstream
  `view_fold(interleaved(256), Any)` at 2.57 ms over resident FWD plus
  0.77 ms to load it, against DenseScan at 17.05-17.26 ms and Inverted at
  4.12-4.25 ms. Fold and top-k do different computations, so this is a lead,
  not a speedup estimate. Attribute elapsed time separately to cursor open,
  LIVE/admission, FWD chunk reads and borrowed-row extraction, masked
  popcount, per-block ranking and final top-k on the exact same corpus.
  The first same-fixture profile found 0.12 ms to open FWD, 1.04-1.14 ms
  for borrowed FWD chunk visits, 2.51-2.60 ms for borrowed row popcount,
  0.60 ms for LIVE admission, and 14.8-15.3 ms to construct/select each
  block's hits. A reused exact heap through k=128 now cuts the full forced
  DenseScan from 19.1-19.5 to 12.5-13.4 ms at k=10. Profile the remaining
  gap with exact admission and scoring together. The new tiled inverted
  accumulator changes the default-path baseline to 2.96-3.00 ms on this
  fixture. The 2026-10-03 quiet-host GloVe D=256 sweep ( yesno b712a03,
  haiiie ed930ae code, 1,048,576 documents, 200 Hamming top-10 queries
  with 114-157 set bits of 256 ) found the Gather/Inverted crossover between
  1 in 8 and 1 in 12, while old Auto switched after 1 in 24; at 1 in 24,
  Auto took 2.296 ms versus Gather's 1.643 ms. The wide-query multiplier is
  now 11, selecting Gather at 1 in 12 while retaining Inverted at 1 in 8.
  A pinned 40-query width sweep in `.agents-workspace/tmp/planner-grid-20261003/`
  found additional narrower-regime losses, but its fine crossover cells all
  failed the disk gate during an unrelated 200 MB/s scan. Re-run the width
  and density grid on a quiet host before changing those regimes. A qualified
  paired 200-query retake at one in 24 now measures old Auto at 2.212-2.487
  ms and new Auto at 1.647-1.671 ms; new Auto matches forced Gather within
  0.2%. The narrower-regime grid, not the wide Auto latency, remains open.
  Do not remove DIM on one query shape.

- [ ] **parallel-tiled-scaling**: on a 1,048,576-document D=256 persisted
  binary fixture, a 20-query eight-worker profile found no stable tiled win
  over CSA. The median critical worker spent 1.40 ms in tiled lane reads and
  accumulation versus 0.99 ms with CSA; per-worker cursor open, top-k and
  final merge did not explain the gap. Work stealing assigned one to four of
  the 16 blocks to a worker. Before changing scheduling or splitting blocks,
  distinguish uneven work from concurrent memory contention using matched
  block assignments and a quiet-host rebaseline. A same-fixture open-only
  comparison now measures about 0.32 ms serial/staggered versus 0.62 ms for
  eight concurrent workers; separate snapshots and a faster allocator do not
  remove the concurrent cost. Upstream holds a per-shard store mutex across
  each key's index range scan, so lock convoy is a lead. Different lane-open
  orders across workers measured 0.53 ms, but ordering and cache effects were
  not isolated. Prove an end-to-end gain before rotating keys locally; hold
  any upstream batched-open prescription until an implementation and benchmark
  exist. The 2026-09-28 JOURNAL entry gives both frames and limits.

- [ ] **split-forward-shard-profile**: yesnodb invariant I7 shards by key, so
  the single FWD key cannot spread across shards. A peer measured the largest
  of 32 shards holding 49.8% of bytes for a 1M-document D=256 binary index
  and 95.4% for a 96,903-document residual index. Evaluate fixed FWD key
  counts on the same corpora: 8, 32, 64 and 128 randomly placed keys occupy
  about 7.2, 20.4, 27.8 and 31.5 of 32 shards in expectation. Measure
  ingest, selective and full scans, shard byte distribution, stream-open cost
  and exact reopened results before choosing a layout. Key routing depends
  on the storage manifest; do not hard-code a current shard map.
  A `chunk % k` split would reuse FWD(0) and silently read only a subset
  of an old single-key index. Require a layout-version bump or a disjoint
  new kind, plus a reopen refusal regression, before changing routing.
  Splitting LIVE by document block is a separate candidate after FWD measurement.

- [ ] **pin-meta-before-index-width-change**: widening the 20-bit INDEX
  field moves today's META key if its address follows the generic layout.
  `open` would fail, but `--create-dims` could create a new empty index at the
  new location and strand the old one. Keep META at its current numeric key
  independently of INDEX width, probe the legacy location on create and open,
  and test that an old directory refuses silent creation before any such change.

### Ingestion performance after the FAISS comparison

The 2026-09-23 JOURNAL entries "Ingestion plan after the FAISS comparison"
and "First encoder optimization" give the construction, priorities, measurements
and remaining acceptance gates.

- [x] **residual-encode-first-pass**: reconstruct decoder norms with contiguous
  exact integer accumulation; encode bounded gRPC chunks in parallel and apply
  mutations through one ordered writer. The full COCO-model encoder check matched
  all 96,903 old 64-byte codes; the journal records timings and the test frame.
- [ ] **residual-encode-pipeline**: sign selection was measured at 13.84-13.89 s
  of a 15.0 s serial encode; a bounded four-accumulator path now reduces the
  complete serial encode to 8.15-8.21 s with every COCO code byte unchanged.
  The direct packed build takes 8.595-8.672 s. A bounded one-tile producer
  reduced measured encode/write time from 1.312-1.347 s to 0.725-0.936 s on
  96,903 rows with 95 matched commits; it is now in streamed ingest. Loopback
  gRPC remains noisy ( 1.834-3.720 s, overlapping the earlier range ).
  Reusable 512-element worker buffers matched every code but saved only
  0.011-0.031 s per 96,903 serial encodes, so no cross-crate scratch API was
  added. A one-request-at-a-time loopback harness reached 54-56 MiB process
  high-water; that includes transport and store memory, not just the queue.
  Same-index fixed-cohort reader p99 remains open because two of three runs
  exceeded the disk-activity gate. Keep transient floats and duplicate-ID order.
- [x] **residual-forward-only-storage**: model-bound residual indexes now persist
  FWD, LIVE, ATTR and META without DIM, ZPLANE or STAT. Durable churn and
  compaction tests cover the layout; core binary search refuses it. The
  2026-09-23 JOURNAL entry records the same-corpus writer and disk measurements.
- [x] **bulk-packed-chunk-ingest**: fresh aligned residual tiles use native
  chunk patches, with the ordered point writer retained for all other mutations.
  Byte-exact reopened-row, WAL-only recovery, churn, loopback gRPC and
  matched-commit controls are recorded in the 2026-09-24 JOURNAL entries.
  Upstream `f7f9daf` passed its SHA-stamped four gates; haiiie's boxed `Op`
  and the SHA-pinned point/packed rebaseline closed the local width regression.

### Production blockers -- fixed 2026-09-14

Instrument at `.agents-workspace/tmp/prodcheck`. These were found by asking
"what would stop this being deployed" after M8, which is later than it should have been
asked -- eight milestones of correctness work never measured ingest at all.

- [x] **ingest-is-o-of-dims-per-document** -- fixed. `Writer::put` clears the old state
  unconditionally -- `dims` removes for the posting lists, `row_bits` removes for the
  forward row -- so a **fresh** insert pays `O(D)` for clearing nothing. Measured **784
  batch operations per document at D=256**, of which roughly 521 are that clearing.
  The fix is cheap and safe: consult the live set and skip the clear path for an id that
  is not present. Expect ~3x fewer operations, and `O(|x|)` rather than `O(D)`, which is
  the right complexity.

- [x] **ingest-accumulates-one-unbounded-batch** -- fixed for the service. an ingest stream builds a single
  in-memory batch, chosen deliberately so a document's code and liveness commit
  atomically. Measured **~3 GiB for 262 144 documents** and a **101 s** commit; ten
  million documents exhausts memory before finishing. **2 500 docs/s** end to end.
  The atomicity that motivated it is per document, not per stream, so chunked commits
  would keep the property that matters. Needs a decision on what a partially-ingested
  stream means to a reader, which is why it is not a one-line change.

- [x] **filter-selectivity-does-not-reduce-work** -- fixed. Originally measured at
  262 144 documents, D=256:

  ```text
  filter         inverted   forward   admitted
  none             1.67 ms   158 ms     262 144
  1 in 100         1.63 ms    88 ms       2 622
  1 in 10 000      1.55 ms   2.76 ms         27
  ```

  Three separate faults. **The inverted path is flat in selectivity by construction** --
  it reads every query dimension's posting list for any block holding one admitted
  document. **The forward path materializes the entire forward index per query**: it
  calls a whole-key load per forward block and caches by block, so a scan touches all
  1 024 of them and allocates hundreds of megabytes. It should use the same targeted
  block read the inverted path was given at M3 and never got. **And the planner picks
  inverted unconditionally**, so the path that would win at high selectivity is never
  chosen. Fixing the forward read is the prerequisite for the planner mattering.

  All three fixed. The forward path takes a targeted block read ( 88 ms to 1.64 ms at
  1 in 100 ), and `Auto` now chooses per block from a measured crossing at **1 in 256**.

  **The first crossing constant was 1 in 16 and was wrong**, taken from a forward path
  that was reading only its first block -- so it scanned a fraction of the documents and
  looked far faster than it is. The benchmark could not see that; a test asserting the
  two forced paths agree could, and did. A constant derived from a measurement taken
  through a defect is worse than no constant, because it carries the authority of a
  number.

### Production limits that remain

- [x] **ingest-order-was-the-ceiling** -- fixed. **37 400 documents/second**, commit
  6.3 s for 262 144 documents, against 7 200/s and 35 s before. `Batch::group_by_key`
  stable-sorts by key before commit.

  The intended fix was columnar accumulation over `WriteBatch::merge_set`, and
  **measuring first showed that premise was false**: merge_set is 598 ms against
  531 ms for 8.4 M dense inserts, a 1.1x that would not have paid for the rewrite.
  Upstream's own doc says why -- it folds onto the ordinary insert path, so it is an
  atomicity fix rather than a bulk-write one.

  The real cause was **insertion order**. The same 8.4 M inserts over 256 keys cost
  554 ms key-major and 7 712 ms document-major -- **14x for the order alone**. Ingest
  is document-major by nature, so it was paying that throughout. Reported upstream,
  because every yesnodb operation is scoped to a single key and ops on different keys
  commute, so the same sort inside `WriteBatch::commit` would give every caller the
  win rather than only the callers who know to ask for it. **Upstream reproduced it
  independently and shipped the fix the same day** ( 22.2x on their fixture; cause was
  `plan_ops` coalescing only consecutive same-key runs, so document-major flushed once
  per operation ).

- [x] **our-pre-sort-is-not-redundant-with-theirs** -- checked rather than assumed, **twice**.
  Going to delete `group_by_key` now that upstream sorts in `commit`, measured first:
  **5.5 s commit with it against 16.4 s without**, both fixes present, 2.8x. Both runs
  perform exactly one sort -- ours makes their `keys_ascending` flag skip theirs.
  **That inference was wrong and is disproven.** Upstream tested moving the sort ahead
  of bucketing: better at 4 shards, worse at 16, correctly not taken. And the
  discriminating experiment they suggested settles it -- at haiiie's shape ( 67 M
  operations, ~1 300 keys ), three runs per arm:

  ```text
  shards   push ( our loop )                commit ( theirs )
       1   1204-1386 vs 1323-1539 ms        8075-8495 vs 2008-2142 ms   4.0x
       8   1219-1601 vs 1372-1504 ms        7955-8311 vs 2171-2247 ms   3.6x
  ```

  Push ranges **overlap** in both: our translation loop is order-insensitive, so the
  residual is entirely inside `commit`. And it is 4.0x at **one shard**, where bucketing
  structurally cannot scatter -- so bucketing was never the cause.

  What the numbers do say is that the residual scales with batch size rather than shard
  count: upstream measured 1.50x at one shard on a 2 M-operation fixture and this is
  4.0x at one shard on 67 M. Reported with the inference that their in-`commit` sort is
  over `Vec<&Op>`, so every comparison is a pointer deref -- the same effect as the
  12% they measured for an `is_sorted_by_key` scan at 2 M, at 30x the scale.

- [x] **a-replacement-now-writes-only-the-difference** ( 2026-09-16 ): `Writer::put` on an
  id the store already holds reads the stored row back and emits operations only for bits
  that changed. The comment it replaces said the old code "is not known without reading it
  back, so clearing cannot be narrowed" -- correct in its first clause and a non-sequitur
  in its second. Reading it back costs one block read per `rows_per_block` documents.

  Worst-case reader stall under a concurrent writer, 262 144 documents, checkpoints
  deferred so none run, 40 000 rows in every arm:

  ```text
  batch    before   same code   changed
    500   27.17ms      2.43ms    7.78ms
   5000  121.97ms      5.09ms   24.15ms
  20000  485.96ms      8.97ms   95.05ms
  ```

  **Quote the `changed` column: about 5x.** The `same code` column is the idempotent
  re-put -- the documented stream-resume path -- where the difference is empty by
  construction and the gain is 24x to 54x. Every pre-change arm was measured that way
  without anyone noticing, which would have made the headline an order of magnitude too
  good. A realistic update still costs about twice a fresh insert, and that part is
  irreducible: changing a bit means both clearing and setting it.

  Within-batch re-puts keep clearing blindly, because their old row exists only in the
  batch and no snapshot can see it. Correctness first, and the case is rare.

  **That fallback was wrong, and had never once run.** 2026-09-16: both guards deciding
  whether an id was written in this batch held **one block** and were replaced when a
  `put` reached another. Ascending ingest never revisits a block, so nothing in the suite
  drove either. An update batch keyed by arbitrary ids does, and the API permits it:
  `put(0, a); put(70_000, b); put(0, c)` left doc 0 holding `a | c` -- weight 6 where the
  last code has 3, and every search then answering correctly from a row that was never
  stored. Two independent instances, one in the batch-written set and one in the
  live-block cache behind `is_present`, plus a third defect found while fixing them: the
  cache patch discarded the block id and wrote the offset into whichever block was
  cached.

  Fixed by making the batch-written record a set of ids rather than one block's mask,
  cleared at `flush_if_large` because a committed prefix is genuinely in the store by
  then. `reput.rs` pins it: two targeted regressions and one randomized arm over three
  blocks with deliberate repeats, checked against last-write-wins. Sabotage-verified --
  reverting either fix is caught, and the randomized arm catches both on its own.

  **Why it is 5x and not 2x, measured 2026-09-16 after upstream asked.** The entry above
  reports a speedup and no operation count, so the two could not be compared. Counting
  them on the *exact* pairing the `changed` arm uses -- a code from `BASE / 3` away in
  the same corpus, `Shape::Balanced`, D=256, density exactly 0.500 -- blind clearing
  emits 256.0 operations per put and diffing emits 128.0 plus 4.00 z-plane moves, an
  op-count reduction of **1.94x**. So roughly 2.6x of the measured 5x is *not* bought by
  emitting fewer operations.

  yesno-7c supplies the mechanism and it is the one above: a commit's cost tracks rows
  touched rather than work done, because every unit pays a tree descent under the
  exclusive lock. Removing a unit therefore saves a descent, not a bit-flip. Their
  inference arrived before either side had the op count; this is it confirmed rather
  than assumed, and it is the better reading of our own number.

- [ ] **ingest-is-still-one-op-per-set-bit**: **and it is a reader-latency item, not only
  a throughput one** ( 2026-09-16 ). At identical batch size and identical total rows, an
  overwrite holds the storage layer's memtable lock **12x to 39x longer** than a fresh
  insert, because `Writer::put` on an existing id emits a clear for every dimension before
  setting the new row -- roughly twice the operations, all of them inside the exclusion a
  concurrent reader waits on.

  ```text
  writer            commits   median      p99    p99.9      max
  ( no writer )           0   0.86ms   0.95ms   0.98ms   1.88ms
  5000 fresh              8   1.20ms   4.43ms   7.49ms  10.47ms
  20000 fresh             2   1.05ms   2.38ms   7.56ms  12.60ms
  5000 overwrite          8   0.86ms   1.89ms  83.06ms 121.97ms
  20000 overwrite         2   0.84ms   0.89ms 159.01ms 485.96ms
  ```

  So the write amplification this entry is about lands on **query tail latency**, not just
  on ingest rate, and a large share of the 82 ms p99 previously blamed on the storage
  layer's commit path is haiiie's own operation count arriving inside someone else's lock.

  Two further facts from the same grid. The hold is **per-byte, not per-commit**: worst
  case grows monotonically with batch size in both modes ( 5.47 to 23.73 ms across a 100x
  range for fresh inserts ), so a shorter critical section upstream is not the lever and
  the structure is. And **batch size is a tail-shape knob** for the same structural reason
  shard count is -- many small commits give many mild stalls, few large ones give few
  severe ones, with 20 000-row overwrite commits producing a p99 *better* than no writer
  at all and a worst case of 486 ms.

  Original entry: 263 batch operations per document at
  D=256. Columnar accumulation would reduce the operation *count*, but the measurement
  above says the count was never what dominated -- so this is now a much weaker item
  than it looked, and wants its own measurement before anyone acts on it.

  **That last sentence was superseded and the entry did not say so.** Upstream then
  measured the cause rather than the effect: 71-80% of the memtable write-lock hold is a
  tree descent and a checksum-verified container read per unit, fired once per row *even
  for a key that does not exist*, so cost tracks **operations touched** rather than work
  done. Our own op count confirmed it -- the diffing writer's 1.94x fewer operations
  bought 5x. So the count is exactly what dominates, and "a much weaker item" was written
  from the measurement that could not see the mechanism.

  **Half of it is now taken, 2026-09-17, using new upstream API.** `WriteBatch` gained
  `insert_range` / `remove_range`, whose own documentation gives the reason: inserting a
  span ordinal by ordinal costs a WAL record and a copy-on-write clone **each**. A forward
  row is `row_bits` consecutive ordinals by construction, so the two places that clear a
  whole row -- `delete`, and `put`'s blind-clearing fallback -- are one operation now
  instead of `row_bits`. Counted in haiiie's own `Batch`, before the storage layer:

  ```text
  D      delete ops   was   ratio
  128           139   266   1.91x
  256           268   523   1.95x
  512           525  1036   1.97x
  ```

  A delete costs about half what it did, and the ratio grows with the code width. What is
  left of this entry is the **posting lists**: one document contributes one ordinal to
  each of `|code|` different `DIM` keys, which no range or set form can fold, because the
  ordinals are spread across keys rather than within one. `merge_set` folds the other
  transpose -- many ordinals into one key -- and would need the writer to accumulate
  column-major across a whole batch.

  **Measured 2026-09-18, and it does not pay.** Upstream's cost model for `merge_set` is
  explicit that a merged set costs by **run count**, not cardinality, and it names the bad
  case: "a set of every *other* ordinal has one run per element". A per-dimension posting
  set within one batch is exactly that shape -- the documents in a contiguous id range
  whose bit `d` is set, which is a random `delta`-dense subset of that range. Counting
  runs in the sets haiiie would hand it:

  ```text
  batch    dims  density   set bits/dim   runs/dim   fold
   1 000    256    0.500          500.0      250.1   2.00x
  20 000    256    0.500       10 006.0     4 998.9  2.00x
  20 000   1024    0.500       10 002.8     5 000.1  2.00x
   1 000    256   sparse           15.9       15.7   1.01x
  20 000    256   sparse          315.1      309.9   1.02x
  ```

  **The fold is `1 / (1 - delta)`**, which the data reproduces exactly and which needs no
  re-measuring on another corpus: a random `delta`-dense subset has `N * delta * (1 - delta)`
  runs against `N * delta` members. So it is 2x at the density a sign or SimHash code
  produces, and **essentially nothing for sparse codes** -- which is the use case that
  would most want a faster ingest.

  Against that: the writer would have to hold per-dimension sets for a whole batch, which
  is the memory shape the ingest path already flushes specifically to avoid ( one batch
  for a whole stream measured at ~3 GiB per 262 144 documents ). A 2x fold on one of the
  three operation families, available only at high density, does not buy a restructure
  that fights the flushing strategy. **Closed unless the writer is being rebuilt for
  another reason**; the range work above took the part that was free.
- [ ] **embedded-ingest-needs-explicit-flushing**: `flush_if_large` bounds memory and
  the server calls it between stream messages; an embedded caller driving a writer
  directly must call it too. Not automatic on purpose -- an implicit flush would commit
  a prefix without the caller asking, which changes what a failure leaves behind.
- [x] **latency-is-super-linear-in-the-corpus** -- **mostly not, and the curve that said
  so was measuring two defects.** It was `N^1.18` ( 1.6 / 7.9 / 19 ms at 262 144 / 1 M /
  2 M ). After the run-container fill and the held-open cursors it is **`N^1.07`**
  ( 0.9 / 3.9 / 8.2 ms ), which is close enough to linear that the remaining curvature
  is not worth a hypothesis. The 10 M extrapolation moves from ~130 ms to ~44 ms and is
  still an extrapolation -- see `nothing-measured-above-2-million`.

  Worth keeping: the super-linearity was read as evidence for a cache/bandwidth story,
  and it was two fixable defects in the read path. A curve is not a mechanism.
- [x] **nothing-measured-above-2-million** -- measured to **16 777 216**, six points over
  a 64x range, and the extrapolation held for once:

  ```text
   documents   ingest/s   serial ms   8 threads   1 in 1024
     262 144     31 882    0.9          0.8         0.3
   1 048 576     32 373    4.0          2.0-2.1     1.1-1.2
   2 097 152     32 125    8.2-8.3      3.9-4.0     2.3-2.4
   4 194 304     32 871   16.9-17.1     6.6-7.3     4.6-4.7
   8 388 608     32 478   34.9-35.3    12.8-14.6   10.2-10.3
  16 777 216     33 939   72.8-73.5    24.6-25.2   24.4-24.5
  ```

  Least squares on log-log: `latency = 1.479e-6 * N^1.066`, worst residual **3.3%**. The
  README's 43 ms at ten million is the fit's own number rather than a linear guess, and
  **ingest is flat at ~32 000 docs/s to sixteen million** -- no knee anywhere in the
  range.

  Two things the extra points show that three could not. Parallel speedup **improves**
  with corpus size ( 2.1x at two million, 2.9x at sixteen ), because thread startup is
  fixed against a per-block gain and there are more blocks to go round. And the filtered
  column scales slightly *worse* than the unfiltered one -- a 1-in-1024 filter is 3.0x
  cheaper at 262 144 and 3.0x at sixteen million, but 3.4x at eight, which is the forward
  path's own curve showing through and is not yet explained.

- [x] **nothing-measured-above-16-million** -- **checked at 33 554 432 on 2026-09-19, and
  the extrapolation held.** Two prior extrapolations from this table were contradicted by
  later measurement; this is the third and the first to survive.

  ```text
   documents   ingest/s   serial ms    8 threads   1 in 1024
   2 097 152     24 510   9.1-10.3     4.6-6.6     2.3-2.4     <- control
  33 554 432     28 936   161.5-166.7  63.8-91.6   83.6-86.1
  ```

  **The control is what makes the new row readable.** The machine was 1.20x slower that
  day than when the six-point table was taken ( 9.7 ms against the fit's 8.1 at two
  million ), so the like-for-like prediction at 33 million is **186 ms**, not the fit's
  156. Measured 164 -- **0.88x the adjusted prediction**, beating it. Within-run exponent
  across the two same-day points is **N^1.020**, against N^1.066 from six points.

  **The filtered column does not scale, and this is the first measurement that shows it.**
  At one in 1024 the same two points give 2.3 ms to 84.8 ms, a 36-fold rise across a
  16-fold corpus. **That was first written here as "N^1.294", which is an average across a
  knee quoted as though it were a law.** Reading the six-point sweep consecutively -- six
  points already on one machine state, already in this file -- shows filtered flat to four
  million ( N^0.969, N^1.031, N^0.985 ) and degrading after it ( N^1.140, then N^1.254 ),
  while serial stays at 1.04 to 1.08 throughout. The open item below carries the table and
  the corrected reading.

  One instrument fault found and worth stating: the fitter printed `worst residual 0.0%`
  for a **two-point** fit. Two points fit a two-parameter model exactly, so that number is
  vacuous and reads as a quality claim. It is the fit over six points, quoted above, that
  has a real 3.3% residual.
- [ ] **filtered-queries-stop-scaling-at-four-million**: **there is a knee, and it is in
  data this project already had.** **Partially improved 2026-09-20, still open:**
  forward scoring now borrows aligned bitmap words instead of copying 8 KiB per
  visited chunk. On a new persisted D=256, Hamming, k=10, every-1024th-ID fixture,
  five warm runs at 4 194 304 documents changed from **4.30-4.45 to 2.22-2.23 ms**;
  at 16 777 216, **24.86-25.02 to 12.90-13.03 ms**. Complete Hits match an independent
  oracle and forced Inverted. This is a separate bulk-built, no-STAT fixture, not a
  replacement measurement of the historical corpus below.

  A private upstream-copy diagnostic found a second mechanism: at 16 777 216,
  every warm sparse forward scan rechecks **677 payloads / 5 545 984 bytes** under
  the 16 384-entry verification-cache generation; at 4 and 8 million, none.
  Doubling that capacity only in the diagnostic removes those misses and changes
  the raw container-read loop from **7.18-7.37 to 5.40-5.49 ms**, five samples in
  each of two passes. Checksums were never disabled. This is not an end-to-end
  fix or a safe cache-policy prescription, and upstream was not changed.
  Next: profile the remaining query overhead and validate a bounded cache policy
  before proposing an upstream change. Planner boundaries also need a fresh full
  density/width grid. Construction and controls are in the 2026-09-20 JOURNAL entries.

  **Follow-up, 2026-09-20:** LIVE and attribute stream plans were also being
  rebuilt per posting block. A timing wrapper measured their combined cost at
  **0.37-0.40 ms at 4M, 4.02-4.07 ms at 16M**, despite only four times as many
  blocks. Scoring scans now retain one admission cursor per worker, with separate
  lanes for repeated term occurrences and fresh cursors after eviction. On the
  same persisted fixture, five warm full-query samples changed from
  **2.23-2.24 to 1.87-1.93 ms at 4M**, **4.76-4.80 to 3.65-3.83 ms at 8M**, and
  **13.20-13.39 to 8.88-8.92 ms at 16M**. A related pre-existing retry bug is fixed:
  partial hits are discarded even if the retried block is now empty or filtered out.

  Extended the same construction to **33 554 432** documents: exact results,
  32 768 admitted/scored, 512 visited blocks, zero skipped; full-query batches
  **47.14-47.68 and 51.93-52.29 ms**, five warm samples each. There is no before
  measurement of this new 33M fixture. The private raw-read probe now records
  **34 119 checksum misses / 269 818 880 bytes per warm scan** with the actual
  16 384-entry generation limit. The 32 768-capacity control reduces that to
  **1 351 / 11 067 392 bytes**, and raw-loop timing from **41.79-42.14 to
  14.37-14.84 ms** across two five-sample passes. These are raw loops, not query
  speedups. Upstream remains untouched; a safe bounded-cache policy is still
  unproven. A further local improvement now opens inverted scoring lanes only
  when a block actually chooses Inverted, retaining the declined-cursor state
  and resetting both scoring cursor families on retry. Gather opens no inverted
  lanes. On the same fixture a fresh five-sample 16M baseline was **8.88-8.95 ms**;
  two five-sample batches after lazy setup were **8.23-8.44 and 8.44-8.60 ms**.
  Raw-read controls also shifted, so this is a modest observed improvement, not
  an isolated setup-cost subtraction. The nine-cell planner screen still chooses
  the better path in eight cells; a full boundary grid remains outstanding.
  `count()` now reuses admission lanes too, and `explain()` holds its LIVE lane.
  On this fixture at 16M, five samples changed from **5.84-5.85 to 0.262-0.273 ms**
  for a one-in-1024 count, and from **2.14-3.07 to 0.252-0.255 ms** for explain;
  a second post-change batch confirmed **0.257-0.262 and 0.252-0.256 ms**. These
  helper timings are not search latency. The scaling item stays open. Adapter
  reopen behavior across missing chunks was not optimized by these changes.

  **Historical diagnosis follows.** The entry first said "N^1.294", from two same-day
  points across a 16x range, and closed by noting that two points are a slope and the next
  step is intermediate sizes on one machine state. The six-point sweep **is** six points on
  one machine state, and reading it consecutively gives the shape for free:

  ```text
  step                          serial     filtered
     262 144 ->   1 048 576    N^1.076     N^0.969
   1 048 576 ->   2 097 152    N^1.044     N^1.031
   2 097 152 ->   4 194 304    N^1.043     N^0.985
   4 194 304 ->   8 388 608    N^1.046     N^1.140
   8 388 608 ->  16 777 216    N^1.059     N^1.254
  ```

  Serial is flat across the whole range. **Filtered is flat to four million and degrades
  after it**, and cost per admitted document says the same: 1.15 us from 262 144 through
  4 194 304, then 1.25 at eight million and 1.49 at sixteen. The 33 554 432 point is
  consistent with continued degradation.

  So `N^1.294` was an **average across a knee**, quoted as though it were a law, and it
  understates the small sizes and overstates nothing at the large ones. Corrected here and
  in `README.md` within the hour, which is the only reason it is worth writing down: the
  two-point number was published before the six-point data it contradicts was looked at,
  and that data was already in this file.

  **Measured 2026-09-19: it is costlier blocks, not more of them.** `ScanStats` already
  reports the block count, so the division needed no new instrumentation:

  ```text
   documents    filt ms   visited   skipped   us per block
   4 194 304       5.62        64         0           87.8
  16 777 216      29.18       256         0          114.0
  ```

  Blocks visited grew **exactly 4x** across a 4x corpus -- linear, as the model says -- and
  the per-block cost grew **1.30x**. Their product is the 5.19x in total time, and the
  implied exponent is `1 + log(1.30)/log(4) = 1.19`, which brackets the 1.14 and 1.25 the
  consecutive steps show.

  **And the per-block figure says where to look.** 87.8 microseconds to score 64 admitted
  documents in a block is about a hundred times the cost of moving the data, so the 8 KiB
  container read is not one read -- it is **one per admitted document**. Those 64
  documents sit in up to 64 *different* forward blocks, because a forward block holds
  `rows_per_block` documents and the admitted ones are scattered across the scan block's
  65 536 ordinals. So a filtered query reads about `admitted * 8 KiB`, which at 4 million
  is 512 KiB per scan block and lands near memory bandwidth. That is
  `filters-below-1-in-384-do-nothing`'s read-granularity problem, now with the filtered
  scaling curve attached to it.

  At that time the **1.30x** remained unexplained: the same number of reads,
  individually slower on a bigger file. Locality was a hypothesis, not a measurement.
  The new control above identifies verification-cache churn as one contributor;
  it does not establish that it explains all of this older corpus's slowdown.

  **What was unmeasured before this, and the note that stood here:** At a fixed selectivity the admitted rows
  per block are constant, so blocks touched grows linearly and the per-block cost is what
  rises. Candidates: B+tree descent depth inside the forward key, whose chunk count is
  `N * row_bits / 65536` and so grows with the corpus -- 8 192 chunks at two million
  against 65 536 at sixteen, which is three more levels and about the right size for a 30%
  rise; or a cache level being outgrown. `ScanStats` already reports `blocks_visited`, so
  the first diagnostic is time per visited block at two sizes, which needs no new
  instrumentation.

  A filtered query is still about a third the cost of an unfiltered one at every measured
  size. This is an eroding advantage, not an inversion.
- [ ] **nothing-measured-above-33-million**: the fit extrapolates to 0.5 s at a hundred
  million and 5.8 s at a billion, single-threaded. The second is **sixty times** beyond
  anything run, and the design documents talk about `N = 1e9` throughout. Two prior
  extrapolations from this same table were contradicted by later measurement; there is no
  reason to think the third is different until someone runs it. Storage is not the
  obstacle -- sixteen million documents is about 1 GiB, so a billion is ~64 GiB -- and
  ingest at a flat 32 000 docs/s puts a billion-document build at about nine hours, which
  is the real cost of finding out.
- [ ] **filters-below-1-in-384-do-nothing**: at 1 in 16 the cost is the unfiltered cost
  ( 0.92 against 0.89 ms at 262 144; 8.5 against 8.2 ms at 2 M ). Renamed from
  `...-1-in-256-...`: the crossover moved when the inverted path got faster. The pitched
  use case is filtered search, and the *common* selectivities -- a tenant with a tenth of
  the corpus, a frequent category -- fall entirely in the flat region. Making the
  inverted path selectivity-sensitive, rather than switching to the forward path only at
  the extreme, is the open problem.

  **Now measured from the other side too, and the forward path's remaining cost is a
  read-granularity problem.** Giving it a per-block top-k took it from 22.01 to 5.31 ms
  unfiltered ( 4.1x ) and 2.23 to 1.44 ms at 1 in 16, and **moved the crossover not at
  all** -- at high selectivity there were never many hits to sort. What is left is that a
  forward block is read 8 KiB at a time to extract one 32-byte row, and admitted
  documents scattered at 1 in 256 touch nearly every forward block anyway: ~5 MiB read to
  score 1 024 rows at 262 144 documents. So the forward path costs
  `min( admitted, rows_per_block ) * 8 KiB` per block, not `admitted * row_bytes`.

  That is the factor of ~200 between the plan's byte-model crossover ( near 1 in 2 ) and
  the measured one. The plan's cost table **did** name the effect -- "line
  amplification" -- but priced it at a 64-byte cache line rather than an 8 KiB container,
  so the term was present and three orders of magnitude small.

  **Updated 2026-09-20:** that full-copy account is historical. Borrowing aligned
  bitmap words is now built using an upstream API that already existed; no new
  sub-range API was needed. Other representations still expand a mask, and a
  verification-cache miss still checks the complete payload. Offline weight-sorted
  compaction is also built, but weight order does not guarantee arbitrary attribute
  clustering. The common-selectivity problem remains open; the current planner is
  query-width-sensitive, so this slug's historical 1-in-384 is not one universal
  current cutoff. A fresh full crossover grid is needed after the borrowing change.
- [x] **no-parallelism** -- built. `Search::threads(n)`, work-stolen per block through
  one atomic counter, no shared mutable state on the hot path, one shared snapshot.
  Results are **byte-identical at any thread count** and `m6_parallel.rs` asserts it
  across metrics, k, forced paths, filters, and repeated runs at the same count.

  **~1.3x at four to eight threads**, measured after the read-path work below: 3.6 to
  3.2 ms at 1 M documents, 8.3 to 5.9 ms at 2 M, and **twenty threads is no better than
  serial**. Nothing at 262 144, where the corpus is four blocks and the path does not
  engage. The sentence that used to stand here -- "not a defect to tune, the scan is
  memory-bandwidth bound" -- was wrong, and `parallel-scaling-is-unexplained` below
  replaces it.

- [x] **the-real-ceiling-is-memory-bandwidth** -- **refuted as stated, 2026-09-14, then
  made true by fixing the code.** Two indirect signals had been read as bandwidth:
  latency growing as `N^1.2`, and parallel speedup capping at 2x. The direct test cost
  one instrument and said otherwise. Three arms on this machine ( 20 cores, Cortex-X925
  + Cortex-A725, 24 MiB L3 ):

  ```text
  streaming read, whole buffer per thread     36.3 GB/s @1    125.1 GB/s @20   3.4x
  compute-bound, resident in L1              444.7 Mops/s     6337.5 @20      14.3x
  3 776 scattered 8 KiB chunks ( the scan )   12.3 GB/s @1     95.6 GB/s @20   7.8x
  haiiie, 30.9 MB per query at 2 M docs        1.62 GB/s        3.25 GB/s      2.0x
  ```

  The scan was at **13% of its own access pattern's single-thread ceiling**, so it was
  not bandwidth-bound; and the compute arm scaling 14.3x ruled out the other candidate
  the original conclusion never tested, that the cores are uneven. Both fixes below came
  from the ablation that followed. After them the read path achieves 12.6 GB/s against
  that 12.3 GB/s ceiling -- so the claim is now true, having been false when it was made.

- [x] **live-mask-scattered-bit-by-bit** -- fixed. `LIVE` is a contiguous ordinal range,
  so every chunk of it is a single whole-chunk run, and `load_block` scattered it one
  position at a time: 65 536 iterations to build 1 024 words of all ones, 110 us per
  block against 2.3 us for a dimension read, **19% of a whole query**. Run containers now
  fill word ranges. 3.52 ms to 0.05 ms at 2 M documents.

- [x] **one-stream-open-per-dimension-per-block** -- fixed. `load_block` is addressed by
  `( key, block )` and so reopened a key stream 3 776 times per query to read bytes that
  take a fraction as long. `SetSnapshot::open_lanes` returns a cursor over a fixed key
  list, held open across blocks: **8.68 ms to 2.27 ms** for the same bytes. Whole query
  at 2 M: 19.1 to 8.3 ms serial, and the latency curve flattened from `N^1.18` to
  `N^1.07`. `SELECTIVITY_CROSSOVER` moved 256 to 384 as a consequence -- it is a ratio
  between two paths, so improving one moves it.

- [x] **parallel-scaling-is-unexplained** -- **found, 2026-09-14: the shard count.**
  `KeyStream::next_chunk` takes `Mutex<ShardStore>` once per chunk and holds it across
  the decode, and yesnodb's default is **eight shards**, so at most eight threads can
  decode at a time against the 3 776 chunk reads a query issues. Confirmed by sweeping
  the count with one shard as the control, where twenty threads deliver 0.36x of one
  thread; scaling then climbs monotonically to 5.56x at sixty-four.

  **Re-run 2026-09-18 on the same instrument: the shape reproduces and the headline ratio
  does not.** Twice, back to back: **4.23x** at sixty-four both times against the 5.56x
  recorded here, with the single-shard control at 0.34x and then 0.57x against the
  recorded 0.36x. Absolute throughput is lower across the whole sweep, which points at the
  machine rather than the code -- nothing in the read path changed this week beyond a
  per-scan weight calculation.

  Annotated rather than rewritten, because **the conclusion does not rest on the headline
  number.** What this item claims is that the shard count is the concurrent-reader ceiling,
  and the evidence is the monotone climb plus the sub-1x single-shard control, both of
  which reproduced. A ratio between the two noisiest endpoints of a sweep is the least
  stable figure it produces and the most quotable -- a bad combination, and this one was
  quoted out of this entry earlier the same day.

  **Parallel speedup at the default was 1.06x on twenty cores**, which is to say the
  parallel scan had been very nearly pointless since it was built. `DEFAULT_SHARDS = 32`
  doubles it for 12% of ingest rate, and `YesnoStore::open_with_shards` exists because
  the count is fixed in the MANIFEST and cannot be revised without a rebuild.

  The refcount hypothesis recorded here was **wrong**; it was sent upstream labelled
  untested, which is the only reason it cost nothing.

- [ ] **commits-stall-in-flight-queries-more-than-checkpoints-do** ( this item used to be
  named for checkpoints, which was the conclusion rather than the subject -- renamed
  2026-09-16 when a measurement separated the two; the old name is written out here
  rather than in backticks because a provenance note is not a citation and the slug check
  rightly could not tell the difference ):
  with checkpoints **deferred so that none run**, a concurrent writer still takes query
  p99 from 4.38 ms to 82.61 ms. Nineteenfold, zero checkpoints.

  ```text
  while querying                 median       p99      worst   ckpts
  no writer                      3.97ms    4.38ms     6.79ms       0
  writer, default policy         4.24ms   83.18ms   363.76ms       2
  writer, checkpoints deferred   4.13ms   82.61ms   138.29ms       0
  ```

  The two costs separate cleanly. **Commits own the tail** -- whatever a commit holds, a
  concurrent query waits for. **Checkpoints own the worst case** -- 138 ms becomes 364 ms.
  Everything below this line was measured about the second and attributed to it; most of
  it belongs to the first.

  **How the attribution went wrong is the part worth keeping.** The metric was *excess
  latency above the no-writer median, divided by the number of checkpoints*. That
  denominator presupposes the cause: it can only express the cost as a per-checkpoint
  quantity, it is undefined when no checkpoint runs, and its values were then read as
  evidence that checkpoints caused the excess. A measurement whose units assume the
  conclusion cannot test it. The control that settles it -- a writer with zero
  checkpoints -- was unreachable through that metric by construction.

  Follows upstream's candidate (2), the memtable `RwLock` and the shard write lock, which
  neither side had measured and which is now the one with evidence behind it.

  **Upstream has since measured the cause under the effect, and it makes the fix smaller
  rather than larger** ( yesno-7c, 2026-09-16 ). Frame: their numbers are *per-commit
  memtable write-lock hold*, measured inside yesnodb at 20 000 resident rows with the
  memtable cold after a reopen, 4 000 rows per arm. Ours above is *haiiie reader p99* at
  262 144 documents. Different surfaces, and they compose rather than compete: ours says
  commits own the tail, theirs says what inside a commit owns it.

  **71-80% of that hold is disk I/O, not memtable mutation.** Each unit's memtable miss
  falls back to a tree descent and a checksum-verified container read, all inside the
  exclusive hold, and **it fires once per row even for a key that does not exist** -- a
  fresh insert into a brand-new key still pays a descent under the lock to learn the
  chunk is absent.

  ```text
    mode        rows/commit   hold per commit   disk_chunk share
    insert               10           13.1us              70.9%
    insert             1000          585.7us              80.3%
    overwrite            10           19.4us              75.4%
    overwrite          1000          881.3us              78.6%
  ```

  This **retracts their earlier advice to us**, which was that the hold is `O(records)`
  for atomicity and only a shadow overlay or a versioned memtable could help. The
  `(key, prefix)` set is known from a batch's planned units before the lock is taken, so
  the reads can be resolved outside it and applied under it, keeping the miss check
  inside because a concurrent commit may have filled it. Theirs to build, not ours.

  They also checked the lock ordering: the only pair anywhere is `mem.write()` then
  `store.lock()` inside the disk fallback, and the checkpoint path drops the store lock
  before taking `mem.write()`. No inversion. Worth recording because the combination
  reads like a deadlock and is not one -- and because this session once misattributed a
  harness deadlock to that very shape.

  Original entry, still accurate about checkpoints specifically: under a writer at the storage policy's
  own cadence, query **p99 goes from 5-10 ms to 85-109 ms and the max to 140-164 ms**,
  while the median does not move at all. A checkpoint is made durable holding the
  shard's store lock; readers take that lock per chunk; and `WriteBatch::commit` runs
  the policy before returning, so it lands underneath the queries. 2 097 152 documents,
  32 shards, a thread updating ~2 800 docs/s, nothing forced -- the policy fired every
  ~20 s, on the 256 MiB dirty trigger rather than the 60 s timer:

  ```text
  threads  writer   median      p90      p99      max   ckpts
        1      no   8.14ms   8.45ms   9.91ms  12.42ms       0
        1     yes   8.57ms   9.56ms 108.94ms 139.86ms       2
        8      no   4.15ms   4.51ms   4.84ms   5.32ms       0
        8     yes   4.16ms   4.71ms  85.74ms 142.46ms       3
       20      no   4.60ms   4.81ms   5.03ms   7.52ms       0
       20     yes   4.88ms   5.49ms  85.92ms 164.16ms       3
  ```

  **The cost scales with dirty state accumulated since the last checkpoint**, not with
  corpus size. Varying the flush interval directly at one reader, holding all else
  fixed -- `excess` is total latency above the median, divided by checkpoints that ran:

  ```text
  flush every   ckpts  docs/ckpt   median      p99      max   excess/ckpt
     1 commit      18      5 000   8.63ms  85.03ms 135.91ms        506ms
     5 commits      5     26 000   8.72ms 106.39ms 138.80ms       2465ms
    20 commits      2     57 500   9.19ms 135.56ms 409.65ms       9281ms
    policy          2     65 000   8.96ms 107.33ms 145.26ms       6364ms
  ```

  Normalised it is **~100 us of query stall per document written** in every row, so
  write volume sets the total and frequency only decides whether it arrives as many
  small stalls or few large ones. Checkpointing more often is not a mitigation.

  **This entry twice said 15-20 ms, and both times the number came from a benchmark
  that forced a checkpoint about once a second.** That kept every checkpoint small, so
  the harness understated the per-event cost by an order of magnitude at the same time
  as it overstated the frequency -- wrong in both directions from one choice. The
  correction did not come from measuring harder; it came from removing the `flush()`.

  It also puts a frame on upstream's "flat in dataset size" ( 11.9-16.8 ms across a 64x
  range of resident keys ): that was taken at **four dirty keys**, holding constant the
  variable that turns out to drive it. Two correct measurements, one misleading
  conclusion, because neither named which quantity was held fixed.

  **Re-taken after upstream moved the three fsyncs out of the store lock** ( their HEAD
  5bffd89 + 23 uncommitted files, diff sha256 a0f2efb5d5c9288f; upstream has
  since committed the same work as `f7f76bb` ). At this write volume
  the change is **below the noise floor**: p99 108.94 -> 108.47, 85.74 -> 84.71, max
  164.16 -> 140.25, all under 1.5%. That is the predicted result and both sides
  predicted it -- the syncs are a fixed ~17 ms floor, and upstream's re-sweep with dirty
  volume as the variable shows a floor plus a **shallow, strongly sublinear** term that
  only separates at millions of dirty ordinals ( ~2x the floor for 20x the dirty volume
  at four million; below a few hundred thousand there is no measurable slope at all ).

  That correction is upstream's, and it retired a number I had quoted from them here.
  Their first sweep was one run per point and implied proportionality; re-run three
  times, the bottom two rows overlap completely. **The shape survived and the steepness
  did not.**

  **Then the arithmetic settled it, and not in either side's favour.** A ~17 ms floor on
  an 85-165 ms stall is 10-20%, which is 10-20 ms and far above my noise -- so my
  "below the noise floor" reading was wrong about its own evidence. Under 1.5% across
  four thread counts does not mean "cannot resolve it"; it **bounds the fsync hold's
  contribution to my stall at under about 2 ms**. Upstream made that argument against
  their own change, which is what made me re-run.

  The discriminator was shard count, and it refutes the remaining store-lock reading.
  One reader, policy-frequency writer, 2 097 152 documents, excess latency above the
  no-writer median per checkpoint:

  ```text
  shards   median      p99      max   ckpts   excess/ckpt
       1   9.61ms  18.09ms  543.20ms      3      5341ms
       8   9.05ms 163.23ms  528.33ms      3      5993ms
      32   9.09ms 105.22ms  363.71ms      3      4892ms
  ```

  **Flat within noise across a 32x range of shard counts.** If the stall were the
  per-shard store lock's exclusive hold, concentrating every checkpoint into one such
  region would have made one shard dramatically worse; I predicted exactly that before
  running and it did not happen. What shard count changes is the **distribution**: one
  shard gives fewer, harder hits ( p99 18 ms, max 543 ms ), thirty-two gives more,
  softer ones ( p99 105 ms, max 364 ms ). The total is the same.

  Read together with the earlier interval sweep -- ~95 us of stall per document written,
  near-constant whatever the checkpoint frequency -- the stall behaves as **work
  proportional to write volume that a reader waits behind however it is divided**, not
  as a lock hold. Untouched candidates upstream named: the memtable `RwLock` and the
  shard write lock. A third, which upstream now backs as the likeliest: the reader reads
  through an mmap and a concurrent `fsync` of the same file can stall its page faults,
  which would be invariant to shard count for exactly the reason observed. **Untested**,
  and recorded as a hypothesis.

  **Refuted on the real system.** The reader takes **zero major page faults** in every
  arm, including the ones carrying 5 600 ms of excess stall per checkpoint, so it never
  waits on disk and a mechanism requiring a fault cannot be the one. Counted from
  `/proc/self/stat` -- cheaper than every other measurement in this investigation, and it
  should have been the first. Upstream's isolated probe found no effect at 512 MiB, which
  bounds rather than excludes; the fault count settles it at the right scale.

  *Scope*: at 2 097 152 documents the data is page-cache resident on a 121 GiB machine.
  An index exceeding RAM would fault and the hypothesis could revive there. It is dead
  for anything that fits in memory, which is every figure this project has published.

  So the structural mitigations -- separating the read and write paths onto different
  files, or not fsyncing the file readers fault on -- are **off the table**, and that
  matters because an unreproduced 3 995 us outlier upstream nearly sent would have
  implied exactly that redesign.

  What remains: the memtable `RwLock`, the shard write lock, or something neither side
  has named. All four pieces of evidence now point away from a lock -- shard-invariant,
  proportional to write volume, no disk wait on the reader, and 0.04% from removing the
  fsyncs from the exclusive region. That shape says the reader is **competing for
  something the checkpoint consumes, not blocking on something it holds**. CPU and memory
  bandwidth are the obvious candidates and neither has been measured.

  **The actionable finding does not depend on any of that.** The shard sweep says the
  total is invariant but the *distribution* is not, so shard count is a tail-shape knob:
  one shard gives fewer, harder hits ( p99 15 ms, max 543 ms ), thirty-two gives more,
  softer ones ( p99 105 ms, max 135 ms ). That is the one conclusion from this thread
  that survived every correction made to it, and it is now in `docs/operations.md`.
  Their light-writer A/B ( worst reader stall 5.9-8.3 ms -> 0.5-0.7 ms, disjoint ranges )
  is the regime where it shows, and it is a real win there.

  The repeat run also **retires two of my own numbers**. Normalised stall per document
  written is now 93, 97, 97, 93 us across the four rows where it was 101, 95, **161**,
  98 -- so the earlier claim of "roughly 100 us, constant" was right, and I made it from
  a set containing an outlier. The 409.65 ms worst query in that same row did not
  reproduce either ( 136.41 ms ); it was noise, and I had published it.

  The fix is upstream. Not measuring it was here, and the next server-shaped claim
  should carry a percentile taken with a writer live.

- [ ] **rwlock-the-shard-store-upstream**: ten of yesnodb's thirteen shard-store lock
  sites need only a shared borrow ( every read path passes `&*store`, and
  `read_container_for` is `&self` ); three need `&mut`, all open, checkpoint or
  statistics. `RwLock<ShardStore>` would remove the read-side serialization without any
  decision about holding the lock across the decode. Reported upstream as a pointer, not
  a claim -- not ours to build. If they take it, re-measure `DEFAULT_SHARDS`: the trade
  it encodes is entirely a consequence of this mutex, and a working `RwLock` would move
  the right answer back down toward the ingest-optimal count.

  **Sequenced behind the checkpoint stall above, on upstream's objection.** A plain
  `std::sync::RwLock` is not writer-preferring, and sustained reading is exactly the
  shape that starves a writer -- which for a checkpoint means unbounded WAL growth, a
  worse failure than the one being fixed. haiiie is **not** phase-separated in the
  server, so that hazard is ours and not hypothetical. Shortening the checkpoint's
  exclusive region first makes the read-side change safer and the starvation question
  smaller.

  **That prerequisite is now partly met, 2026-09-17.** Upstream split
  `commit_superblock` into a prepare-under-the-lock and a run-with-it-released, so the
  three `fsync`s -- 3 to 9 ms per checkpoint -- no longer block every reader on the shard.
  Their comment for it cites this project's own figure as the other side of the
  measurement ( p99.9 6.85 to 54.92 ms with a writer present, median unmoved ).

  What is **not** met is the rest: the checkpoint still takes the store lock for the index
  rebuild, which upstream measured at 70-82% of the remaining exclusive region. So the
  starvation window is shorter and has not closed, and a plain `RwLock` is still the wrong
  primitive for it.

  **Recording this because the deferral is easy to misread as a lack of justification, and
  it is the opposite.** The case is measured on this side, not argued: `next_chunk` takes
  the mutex once per chunk and holds it across the decode, so the shard count *is* the
  concurrent-reader ceiling. Swept, with one shard as the control -- twenty threads give
  **0.36x** of one thread at one shard, **1.06x** at the eight that used to be the
  default, and **5.56x** at sixty-four -- the last re-measured at 4.23x on 2026-09-18,
  with the monotone shape and the sub-1x control intact; see the entry above. `DEFAULT_SHARDS = 32` is a workaround for this and
  costs 12% of ingest rate to buy it. The item is blocked on a safety question upstream
  owns, not on evidence.

- [x] **eviction-cannot-observe-a-stale-cursor** -- closed. `EvictingStore::on_evict`
  runs a callback at the injection point, so a **write lands between the fault and the
  retry** and the retry's snapshot holds a version a carried-over cursor cannot see. All
  three rebuild sites are now sabotage-caught: the serial rebuild, the lazy forward
  cursor's reset, and refreshing the snapshot at all.

  Three things had to be true before the test could observe anything, and each was found
  by a sabotage that passed:

  * **The fault must land where a cursor is open.** `arm` now restarts the read count, so
    a fault's position in a scan is a property of the test rather than of everything that
    ran before it. A fault arriving before the forward cursor opens leaves nothing stale
    to carry.
  * **Each arm must promote a different document.** Promoting one id made the second path
    unobservable -- that promotion was already durable, so the stale snapshot contained
    it too.
  * **Both paths must run.** They hold different cursors, reset in different places, and
    covering one leaves the other exactly as unguarded as before.

- [x] **upstream-leaf-suffix-width-crash** -- **fixed upstream and verified here
  2026-09-14.** `suffix_u64` became `suffix_u128` and `t_lo` is compared at u128 width,
  so widths 10 and 12 compare all 80 / 96 bits instead of underflowing and then
  truncating. Both `YesnoStore` reopen tests are un-ignored and green; 32 tests pass.
  `tests/key_layout.rs` now pins the property directly rather than relying on it
  falling out of the scoring tests: our `(namespace << 56) | (kind << 20) | index`
  layout makes `live()` and `dim(0)` differ **only** at bits 20..28, entirely inside
  the range a narrowed comparison discards, so that pair is the decisive case and it
  exists in every index. Declining to bend the key layout around the bug was the right
  call -- the layout is what made the test decisive.

### M8 -- done 2026-09-14

- [x] **grpc-wire-format**: `haiiie-proto` ( generated types, **no transport** ) and
  `haiiie-grpc` ( service and client ), plus `haiiied` and `haiiie` binaries. Chosen
  over Arrow Flight at the maintainer's preference and it is the better fit: the remote
  surface is a query in and a top-k out, where Flight is built for streaming record
  batches and would import Arrow into the server to carry a ten-element answer. Ingest
  is client-streaming, which gRPC has natively. Talking **to** yesnodb stays Flight,
  because that is yesnodb's protocol rather than ours.
- [x] **remote-equals-embedded**: tested over a real socket across all four metrics,
  three values of k, filters, and streaming ingest -- including that the **exact
  rational score** survives the wire, not merely its `f64` rendering.
- [x] **protoc-is-vendored**: `protoc-bin-vendored` supplies the compiler, so a
  checkout builds with nothing installed on the host.
- [ ] **one-directory-per-process**: `haiiied` serves a single index, but the constraint
  is narrower than an earlier wording here claimed. yesnodb's exclusive lock is **per
  directory**, and namespaces already let several indexes share one directory and one
  `Db` -- `YesnoStore` is `Clone` and clones share the handle -- so multi-index hosting
  in one process is available today and simply not exposed by the binary. What is
  genuinely one-per-process is the **data directory**. Corrected 2026-09-14 after the
  per-shard-lock question exposed the imprecision.
- [ ] **no-auth-no-tls**: the service is unauthenticated and unencrypted. Do not expose
  it. Upstream's control plane has a worked model ( principals, `pg_hba`-style rules,
  mutual TLS ) if this ever needs one.
- [x] **empty-index-reports-one-skipped-block** -- fixed. `last_block` returns
  `Option<u64>`, so "the last block is block zero" and "there is no last block" stop
  being the same value and `0 ..= 0` stops running over a block that does not exist.
  Fixed at the type rather than at each of the four call sites, because the three that
  were right were right by accident.

  Cosmetic, and worth the test anyway: the **hits were never wrong**, so no differential
  oracle in the suite could see it -- they all compare results. `empty_index.rs` covers
  both an index that never held a document and one whose documents were all deleted,
  which are different states ( the second has a written `LIVE` key ), across four forced
  paths and two thread counts.

### M6 -- done 2026-09-14

- [x] **profile-before-optimizing**: instrument at `.agents-workspace/tmp/profile`.
  Over a **real yesnodb store**, 262 144 documents at D=256 and a 118-bit query:
  reads 2.02 ms of a 3.45 ms scan ( 59% ), accumulation ~3%, everything else the rest.
- [x] **consume-bitmap_words**: the read path reassembled 1 024 `u64`s from bytes per
  block per dimension. `unstable_arrow::bitmap_words` -- the API we asked upstream for
  and had not used -- lends a bitmap container's words directly. Reads **2.02 ms to
  0.48 ms ( 4.2x )**, whole scan **3.51 ms to 1.66 ms**. Sparse containers take a
  position scatter instead, which is also cheaper than materializing a mask to copy.
- [x] **staged-wand-would-not-help**: **not built, and the profile is why.** It reduces
  *accumulation* cost, which was ~3% of the scan before the read fix. Even eliminating
  accumulation entirely could not have paid for the mechanism.
- [x] **parallel-blocks** -- built; this entry was a duplicate of `no-parallelism` above
  and survived it by three milestones. Its stated reason for deferring ( "1.66 ms
  single-threaded, not worth the machinery" ) was also obsolete twice over: the figure
  is 0.9 ms after the read fixes, and the machinery it feared -- a shared threshold --
  was never needed, because blocks merge without coordination.

### M5 -- done 2026-09-20

- [x] **block-aligned-filter**: the filter is evaluated one block at a time into a
  reused mask. Whole-query allocations went from 4.3 million at 196k documents to
  **16 at 131k against 13 at 20** -- constant in the corpus.
- [x] **memstore-snapshot-was-a-deep-copy**: `MemStore::snapshot` cloned the entire map,
  so the double's cost model differed from the thing it doubles and every budget taken
  through it was measuring the double. Now copy-on-write behind an `Arc`, which is also
  what a real MVCC snapshot is.
- [x] **block-statistics**: `BlockStats { w_min, w_max }` under `STAT`, invalidated by
  any write to the block in the same atomic batch, recomputed by `refresh_stats()`.
  Absence is the safe state: a block without statistics gets the loose bound.
- [x] **centroid-pruning-is-measured-dead**: **do not build it.** Instrument at
  `.agents-workspace/tmp/radius`. The bound `|q AND c_b| + r_b` prunes **0.0%** on
  524 288 random 128-bit codes, under every ordering tried and at every granularity
  from 65 536 documents per group down to **16**:

  ```text
  group    radius min/mean/max    bound ~   tau ~   prune
  65 536         57/62/67 of 128        94      49    0.0%
   1 024         48/65/84               98      49    0.4%
      16         43/56/72               83      49    0.0%
  ```

  The mechanism is not tuning: random binary codes at density 0.5 are ~D/2 apart, so a
  centroid's radius stays near D/2 however few documents share it, and the bound never
  approaches the threshold. **Clustered ordinal assignment cannot fix this** -- sorting
  cannot create structure the data does not have, which is why the sweep goes down to
  16 documents and still fails. Revisit only with **real embeddings** at M8, where
  cluster structure is a property of the data; our synthetic generators cannot validate
  it, and that is a limit on the suite worth knowing.
- [x] **weight-range-bound-instead**: what the measurement *did* find. A per-block
  `w_min` tightens Jaccard from `a/m` to `a/max(m, m+w_min-a)` and cosine likewise,
  needing no centroid and no clustering. Candidates admitted, on the same corpus:
  loose **42.5%**, tightened **20.7%**, and with weight-sorted ordinals **0.1%**.
- [x] **weight-sorted-ordinal-assignment**: the caller assigns ordinals, and sorting
  them by code weight is what makes a block's weight range narrow. Measured end-to-end
  rather than at the bound: **189x** on documents scored for Jaccard, 67 672 to
  358 at 524 288 documents; cosine is 90 784 to 385, **236x**. It was 40x until
  2026-09-19, when the scan began carrying its
  threshold between blocks and moved both ends -- the unsorted case improved 1.9x and the
  sorted case 8.8x, so the **gap widened**. Originally a documented caller strategy;
  the user docs now carry the table rather than the adjective "narrow", because a reader
  cannot act on an adjective.

  It cannot be owned silently at ingest -- ids are caller-assigned, dense and stable.
  The maintainer selected explicit compaction and ordinal reclamation together on
  2026-09-20, extending M5 to include both.

  Worth noting what the numbers do *not* justify: haiiie silently reordering anything.

  **The cheaper honest move is built**, and this entry did not say so for a release.
  `Plan::block_weight_spread` reports the mean of `w_max - w_min` over blocks carrying
  statistics, so a caller querying by Jaccard on badly ordered ids can see the 189x they
  are paying rather than being told about it in prose they may never read. It is shown
  only for the ratio metrics -- it is a fact about the index either way, but only an
  actionable one when it is costing them -- and compared against `dims`: a spread near
  `dims / 2` means ids are in no weight order, a small one means they are. Mean rather
  than max, because one outlying block says little and the reader's question is about
  the whole index. `m5_stats.rs` pins the tightened-versus-loose ordering and the `None`
  case before `refresh_stats`.

  **Built 2026-09-20:** `compact(directory, namespace)` rewrites the namespace in
  descending weight order under the exclusive directory lock, rebuilding codes,
  postings, attributes and `STAT` atomically. Dead-only attribute memberships are
  removed before ordinals can be reused. A durable ID mapping lands in the same batch;
  `Index::open` refuses the namespace until `acknowledge_compaction` confirms external
  references have been migrated. Retrying compaction recovers the pending mapping.

  Three actual compactions reproduce the score counts above exactly against the
  brute-force oracle at D=256, k=10, seed 7, `Shape::Balanced`, 524 288 documents.
  Compaction itself takes **15.120-16.116 s**, including opening and checkpointing,
  on this machine with 32 shards; these are not query latency ratios. It holds live
  rows and one rewrite batch in memory, requires downtime, and has no CLI/gRPC entry.
  Construction and correctness coverage are recorded in `JOURNAL.md`.
- [x] **explain**: `Search::explain()` returns a `Plan` -- path and the reason for it,
  kernel, accumulator width, blocks live and blocks carrying statistics, and whether
  the ranking key is exact or a bound plus refinement. It reports decisions and cheap
  facts and **does not predict cost**: there is no measured cost model here, and a
  plausible number carrying no measurement is worse than an absent one. A test asserts
  the `Auto` reason still says "not a measured choice", so the default cannot quietly
  start reading as a planner decision.
- [x] **resumable-scan-under-eviction**: `Consistency::{Strict, ResumeOnEviction}`,
  `Hits::version_range`, `ScanStats::resumes`, and `EvictingStore` as a fault injector.
  Every resume test asserts the fault actually **fired**, because a passing resume test
  with zero injections proves nothing -- and that assertion immediately caught an
  injection interval tuned for the inverted path firing never on the forward one.
- [x] **a-planner-needs-a-cost-model** -- the grid it asked for is measured, and it found
  a real defect rather than confirming the constant. At D=256 over 262 144 documents the
  crossover moves **thirty-fold** with the query's width:

  ```text
  |Q|    |Q|/D   inverted still wins up to
  118    0.461                     1 in 16
   59    0.230                    1 in 256
   40    0.156                    1 in 256
   30    0.117                    1 in 384
   20    0.078                    1 in 512
   15    0.059                    1 in 512
  ```

  Flat in corpus size ( same cells at 262 144 and 1 048 576 ) and tracking the *ratio*
  rather than either width alone ( same shape at D = 128, 256 and 512 ).

  **Why it moves is the durable part.** Gather reads whole rows, so its cost does not
  depend on the query at all -- 0.74 ms against 0.70 ms at one in twenty-four, for queries
  of 118 and 15 bits. The inverted path reads one posting list per query bit and is affine
  in `m`: 0.85 ms at 118, 0.30 ms at 15. Only one side of the comparison depends on `m`,
  so a single constant was always a constant for one query width.

  Cost of the old rule, measured: for a 15-bit query the planner picked the forward path
  from one in twenty-four onward and was **1.1x to 2.4x slower** than the alternative
  across that entire band. Sparse codes are an advertised use case, so it is not a corner.

  `crossover_for(m, dims)` replaces it with three coarse regimes, breakpoints placed
  between measured rows. **Deliberately not a curve**: a power-law fit gives
  `23.8 * (D/|Q|)^1.06` with a **238% worst residual**, because the crossover saturates
  below `|Q|/D` of about 0.08 and a power law cannot. This project has already retired one
  model that fitted its own numbers and nothing else. Residual error is bounded and stated
  at the constant: about 1.3x near the 0.12 breakpoint, against 2.4x for what it replaces.

  Verified by timing `PathHint::Auto` itself rather than predicting it -- `Auto` now
  tracks the better path at every point in both regimes. `planner.rs` pins the two
  regimes behaviourally via `Hits::path`, with no stopwatch; it is **not** sabotage-checked
  and says so.

  What stays open is the honest remainder: this is a measured step function, not a cost
  model. A model would compare estimates of both paths from `m`, `admitted` and
  `rows_per_block` -- the affine-in-`m` and independent-of-`m` shapes above are what one
  would be built from -- and would need per-machine constants, which is the reason
  `explain()` deliberately reports decisions and not predicted costs.
- [x] **all-four-metrics**: Jaccard and cosine on the inverted path via the
  monotone-bound refinement pass, `slice::ge`, exact rational comparators. Every suite
  now runs all four metrics across all four path/kernel combinations. 46 tests.
- [x] **the-ratio-bound-is-loose** -- the prescribed fix shipped with M5's `STAT` keys
  and the entry was never re-measured, so it read as an open problem with a known remedy
  while the remedy sat in the tree. Re-measured at 524 288 documents, D=256, k=10,
  counting documents given an exact score:

  ```text
  arm                                       Dot    Hamming    Jaccard     Cosine
  no per-block statistics                   120         97     456336     521562
  statistics, arbitrary ordinals            120         97     126160     155536
  statistics, weight-sorted ordinals        128        105       3136       4834
  ```

  `Metric::bound_with` taking `w_min` is worth **3.6x**; assigning ordinals in weight
  order is worth a further **40x**, and together they take a Jaccard query from scoring
  87% of the corpus to 0.6%. The bound is no longer the loose part.

  **Superseded 2026-09-19 by carrying the threshold between blocks.** Same instrument,
  same corpus, with the scan's running `k`-th best fed into each block's refinement:

  ```text
  arm                                       Dot    Hamming    Jaccard     Cosine
  no per-block statistics                   120         97     373835     509531
  statistics, arbitrary ordinals            120         97      67672      90784
  statistics, weight-sorted ordinals        128        105        358        385
  ```

  **1.9x on arbitrary ordinals and 8.8x on weight-sorted ones**, on top of everything
  above. Dot and Hamming are unchanged at 120 / 97 and 128 / 105, which is the control:
  they are linear, skip the refinement entirely, and must not move.

  The sentence that stood here -- "the dominant factor is not in this crate at all" -- was
  wrong, and it was wrong in a way worth recording. It concluded from a table that the
  remaining headroom belonged to the caller, when the scan was throwing away a threshold
  it already had. A Jaccard query on **arbitrarily ordered** ids now scores 67 672 against
  the 3 136 that weight-ordered ids used to cost, so this crate closed most of the gap it
  had attributed to the caller. See `weight-sorted-ordinal-assignment`, which is still
  worth 189x on top.
- [x] **refinement-is-one-pass** -- **closed 2026-09-19 with a proof, not a measurement.
  One pass is already the fixed point.**

  This sat open for three milestones as "an optimization waiting on a measurement showing
  the second pass is wide", and the measurement it waited for was the wrong question.
  Iterating what is implemented **cannot change anything**:

  * the descent picks the top `k` by intersection;
  * `tau` is the `k`-th *exact* score among those `k`;
  * the survivors keep everything whose intersection could still reach `tau`, which
    necessarily includes those same `k` -- the one that achieved `tau` has
    `bound(a) >= tau` by the definition of a bound, so its `a` is at or above the
    threshold;
  * so a second descent over the survivors returns the same `k`, computes the same `tau`,
    and keeps the same set.

  Pinned as a unit test across two ratio metrics, three values of `k` and three `w_min`,
  with a companion asserting the refinement removes candidates at all -- without which the
  fixed-point test would pass vacuously against a refinement that narrowed nothing.

  **The design sketch's loop is a different loop**, and conflating them is what kept this
  open. Its step scores *every* survivor rather than the top `k`, which does raise `tau`
  and is exactly the cost the refinement exists to avoid. So iterating what is built is
  free and buys nothing; iterating what was designed buys something and is not cheap.
  Both readings close the item, and the previous entry's "points both ways" was reasoning
  about a version of the loop that was never written.

### M3 -- done 2026-09-14

- [x] **carry-save-kernel**: `Csa`, four-way path equivalence, allocation budgets,
  and the 2026-09-14 local-plane benchmark used to assess the upstream
  bit-sliced-lens proposal, closed on 2026-09-19.
- [x] **live-set-is-materialized-per-query**: the scan built `live` and `admitted` as
  `BTreeSet`s -- `O(N)` allocations on every query, 4.4 million at 196k documents, which
  dwarfed everything the kernels do. `Filter::eval_block` replaced it: the filter is
  evaluated one block at a time into a reused mask, costing one mask per operand and
  nothing per document.

  **This entry stayed open for a release after the fix landed**, which is the same
  failure `the-ratio-bound-is-loose` records two entries above -- an item reading as an
  open problem while its remedy sits in the tree. Both were found by reading the code
  rather than the backlog. The closure is not a code read alone:
  `allocations_do_not_scale_with_the_corpus` is a standing gate test, so a regression
  here fails the build rather than reopening the entry.
- [ ] **counter-array-kernel-deferred**: the plan put a sparse scatter kernel at M3. It
  is not built, because `load_block` hands back a dense mask -- a counter kernel would
  densify and then scatter, which is strictly worse. Its premise is a sparse *read*
  path that returns positions rather than bits. Revisit with that, not before.

### M2 -- done 2026-09-14

- [x] **inverted-path-and-slice**: `slice.rs` ( ripple-carry accumulator, descent ),
  `DIM` and `ZPLANE` written at ingest, `Path::Inverted`, and a three-way
  path-equivalence gate. 31 tests.
- [x] **the-forward-key-merge-is-a-layout-break** -- **no bump; the layout version stays
  1.** haiiie has not shipped, so no index exists that this build did not write, and a
  version number distinguishing two layouts nobody holds is ceremony. I bumped it to 2
  first and was corrected. The first release fixes the meaning of 1; every layout change
  after that needs a bump.

  What survives, because it does not depend on the number: `decode` refuses an unknown
  version with `Error::UnsupportedLayout` rather than `CorruptMeta`. The header parsed
  and the magic matched, so telling the holder of an intact index that it is damaged
  sends them looking for a fault that is not there; the message says the layout moved and
  the index must be rebuilt. A test asserts the **variant** rather than `is_err()`, since
  an `is_err()` assertion would survive a change that reclassified it as corruption.

  Worth recording why the check earns its place before it has ever rejected anything: the
  forward-key merge is exactly the shape of change that would be **silent**. The merged
  key's number is the number the old scheme used for block zero, so an index from the
  earlier layout would find real data there, read its first `rows_per_block` documents
  correctly, and return an all-zero row for every document after them -- scoring as a
  code of weight zero, with no missing key and no decode failure. Once there is a release
  to break, that check is the only thing between such an index and a result that looks
  normal and is wrong over most of the corpus.

- [x] **backfill-for-m1-data** -- **the gap it names is closed and its premise is gone**,
  and it stayed open past the milestone it was due before ( M3 ) saying otherwise.

  What it asked for was a version marker, so that an index without `DIM` or `ZPLANE` keys
  is refused rather than silently scored as all-zero on the inverted path. That marker
  exists: the metadata blob carries a layout version, a mismatch raises
  `UnsupportedLayout` naming both versions, and the suite constructs it -- verified by an
  error-variant audit rather than assumed, six constructions across the workspace.

  The backfill half is moot rather than done. haiiie has not shipped, so no index exists
  that this build did not write, and the one layout change since was made **in place**
  under version 1 on that reasoning. There is no M1 data to backfill anywhere.

  What remains live is already recorded where it belongs, beside the constant rather than
  in a backlog entry: the first release fixes the meaning of version 1, and every layout
  change after that needs a bump. The forward-key merge is the worked example of why --
  the merged key's number is the number the old scheme used for block zero, so such an
  index would read its first `rows_per_block` documents correctly and return weight-zero
  rows for every document after them, with no missing key and no decode failure.
- [x] **load_block-still-materializes** -- `YesnoStore` overrides it through chunk
  containers, and `open_lanes` now goes further: a cursor held across blocks, which is
  where the read path reached the machine's memory ceiling. The allocation budget landed
  with the override as planned.

### M1 -- done 2026-09-14

- [x] **key-space-ingest-and-forward-paths**: `Index`, `Writer`, `META`/`LIVE`/`FWD`/
  `ATTR`, `Gather` and `DenseScan`, Dot and Hamming, filters, deletes. 24 tests.
- [x] **m1-materializes-on-purpose** -- superseded. `SetSnapshot::load` still returns an
  owned `Vec<u64>` and still should: it is the right shape for whole-key reads and for
  the `MemStore` double. The scan does not use it. **The standing rule survives the
  entry: do not build anything new on `load`** -- block reads go through `load_block`,
  and a fixed key list read across blocks goes through `open_lanes`.
- [x] **no-dim-or-zplane-yet**: both landed at M2 with the kernel that reads them.
  The backfill obligation is tracked above as `backfill-for-m1-data`.

### M0 -- done 2026-09-14

- [x] **workspace-and-oracle**: workspace, `haiiie-core` ( code, score, keyspace,
  error ) and `haiiie-testkit` ( oracle, corpus, rng ). 14 tests, gate green.
- [x] **gate-scripts-are-inert-until-m1**: resolved early. `check-layout.py` was
  verified live by watching it refuse nine unlisted source files, and `check-deps.py`
  by adding `tokio` to `haiiie-core` and watching it refuse. Neither was decorative for
  a milestone, which was the risk this item existed to prevent.

### Deferred out of M0, deliberately

- [x] **memstore-and-store-traits** -- landed at M1 as planned, beside `YesnoStore`,
  with `the_two_stores_return_byte_identical_hits` as the property that justifies the
  abstraction.
- [x] **haiiie-embed-and-haiiie-cli** ( M8, M7 ) -- both have content and both are in
  `[workspace] members`, which is load-bearing: `cargo test --workspace` would not
  otherwise reach them.

### Repo scaffolding -- done 2026-09-14

- [x] **licences-and-notices**: dual `LICENSE-APACHE` / `LICENSE-MIT`, `NOTICE`,
  `THIRD_PARTY_NOTICES.md`. Licence texts copied from upstream rather than reproduced
  from memory -- an inaccurate licence file is worse than an absent one.
- [x] **readme**: `README.md` owns the human-facing description, the measured numbers,
  and an explicit "what is not built" list.
- [x] **user-docs**: `docs/` -- index, getting started, data modeling, query language,
  recall, operations. Self-contained by the gate, which refuses a source path in prose.
- [x] **ci**: `.github/workflows/ci.yml` runs `scripts/gate.sh` as one command, plus a
  separate job proving the workspace still builds at the 1.89 floor.
- [ ] **ci-has-never-run**: the workflow is written and has executed nowhere. It assumes
  a `moriyoshi/yesnodb` checkout is fetchable beside this one; if that repository is
  private or laid out differently, the path dependency will not resolve and both jobs
  fail at the first build. Treat it as unproven until a run is green.
