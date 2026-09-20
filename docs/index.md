# haiiie documentation

haiiie returns the exact top-k under a stored-code score, composable with
arbitrary boolean filters. Ordinary indexes score binary codes; model-bound
residual indexes score fixed 64-byte rows by their signed-integer key.

Start here:

* **[Getting started](getting-started.md)** — create an index, ingest codes, run
  a query, embedded or over gRPC.
* **[Data modeling](data-modeling.md)** — how documents, ids, codes and
  attributes fit together, and which decisions are yours rather than haiiie's.
* **[Query language](query-language.md)** — the four metrics, what each one
  means, and exactly what "exact" covers.
* **[Recall](recall.md)** — what binarization costs, how to measure it, and why
  a recall figure without a corpus description is not a figure.
* **[Operations](operations.md)** — running the server, statistics, snapshots,
  and the things it deliberately does not do.

## The shape of the thing

An ordinary binary code is a set of positions. Its index stores each
dimension's posting list and scores a query by accumulating how many query
positions each document shares. That count is built in bit-sliced form, so one
posting-list operation advances 65 536 accumulators. A model-bound residual
index stores only forward code rows, liveness and filter attributes; its exact
integer scorer does not use binary postings.

Everything is exact under the selected stored-code score. There is no
approximate search mode, recall tuning knob or candidate sampling: a result is
the true top-k of the codes. Encoding a float vector into a residual row can
still lose information, which is a separate, measurable source of error.

## What haiiie does not do

It does not store your documents, embed your text, decide your ids, or retain
float vectors. It holds codes and returns document ids in score order. A
residual model encodes float documents at ingest and prepares float queries,
then the vectors are discarded.
