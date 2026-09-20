# Metrics and exactness

## The four metrics

Write `a` for the number of bits the query and document share, `m` for the
query's own bit count, and `w` for the document's.

| Metric | Value | Ranks by |
|---|---|---|
| Inner product | `a` | more shared bits |
| Hamming | `m + w - 2a`, returned negated | fewer differing bits |
| Jaccard | `a / (m + w - a)` | overlap relative to union |
| Cosine | `a / sqrt(m * w)` | angle between codes |

Inner product ignores document length entirely, so a document with many bits set
scores well against everything. Hamming, Jaccard and cosine each correct for
that differently: Hamming subtracts it, Jaccard divides by the union, cosine by
the geometric mean. If your codes have similar weights the three agree closely;
if they do not, pick the one whose correction you want.

**Jaccard and cosine cost more, and how much more is up to you.** The first two
are linear in `a`, so a query can rank by an integer and stop early. The ratios
cannot: they need the document's own weight, so a query prunes with an upper
bound and then scores whatever survives it. How much survives depends on the
per-block weight statistics — which are only informative if ids were assigned in
weight order, and which have to be rebuilt after a bulk load. Get both right and
a ratio query scores a few thousand documents where it would otherwise score
most of the corpus; get neither and expect a ratio query to cost several times
what the same query costs under Hamming. See the data-modeling and operations
pages.

## Exactness

All four are computed **exactly**, and that word is doing specific work.

* **Scores are rationals, not floats.** Comparison is by cross-multiplication in
  128-bit integers. Nothing is rounded before an ordering, so a tie is a real
  tie rather than an artifact of precision. A result carries both the exact
  rational and a floating-point rendering; the rendering is for reading.
* **The ordering is total.** Descending by score, then ascending by id. Two
  servers holding the same data return byte-identical results, and so does the
  same query run twice.
* **Nothing is sampled or pruned away.** Bounds are used to skip work that
  provably cannot place, never to guess.

What "exact" does **not** cover is the step before haiiie: turning float vectors
into codes. That error is real and is measured separately.

## Ties

Documents with equal scores are returned in ascending id order. This is stable
within a snapshot, but id order is not stable across a compaction, so ties may
resolve differently afterwards.

## Counting without scoring

Asking how many documents a filter admits scores nothing and materializes
nothing. It is the cheap question and is answered as one.

## Explaining a query

`explain` reports the path, why that path was chosen, the accumulator width, how
many blocks hold live documents and how many carry statistics, and whether the
metric is ranked exactly or through a bound plus a refinement pass.

It deliberately does **not** estimate cost. There is no measured cost model
behind the path choice yet, and the reason string says so rather than presenting
a default as a decision.
