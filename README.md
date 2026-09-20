# haiiie

Haiiie is an exact stored-code vector search engine built on
[yesnodb](https://github.com/moriyoshi/yesnodb). It ranks bit-packed codes and
combines ranking with boolean filters. The server can embed yesnodb or run beside
yesnod as a peer, leaving yesnod in charge of the database directory.

> **Pre-release:** haiiie has not served a production workload. The measurements
> below come from one machine and named corpora or fixtures. They establish
> correctness and specific trade-offs, not a latency, recall or availability
> guarantee for another deployment.

## What a result means

An ordinary index returns the true top-k under one of four stored-code scores:
binary inner product, Hamming, Jaccard or cosine. A model-bound residual index
stores a 64-byte code per document and returns the true top-k under its named
signed-integer residual score. Both modes admit arbitrary boolean filters, and
ties are resolved deterministically. Query embeddings are transient; residual
ingest does not retain document floats.

**Exact refers to the stored-code score, not the original float embedding.**
Encoding can change which float neighbours rank highest. Residual recall must
be measured against exhaustive float search on the intended corpus and query
set; [the recall guide](docs/recall.md) explains the split and rank-boundary
measurements needed to interpret it. Filters do not introduce additional
search error: haiiie ranks the admitted documents exactly under the same code
score.

## Where it fits

Haiiie is aimed at exact top-k under selective filters, such as searching within
a tenant and a set of tags. Binary indexes keep both forward codes and inverted
posting lists. The planner chooses a scoring path per block from filter
selectivity; callers can force a path for comparison. Model-bound residual
indexes use the forward codes without binary postings. The default inverted
scorer uses a word-tiled bit-sliced accumulator; the slower carry-save and
ripple implementations remain exact oracles.

Unfiltered search still scales with corpus size. On matched queries, FAISS
BinaryFlat is **2.51x** faster than haiiie's embedded Auto path. FAISS is a
vector-index library, not a transactional OLTP store. It can [save and load indexes](https://github.com/facebookresearch/faiss/wiki/Index-IO,-cloning-and-hyper-parameter-tuning),
but the benchmark times in-memory index operations. Haiiie stores codes,
attributes and postings in yesnodb's transactional database; a yesnod-backed
deployment can use server-managed replication and recovery. Search latency
compares the ranking paths, not the capabilities of complete serving systems.

## Performance snapshot

The baseline search measurements use yesno `b712a03`, haiiie `ed930ae`
code, and FAISS CPU 1.15.1 where named. The planner retake below compares that
baseline with the revised threshold. Quoted timings passed CPU and disk quiet
gates; the [journal](.agents/docs/JOURNAL.md) records construction, excluded
cells and source revisions.

| Workload | Result |
|---|---|
| Exact binary, 1,048,576 GloVe-derived D=256 codes | Haiiie and FAISS BinaryFlat returned identical distances and ID sets on **1,000/1,000** Hamming top-10 queries. On the same first 100 queries, five-pass one-thread medians were **2.314 ms** haiiie versus **0.921 ms** BinaryFlat. |
| Filtered, one in 1,024 IDs admitted | On those first 100 queries, haiiie took **0.380 ms** at **0.604 recall@10** against exact float-cosine truth. HNSW ef256 took **0.323 ms** at **0.342** recall; ef1024 took **1.557 ms** at **0.698** recall. |
| Residual code, held-out COCO | A fixed **64-byte** code averaged **0.7631 recall@10** across three fits, versus **0.7671** for a 72-byte stored-norm control. |
| Separate-process peer, 262,144 D=256 documents | **0 full-hit mismatches** in 19,200 original comparisons and 4,800 bitmap-heavy rerun comparisons. On natural one-thread queries, Run-shaped lanes took **0.38-0.40 ms** embedded, **2.39-2.64 ms** arena and **1.72-1.89 ms** inline; bitmap-heavy lanes took **0.40-0.48**, **2.95-3.25** and **3.79-4.90 ms**, respectively. |

On the same 1M-document D=256 fixture, the revised wide-query planner now
tracks Gather at one-in-12, one-in-16 and one-in-24 filters. A quiet paired
old/new rerun at one in 24 measured **2.21-2.49 ms** per query before versus
**1.65-1.67 ms** after, with identical hits across paths. Narrower-query
thresholds are unchanged pending a qualified width sweep.

Ingest has a different frame from FAISS's in-memory `add`. One qualified
embedded binary point-write run reached **28,546 documents/s** and then spent
**1.40 s** checkpointing. On 96,903 COCO residual documents, 20-worker
encoding plus embedded packed writing and checkpoint took **1.10-1.27 s**;
the separate-process peer's Flight point-writer phase varied **5.85-10.27 s**.
The peer cannot send packed chunk patches, so these are different write paths.

Concurrent updates affect query tails. On a 2,097,152-document synthetic
binary index, changed-code writes with policy checkpoints yielded **21.97 ms p99**
at eight query threads, versus **5.40 ms p99** without a writer in the
qualified cells. The old same-code writer harness now emits no bit changes
and is only an idempotent-write control. Four of eight current timing cells
failed quiet-host gates; qualified cells still had background compile load.

## Running it

Embedded mode opens a local yesnodb directory. A simple binary index can be
created and served with:

```console
cargo build --release
./target/release/haiiied --data-dir ./data --namespace 1 --create-dims 256 &
./target/release/haiiie describe
```

The data directory is exclusively opened by the storage engine. For a
server-owned directory, run yesnod as the sole owner and start haiiied with
`--peer-socket`; it reads snapshots and container lanes over yesnod's local Unix
channel. Optional Flight and control endpoints enable atomic point writes and
checkpoint requests. The [operations guide](docs/operations.md#what-runs) has
the exact flags, version pin and cutover behavior. The channel is local, not a
remote posting-list service; shipping posting lists over the network was a
separate, much more expensive experiment.

The [sidecar Helm chart](charts/haiiie-sidecar/README.md) drafts this shape for
yesno's Kubernetes operator. Its default profile is an unauthenticated
evaluation cluster. Its cert-manager profile opens an existing index read-only:
the operator does not project client certificates into the plugin, and haiiie's
peer writer does not yet use client mTLS. The chart has been rendered against
the operator CRD but has not been installed on a live cluster. Haiiie's
client-facing gRPC service has no authentication or TLS; keep it behind a
trusted boundary.

For embedded library use, the index owns the code and attribute operations:

```rust
use haiiie_core::{CodeRef, DocId, Filter, Index, Metric, YesnoStore};

let index = Index::create(YesnoStore::open("./data")?, 1, 256)?;
let mut writer = index.writer();
writer.put(DocId(0), CodeRef::Dense(&code))?;
writer.attr(DocId(0), 7);
writer.commit()?;

let hits = index.search()
    .code(CodeRef::Dense(&query))
    .metric(Metric::Hamming)
    .k(10)
    .filter(Filter::Term(7))
    .execute()?;
```

An embedded caller with a large ingest must periodically flush its writer;
`flush_if_large` bounds the pending batch. Each committed document remains
atomic, but a failed stream can leave an already committed prefix. See
[getting started](docs/getting-started.md) for the full ingest and query flow.

## Current limits

* **No production operation or Kubernetes proof.** The operator sidecar chart
  has not been exercised through follower catch-up, leader promotion and
  rebootstrap in a live cluster. Haiiie does not implement its own cluster
  coordinator; yesnod owns storage replication and recovery in peer mode.
* **No secure writable sidecar yet.** The read-only mTLS chart profile needs an
  existing index. The evaluation profile uses plaintext local endpoints, and
  haiiie's public gRPC listener has no authentication or TLS.
* **No approximate search mode.** Centroid-and-radius pruning was measured,
  found ineffective on the tested codes and removed. The product keeps an
  exact solver for its stored-code score.
* **No float side-store or rerank.** The 64-byte residual code is the durable
  representation. It trades some float-oracle recall for fixed code storage.
* **No online compaction API.** Offline weight-order compaction returns an ID
  remapping the application must durably acknowledge. Disk return depends on
  the yesnodb revision and fresh-process checkpoint conditions described in
  [operations](docs/operations.md#deletes-and-compaction).
* **No end-to-end capacity claim.** Embedded and peer queries now have
  single-corpus measurements, but no real workload has a matched capacity
  measurement. The planner needs a wider query-width crossover grid; peer
  latency needs broader block-count and concurrent-query measurements, and
  write-time tails need a current measurement, before they become deployment
  sizing numbers.

## Documentation

* [Getting started](docs/getting-started.md)
* [Data modeling](docs/data-modeling.md)
* [Query language](docs/query-language.md)
* [Recall and codec measurements](docs/recall.md)
* [Operations](docs/operations.md)
* [Sidecar chart](charts/haiiie-sidecar/README.md)
* [Research and implementation journal](.agents/docs/JOURNAL.md)

## Licence

Either of [Apache 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
