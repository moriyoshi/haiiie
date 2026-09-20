# Data modeling

haiiie stores one bit-packed code per document, plus whatever boolean
attributes you attach. An ordinary index also stores binary postings and
weight planes; a model-bound residual index keeps only the forward code because
its exact scorer does not read those binary views. Everything else is yours.

## Document ids are ordinals, and you assign them

A document id **is** the storage ordinal. There is no dictionary and no
translation step: ids should be dense, ascending and assigned by you from a
durable counter.

Usable range is **0 through 2^36 - 1**, about 68.7 billion. Above that an id is
refused rather than stored: the shared id limit is set by a document's row and
the binary layout's block-statistics key range, even though residual indexes
omit those statistics. `2^64 - 2` is the storage
layer's own ordinal ceiling and is not the number that applies here.

Three consequences worth knowing before you pick an id scheme:

* **A query costs what your id *range* costs, not what your document count
  costs.** Documents are processed in blocks of 65 536 consecutive ids, and a
  scan visits every block from zero to the highest id in the index — including
  the empty ones, at about 5 microseconds each. One document at id 2^36 - 1
  makes a query take **60 seconds**; the same document at id 0 takes 250
  milliseconds. Nothing is wrong in either case and both return the same answer.
  This is the practical reason ids should be dense, and it is why a hash is a bad
  id: it spreads a thousand documents over the whole range and buys a scan that
  visits a million empty blocks.
* **An id is recycled only by explicit compaction.** Deleting a document retires its id. Reusing one
  whose old code bits survived anywhere would score the new document by the
  old code — silently, with the right count. Ids return to the pool only after
  an offline compaction that rewrites the namespace and hands the caller a durable
  ID mapping. See [operations](operations.md#deletes-and-compaction).
* **Ids decide locality for Jaccard and cosine.**
  Documents are processed in blocks of 65 536 consecutive ids, and the
  statistics that let those two metrics skip work are per block. Both are
  ratios, so the work a query can skip depends on knowing that a block's
  documents are of similar weight — and a block's weight range is narrow only
  if ids were assigned in weight order. Measured on 524 288 documents of 256-bit
  codes, counting how many documents a top-10 Jaccard query had to score
  exactly:

  | Ids assigned | Statistics | Documents scored |
  |---|---|---|
  | arbitrary order | none | 373 835 of 524 288 |
  | arbitrary order | rebuilt | 67 672 |
  | **weight order** | rebuilt | **358** |

  Inner product and Hamming score around 100 in every one of those cases: they
  are not ratios and do not care. So this matters if and only if you query by
  Jaccard or cosine — and if you do, it is the single largest thing under your
  control.
* **Ids will decide shard placement** if haiiie ever shards, because a shard is
  a contiguous id range. Whoever assigns ids is choosing placement whether or
  not they know it.

## Codes

A code is `D` bits, in one of two representations:

* **Dense** — word-packed, least significant bit first. Smaller above roughly
  1/32 density. Sign-quantized embeddings are dense.
* **Sparse** — ascending, unique set-bit positions. Smaller below that.
  Learned sparse retrieval vectors are sparse.

Both are accepted everywhere. Sparse positions **must** be strictly ascending;
unordered input is refused rather than sorted, because the intersection merges
rather than searches and would otherwise be quietly wrong.

Pick `D` by the recall you need, not by your embedding's width. A random
projection lets you spend a 768-dimensional embedding on 256 or 1024 bits.

`D` is fixed when the index is created and cannot be changed afterwards. The
maximum is **65 536**, because a document's row lives inside one 65 536-ordinal
block and a code wider than a block leaves no room for a document in it. A
wider `D` is refused at creation rather than accepted and found unusable later.

## Attributes

An attribute is a term number attached to a document. It is a set, so a document
may carry any number of them, and one term may cover any number of documents.

**Terms run from 0 to 2^20 - 1**, about a million. Ingest and query filters
both refuse larger terms. The term is packed into the key alongside what kind
of key it is, and that field
is twenty bits wide.

A million is generous for a vocabulary you enumerate -- tenants, categories,
languages, tags -- and it is **not** enough to hash a term name into. Assign
term numbers from a counter and keep the mapping on your side, the same way you
assign ids. Hashing a name to a number is the one id scheme this rules out.

Use them for anything a filter should test: tenant, category, language,
lifecycle state, a coarse time bucket. Time ranges want bucketing into terms —
`day=19000` and so on — combined with `Or`, rather than an id range, unless id
order genuinely carries time.

The term space is yours to namespace. haiiie records no mapping from your names
to term numbers; keep that mapping with your ingest code and back it up with the
data, because without it the posting lists remain readable and meaningless.

## Filters

A filter composes `All`, `None`, `Term`, an explicit id set, a half-open id
range, and `And` / `Or` / `Not` over those.

`Not` is complemented **within the live set**, not over the whole id universe.
An unbounded complement spans 2^64 and is meaningful for a count rather than
for a candidate list.

**An empty `And` matches everything and an empty `Or` matches nothing.** Each is
the identity of its own operation, and they are opposites, which matters if you
build clause lists in code:

```rust
// A request with no filters selected.
Filter::And(vec![])   // every live document
Filter::Or(vec![])    // no documents
```

So a request that assembles `And` clauses from optional parameters returns the
whole corpus when none are set, and the same shape built with `Or` returns
nothing. Neither is a special case in haiiie and neither is an error; if you
want "no filters means no results", write `Filter::None` rather than relying on
an empty list to mean it.

## What lives outside haiiie

Your documents, your source embeddings, your id assignment, the choice of fit
and validation corpora, and the mapping from your domain vocabulary to term
numbers remain yours. The offline fitter produces the shared residual model. A
residual-model server accepts floats at ingest, but retains only their 64-byte
packed codes and returns ids.
