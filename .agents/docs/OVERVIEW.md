# haiiie Project Overview

**haiiie** ( はい / いいえ, Japanese for *yes* / *no* ) is an exact stored-code
vector search engine built on [yesnodb](../yesno). Ordinary indexes rank D-bit
codes by four binary similarities; model-bound residual indexes rank a fixed
64-byte code by their named integer score. Both compose with arbitrary boolean
filters.

## Status

**M0 through M8 complete; not operated anywhere.** The engine works: exact top-k over
binary codes on all four metrics, arbitrary boolean filters, a gRPC service, a CLI,
a residual fitter and serving. 205 tests and a gate that counts its own steps -- and, since
2026-09-16, counts the tests too, because this sentence said 68 for long enough
that nobody re-derived it.

Every measured number in this repository comes from one machine. Latency, ingest and
allocation figures are against synthetic corpora; recall is against GloVe, 1 183 514 real
word vectors -- this said "every measured number comes from synthetic corpora" until
2026-09-19, which was true before the GloVe run and had outlived it.
Nothing has seen a real workload, and three features named in the original plan were
**measured and deleted rather than built** -- see `TODO.md` for which and why, and
`JOURNAL.md` for the numbers. `README.md` owns the user-facing statement of what is not
built; do not restate it here and let the two drift.

## The thesis

haiiie returns the **exact top-k under the selected stored-code score**. That is
a standard binary similarity for an ordinary index and the named integer
weighted-sign plus norm score for a residual index. It is an *exact solver for an
approximate problem*: quantization error is measurable offline and identical for
every query, whereas a graph index's search error is query-dependent,
tuning-dependent, and invisible at query time. That is a different claim from
"more accurate", and the difference must not be blurred in user-facing prose.

Its cost is **linear in the filtered candidate set, not in the corpus**. That is the
product: exact top-k under arbitrary high-selectivity boolean filters, at roughly 128
bytes per document forward-only, with transactional inserts and deletes and no rebuild
step.

## Why yesnodb

A binary vector *is* a set of set-bit positions, so similarity becomes set algebra --
which is the one thing yesnodb is built to do fast and durably. yesnodb's own
`formal-model.md` fixes the *which bitmaps exist* axis at equality encoding and names
O'Neil and Quass's bit-sliced indexes as the axis it left open. haiiie is that layer.

## Scope

* Binary codes supplied by the caller, or fixed 64-byte residual codes produced by a bound model.
* Inner product, Hamming, Jaccard and cosine over ordinary codes, plus the named residual integer score, all exact.
* Arbitrary boolean filters, composed at query time, with no per-filter index.
* Embedded first; a `haiiied` server later.

## What It Deliberately Is Not

* **Not a document store.** haiiie holds codes and ordinals. The application owns the
  objects and, in v1, the ordinal assignment.
* **Not an approximate index.** IVF and clustering are later, opt-in, and never replace
  the exact path -- which must keep passing the same differential suite with them on.
* **Not a float vector store.** Residual ingest consumes floats without retaining them; query embeddings are transient.
* **Not faster than HNSW unfiltered at billion scale.** It is 10-100x slower there and
  will not close that by tuning. Say so.
* **Not a fork of yesnodb.** Set algebra belongs upstream; similarity belongs here.

## Milestone Gates

Milestones marked done are done; the table is kept because what each one *proved* is
the durable part.

| M | content | what it proves |
|---|---|---|
| M0 | harness, workspace, `MemStore`, brute-force oracle | the oracle exists before the thing it checks |
| M1 | key space, ingest, forward paths, Dot + Hamming | exact top-k, filtered, verified |
| M2 | inverted path, gang cursor, ripple-carry accumulator | ripple first, because it is the CSA's oracle |
| M3 | CSA kernel, counter-array kernel, allocation tests | the measured speedup |
| M4 | filter composition, Jaccard + cosine, rational comparators | all four metrics |
| M5 | block statistics, centroid pruning, clustered ordinals, planner; explicit compaction and ID reclamation added 2026-09-20 | the measured prune rate and safe ordinal reuse |
| M6 | staged WAND, parallel blocks | throughput |
| M7 | wire format, `haiiied`, CLI | remote equals embedded |
| M8 | residual fitter, codec and serving | recall against exact float search and exact stored-code execution |
| M9 | upstream `view_count` alone -- **declined 2026-09-19**, because the premise that made it immune to measurement was false and untested | a proposal that did **not** survive its own evidence |

## Operating Model

Work is planned against the milestone list, journalled as it lands, and consolidated
into `LTM/` when a topic accumulates enough to be worth a standing document.
