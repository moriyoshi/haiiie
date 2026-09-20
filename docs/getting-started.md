# Getting started

## Embedded

```rust
use haiiie_core::{CodeRef, DocId, Filter, Index, Metric, YesnoStore};

// 256-bit codes, in namespace 1 of a data directory.
let index = Index::create(YesnoStore::open("./data")?, 1, 256)?;

let mut w = index.writer();
for (id, code) in corpus {
    w.put(DocId(id), CodeRef::Dense(&code))?;
}
w.commit()?;          // one atomic commit for everything above

let hits = index.search()
    .code(CodeRef::Dense(&query))
    .metric(Metric::Hamming)
    .k(10)
    .execute()?;

for hit in &hits.hits {
    println!("{} score={:?} shared={} weight={}",
             hit.id.get(), hit.score, hit.inter, hit.weight);
}
```

A writer accumulates changes and commits them **atomically**. A document's
code, its liveness and its attributes land together, so a concurrent reader
never sees a document that is live but has no code.

## Over gRPC

```console
haiiied --data-dir ./data --create-dims 256 --listen 127.0.0.1:50071
```

```console
haiiie describe
haiiie search "ffffffff00000000,0,0,0" --metric hamming --k 10
haiiie explain "ffffffff00000000,0,0,0" --metric jaccard
haiiie count --term 7
```

Those commands reach `http://127.0.0.1:50071`, which is where the server above
listens. `--server <url>` points them somewhere else:

```console
haiiie --server http://db.internal:50071 describe
```

It comes before the subcommand, not after.

Codes on the command line are hex words, least significant first, comma
separated. Ingest over gRPC is a client-streaming call: send documents in
batches, receive one commit version.

## Residual embeddings

`haiiie-fit` consumes headerless row-major little-endian `f32` vectors. Give it
a representative fit corpus, the embedding width, an explicit deterministic
seed, and a new output path:

```console
haiiie-fit --input fit.f32le --dims 512 --output model.bin --seed 43 --threads 4
```

The fitter normalizes every row, trains the fixed 500-sign residual encoder,
fits and integer-quantizes its decoder, and calibrates the 12-bit norm field. It
refuses an existing output file. Its report includes the input SHA-256, row
count, seed, achieved thread count, model identity and quantizer parameters.
The seed changes the fitted model, so compare several fixed seeds on a separate
validation set for your corpus and record the chosen command. Four threads is
the study configuration and the default; changing the width does not change the
stored format.

The resulting model creates a model-bound 512-bit index. The server checks its
content identity against the index before it starts serving:

```console
haiiied --data-dir ./residual-data --create-dims 512 --residual-model ./model.bin
haiiie search-vector "0.12,-0.35,0.91,0.04" --k 10
```

The complete embedding width comes from the model; the four values above are
only illustrative. A gRPC ingest stream carries `IngestRequest` messages, each containing ordered
`Mutation` entries. For a residual put, select `Document.value.embedding`; for an ordinary binary
put, select `Document.value.binary_code`. The server normalizes and encodes an
embedding, then commits the resulting 64-byte row atomically with liveness and
attributes. It retains no document float.

Search selects exactly one `Query.kind`: `residual` carries an embedding and
`binary` carries a code plus metric. A residual hit selects
`SearchHit.score.residual` and carries the complete signed integer ranking key
in its `value`; a binary hit selects `SearchHit.score.binary` and carries its
exact rational separately from the approximate display value. The protobuf
oneofs make mixed representations unrepresentable. A representation that does
not match the index kind is refused before scoring or writing.

## After a bulk ingest

Run the statistics rebuild. It computes a weight range per block, which
tightens the bound the ratio metrics prune with — on one measured corpus it cut
the candidate set from 42.5% of documents to 0.1%.

```console
haiiied --data-dir ./data --refresh-stats
```

Statistics are **deleted by code writes and live-document deletes in their
block** and are never refreshed in place. Attribute-only changes and absent-ID
deletes leave them intact. A block without statistics is scored with a looser
bound: slower, never wrong. That is deliberate — a stale bound would drop
results silently.

## Attributes and filters

An attribute is a boolean tag on a document, and a filter is an arbitrary
boolean expression over attributes, id sets and id ranges.

```rust
w.attr(DocId(id), 7);

index.search()
    .code(CodeRef::Dense(&query))
    .filter(Filter::And(vec![
        Filter::Term(7),
        Filter::Not(Box::new(Filter::Term(9))),
    ]))
    .k(10)
    .execute()?;
```

Filters are evaluated **before** scoring, so a selective filter makes a query
cheaper. There is no per-filter index to declare and no recall penalty for
filtering hard.
