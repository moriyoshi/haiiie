# haiiie Architecture

## Repository Layout

<!-- BEGIN LAYOUT (checked by scripts/check-layout.py) -->
```text
AGENTS.md                      agent protocol; CLAUDE.md symlinks to it
YESNO_REVISION                 exact upstream commit for sibling path dependencies
.agents/docs/                  agent-facing documents (this file among them)
.agents-workspace/             scratch, gitignored
docs/                          user-facing, self-contained (no source paths)
charts/haiiie-sidecar/         operator-managed peer sidecar Helm chart
dist/Dockerfile                pinned haiiied and probe image build
.dockerignore                  keep build context free of scratch and target output
scripts/gate.sh                every check, one command
scripts/check-layout.py        this block against the tree, both directions
scripts/check-deps.py          haiiie-core's dependency budget
scripts/check-yesno-revision.py  pinned clean sibling dependency checkout
scripts/check-docs-selfcontained.py   docs/ names no source paths
scripts/check-slug-citations.py       cited backlog slugs still exist
scripts/check-ci-workflow.py   the workflow's checkout layout agrees with the manifest
scripts/check-test-count.py    OVERVIEW.md's test count against the tree
scripts/check-cli-flags.py     every CLI long flag is named in docs/

haiiie-core/src/lib.rs         crate root; the re-export surface
haiiie-core/src/code.rs        DocId, CodeRef, weight and intersection
haiiie-core/src/compact.rs     offline atomic weight-order compaction and durable ID handoff
haiiie-core/src/error.rs       the crate's one error type
haiiie-core/src/keyspace.rs    every yesnodb key this index uses
haiiie-core/src/score.rs       exact scores, the four metrics, the monotone bound
haiiie-core/src/store.rs       the SetStore/SetSnapshot/Lanes traits and Batch
haiiie-core/src/yesno_store.rs the embedded store over a real yesnodb Db
haiiie-core/src/meta.rs        index metadata, bit-packed into the store itself
haiiie-core/src/index.rs       Index, Writer, the forward row layout
haiiie-core/src/search.rs      the forward and inverted paths, filters, exact top-k
haiiie-core/src/slice.rs       bit-sliced integers, the descent, and the comparator
haiiie-core/tests/score.rs     score ordering and bound properties
haiiie-core/tests/meta.rs      metadata round-trips and refusals
haiiie-core/tests/op_layout.rs point-write batch element width guard
haiiie-core/tests/csa.rs       carry-save against the ripple-carry oracle
haiiie-core/benches/accumulate.rs  carry-save vs ripple vs allocating-composed

haiiie-testkit/src/lib.rs      test apparatus root
haiiie-testkit/src/corpus.rs   boundary-biased corpus generation
haiiie-testkit/src/oracle.rs   the brute-force scorer every kernel is checked against
haiiie-testkit/src/counting_alloc.rs  a counting global allocator, for budgets
haiiie-embed/src/lib.rs        fixed residual codec and deterministic fitting RNG
haiiie-embed/src/residual.rs   persisted 500-sign plus 12-bit norm codec and exact packed scorer
haiiie-embed/src/rng.rs        deterministic normals for offline fitting
haiiie-embed/tests/residual.rs  fixed residual model, layout and exact scoring properties

haiiie-testkit/src/evicting.rs a store that evicts snapshots, to reach the resume path
haiiie-testkit/src/memstore.rs an in-memory SetStore: the second implementation
haiiie-testkit/src/rng.rs      seeded SplitMix64, so a corpus is reproducible
haiiie-testkit/tests/oracle.rs the oracle's own properties
haiiie-testkit/tests/m1_index.rs  both stores x both paths x the oracle
haiiie-testkit/tests/m2_inverted.rs  the inverted path against both forward paths
haiiie-testkit/tests/key_layout.rs   keys stay distinguishable through a persisted leaf
haiiie-testkit/tests/allocation.rs   allocation budgets for the inverted scan
haiiie-testkit/tests/m5_stats.rs     per-block statistics tighten without changing answers
haiiie-testkit/tests/compaction.rs   compaction against the oracle, recovery, locks and ID reuse
haiiie-testkit/tests/m5_resume.rs    resuming after eviction, and explain()
haiiie-testkit/tests/m6_parallel.rs  thread-count independence of every result
haiiie-testkit/tests/blockmask.rs    block masks and cursors against the ordinal list
haiiie-testkit/tests/empty_index.rs  what a search reports when there is nothing to search
haiiie-testkit/tests/churn.rs        overwrite-heavy workloads, checked with the store's own fsck
haiiie-testkit/tests/generators.rs   the corpora produce every container kind

haiiie-proto/proto/haiiie.v1.proto   the gRPC service contract
haiiie-proto/build.rs                generates the types; vendors protoc
haiiie-proto/src/lib.rs              generated types, transport-free
haiiie-peer/src/lib.rs               yesnod socket-backed SetStore adapter
haiiie-peer/src/control.rs           control RPC checkpoint worker
haiiie-peer/src/write.rs             serialized Flight writer for the peer store
haiiie-peer/tests/channel.rs          real-socket peer versus embedded snapshot and scorer
haiiie-grpc/src/lib.rs               service crate root
haiiie-grpc/src/convert.rs           wire types to engine types, total or an error
haiiie-grpc/src/service.rs           the Haiiie service implementation
haiiie-grpc/tests/remote_equals_embedded.rs  over a real socket
haiiie-cli/src/bin/haiiied.rs        the server binary
haiiie-cli/src/bin/haiiie.rs         the client binary
haiiie-cli/src/bin/haiiie-fit.rs     offline fixed residual-model fitter
```
<!-- END LAYOUT -->

Every Rust source file must appear in the block above, and every listed path must
exist; `check-layout.py` verifies both directions. Adding a module therefore costs a
documentation edit, which is the point rather than a side effect.

## Planned crates

| crate | responsibility | direct deps |
|---|---|---|
| `haiiie-core` | key space, store traits, embedded yesnodb store, kernels, planner, top-k, query API | **`yesno-core`, `thiserror` -- budget of 2** |
| `haiiie-testkit` | brute-force oracle, `MemStore`, boundary-biased generators | dev-only |
| `haiiie-proto` | generated protobuf types, **no transport** | prost, tonic (codegen only) |
| `haiiie-peer` | yesnod socket-backed store adapter | haiiie-core, yesno-plugin, yesno-flight, rustix, memmap2, tonic |
| `haiiie-grpc` | binary and model-bound residual service | tonic, tokio, prost, haiiie-embed |
| `haiiie-cli` | server, client and offline residual fitter binaries | clap, tokio, tonic, nalgebra, rayon, sha2 |
| `haiiie-embed` | fixed residual codec, model identity and exact scorer | haiiie-core, sha2 |

## Data Model

The unit of everything is a **Block**: ordinals `[b*65536, (b+1)*65536)`. One chunk of
every posting list, one chunk of the filter, one chunk of every weight plane. Every
kernel, bound, statistic and parallel task is defined on a Block.

`DocId` **is** the yesnodb ordinal. No dictionary in v1. A deleted ordinal is retired
and returns to the pool only after a compaction that has rewritten every
key for its Block -- a reused ordinal whose old posting bits survive produces a
*silently wrong score*, which no test that checks only cardinality can see.

`compact(directory, namespace)` acquires the exclusive database directory lock and
rewrites the entire namespace in one batch. Live rows are packed by descending weight,
breaking ties by ascending old ID, matching the measured construction;
forward rows, `LIVE` and `ATTR` are replaced together. Binary indexes also
rebuild `DIM`, `ZPLANE` and `STAT`; model-bound residual indexes omit them. The
old-ID mapping is stored in `REMAP`, one 64-bit row per new ID with a presence bit at
63; `COMPACTION` holds a presence marker and the source store version. These land in
the same commit as the data. Repeating `compact` returns the pending mapping.

`Index::open` refuses a pending namespace until `acknowledge_compaction` removes both
handoff keys atomically. The caller must first durably migrate objects, saved ID filters
and optional floats using `old_ids[new_id]`. This explicitly changes caller IDs; there
is no automatic ingest reordering or permanent ID translation. Other namespaces stay
intact. Compaction holds live rows and the rewrite batch in memory. With
yesno-core at commit `03cd5a3` or a descendant, an open that is the first for
this directory in its process can punch slabs freed by the open-time allocator
rebuild on its next dirty checkpoint. `compact` and `acknowledge_compaction`
each open their own store, so they return allocated disk space when each call
runs in a separate fresh process; an earlier open in the same process makes
freed slabs reusable only. On 262,144 D=256 documents, a measured
fresh-process delete/refill/compact cycle settled at 26.82 MB allocated versus
18.24 MB fresh, while the same-process cycle plateaued at 58.77 MB. No
online/region-batched compaction or CLI/gRPC entry point is implemented.

The directory lock excludes ordinary `YesnoStore` handles. Upstream's
`Db::open_reader` bypasses it, so foreign readers wrapped by `YesnoStore::from_db`
must be stopped explicitly by the caller along with other application users.

### Key space

Every key is one 64-bit number, laid out as three fields:

```text
 63          56 55                    20 19                 0
+--------------+------------------------+-------------------+
|  NAMESPACE   |          KIND          |       INDEX       |
|    8 bits    |    36 bits, values     |   20 bits, max    |
|              |     0x01 .. 0x41       |     1 048 575     |
+--------------+------------------------+-------------------+

key = (NAMESPACE << 56) | (KIND << 20) | INDEX
```

`NAMESPACE` makes an index relocatable: several haiiie indexes, or one haiiie
index and an application's own keys, can share a database when their keys do
not overlap. Creation checks every defined haiiie kind range for occupied keys;
later foreign writes must still avoid those ranges. yesnodb hashes keys to
shards with SplitMix64, but all chunks under one key remain on one shard. The
single FWD key held 49.8% of allocated bytes for a measured binary index and
95.4% for a residual index, so hashing keys does not balance bytes here.

#### The kind map

Kinds are grouped by purpose in their high nibble, so a key read in a hex dump
sorts into its role. `INDEX` means something different in each row, and that is
the field most easily misread.

| KIND | name | `INDEX` field is | written by |
|---|---|---|---|
| `0x01` | `META` | always 0 | both layouts |
| `0x02` | `LIVE` | always 0 | both layouts |
| `0x10` | `DIM` | dimension `d`, `0 .. dims-1` | binary only |
| `0x11` | `ZPLANE` | plane `j` of `z = D - |x|` | binary only |
| `0x20` | `FWD` | always 0 -- **one key, not one per block** | both layouts |
| `0x21` | `STAT` | block number | binary only |
| `0x30` | `ATTR` | the caller's own term | both layouts |
| `0x40` | `COMPACTION` | always 0 | compaction handoff |
| `0x41` | `REMAP` | always 0 | compaction handoff |

The current single-key FWD layout and 20-bit INDEX width are part of the
persisted format. A split using FWD(0) would overlap the existing key and
silently return zero rows for chunks sent to other split keys unless the
metadata version rejects old indexes. Widening INDEX would also move META
under the generic formula, bypassing that very version check during create.
A layout change must keep or probe META's current numeric key and fail loudly
for an incompatible directory. This is a future migration requirement, not
behavior the current version marker can enforce by itself.

`META` carries geometry and an optional opaque model identity; **the model
identity's presence selects the forward-only residual layout**, which omits `DIM`, `ZPLANE` and
`STAT` entirely because residual scoring never reads them. Core refuses binary
scoring on such an index rather than scoring from absent postings.

Only `DIM`, `ZPLANE`, `STAT` and `ATTR` use `INDEX` as a variable.
`Index::create` bounds dimensions and their derived `ZPLANE` count;
`max_doc_id` bounds the `STAT` block number; writes and query filters
check caller-assigned attribute terms. `KeySpace::key` also asserts the
field bound in debug builds as a defense against internal mistakes.
Without those runtime checks, an overflowing index can carry into the
kind field and silently name another key in release builds.

#### Three ordinal axes, which is the part to get right

A key's *ordinals* mean different things depending on its kind. Reading a key
on the wrong axis is the error this table exists to prevent.

| axis | kinds | an ordinal is |
|---|---|---|
| **document** | `LIVE`, `DIM`, `ZPLANE`, `ATTR` | a `DocId`, directly -- `DocId` **is** the yesnodb ordinal |
| **bit position** | `FWD` | a bit of a packed code, at `forward_chunk * 65 536 + row * row_bits + bit` |
| **packed record** | `META`, `STAT`, `COMPACTION`, `REMAP` | a bit index into a small encoded blob |

The document axis is what makes the design work at all: a posting list *is* the
set of documents with that bit, so a set-algebra engine scores similarity
without knowing what similarity is.

The packed-record axis carries its own framing in each case, because a set of
set-bit positions does not record its own length. `META` sets a sentinel bit at
`len * 8`, one past the final byte, so the encoding is self-delimiting for any
blob -- without it a header ending in zero bytes decodes short. `STAT` puts a
presence marker at ordinal 0, then `w_min` in ordinals `1 .. 32` and `w_max` in
`33 .. 64`, so an all-zero `w_min` is distinguishable from an absent entry.
`REMAP` stores one 64-bit row per new ID with a presence bit at 63.

#### The forward row, and why `FWD` is one key

```text
FWD ordinal space, D = 256 ( row_bits = 256, rows_per_block = 256 )

  FWD chunk 0                            FWD chunk 1
|<--------- 65 536 ordinals ---------->|<--------- 65 536 ---------->|
| row 0  | row 1  | ... | row 255      | row 0  | ...
|<-256-->|<-256-->|     |<----256----->|
  doc 0    doc 1          doc 255        doc 256
```

`row_bits` is `dims` rounded up to a multiple of 64 and at least 64, so a row
never straddles a word; `rows_per_block` is `65 536 / row_bits`, so a row never
straddles a chunk. At D = 256 that is 256 rows per block; at the residual
layout's D = 512, 128 rows. The physical FWD set is exactly
`View::interleaved(row_bits)` over document ordinals. A 2026-09-27 peer
check found 0 mismatches in 200,000 sampled ( document, bit ) pairs on the
binary D=256 fixture, and equal total set-bit counts across FWD and DIM.
This is a packing equivalence, not a new scoring API: a count across all
constituents gives code weight, while an exact query needs a query-selected
intersection. Existing `view_fold` returns only Any, All or Parity.

`FWD` was once one key per block, so that a whole-key `load` was bounded. That
cost a fresh stream open per chunk -- the forward path's dominant cost at
exactly the selectivities it is chosen for. Merging them moved no ordinal, since
a row's address is already global and its chunk index is already `block`, and it
retired an implicit ceiling: a block number in the 20-bit `INDEX` field capped an
index at `2^20 * rows_per_block` documents.

#### Bitmap layout: one convention, used three ways

Every bit array in this crate is **word-packed, LSB-first** -- bit `i` lives in
word `i / 64` at position `i % 64`. That is not a house preference: it is
bit-identical to a Roaring bitmap container and to an Arrow boolean buffer,
which is what lets a chunk hand off as a refcount bump rather than a decode.

```text
one u64 word, LSB-first

  bit 63                                                    bit 0
    |                                                         |
  [ b63 b62 b61  ...............................  b2  b1  b0 ]
                                                    ^
                                        ordinal i -> word i/64, bit i%64
```

**1. A block plane.** One block is 65 536 ordinals, so one plane is
`BLOCK_WORDS = 1024` words, and `BlockMask = [u64; 1024]` is the unit every
kernel works on -- one chunk of a posting list, of the filter, or of a weight
plane, all the same shape.

```text
BlockMask: 1024 x u64 = 65 536 bits = one block = one yesnodb chunk
|<-- word 0 -->|<-- word 1 -->| ... |<-- word 1023 -->|
  ordinals 0-63  ordinals 64-127      ordinals 65472-65535
```

**2. A forward row.** The same packing, but the ordinals are *bit positions*
rather than documents. Dimension `j` of the document at `row` sits at
`block * 65 536 + row * row_bits + j`, so a `CodeRef::Dense` word array drops
into the row unchanged.

**3. A bit-sliced accumulator.** `Slice` holds `L` planes of that same
`BlockMask`, read across the planes rather than along one:

```text
value( o ) = sum over j of 2^j * [ bit o of plane j ]

  plane 2  [ .. 1 .. ]  significance 4
  plane 1  [ .. 0 .. ]  significance 2
  plane 0  [ .. 1 .. ]  significance 1
              ^ ordinal o                value( o ) = 5
```

One plane operation adds a bit to all 65 536 accumulators at once, which is the
entire reason the inverted path is competitive: **a posting list is already
exactly the addend.** `Csa` carries the same plane shape plus at most one
unpaired addend per level. The default inverted kernel instead borrows all
posting lanes for a block, compresses sixteen-lane groups within 16-word tiles,
and writes the same bit-sliced result. Plane-wide `Csa` and ripple remain forced
oracles; stores without borrowed lanes use the plane-wide fallback.

#### Block statistics, and why absence is a valid state

`STAT` holds one entry per block -- key index **is** the block number -- and the
entry is two 32-bit weights packed as ordinals in their own windows:

```text
STAT( block ) ordinal space

  ord  0      presence marker, always set
  ord  1..32  w_min, bit ( o - 1 )    the smallest |x| among live documents
  ord 33..64  w_max, bit ( o - 33 )   the largest
  ord 65+     rejected on decode
```

**The presence marker at ordinal 0 is what makes the encoding total.** Without
it an all-zero `w_min` is indistinguishable from an absent entry, because a set
of set-bit positions cannot represent "zero" and "nothing" differently.

Decoding rejects rather than repairs. A missing marker, an ordinal past 64, or
`w_min > w_max` all return `None`, which the scan reads as *no statistics for
this block* -- not as an error and not as a zero. So a corrupt or partial entry
degrades to the loose bound instead of pruning on a value it should not trust.

**Statistics are invalidated by live code writes and deletes, never refreshed
by them.** A real code write or live-document delete removes that block's
`STAT` key in the same atomic batch; absent-ID deletes and attribute-only
changes leave it intact. `refresh_stats` recomputes them later as a separate
pass. The asymmetry is deliberate and it is a correctness argument, not a
performance one:

```text
   live-row write in b -> delete STAT( b )        same batch
   refresh_stats()   ---->  recompute, rewrite      separately, later
   scan sees no STAT ---->  loose bound             slower, never wrong
   scan sees STAT    ---->  tightened by w_min      only where it exists
```

`w_min` tightens a bound, and **a bound that is too tight drops results
silently** -- no crash, no count change, just a document missing from the
top-k. Refreshing on write would be faster and would make correctness depend on
the refresh being right every single time; invalidating makes the failure mode
"slower" instead of "wrong". Residual indexes write no `STAT` at all, and their
`refresh_stats` is a no-op returning the current version.

#### The residual packed code, 64 bytes exactly

The model-bound layout spends its 512 bits with no slack, which is why the
tail is irregular:

```text
byte   0 .......................... 61 | 62      | 63
      |<-- 496 signs, 8 per byte -->|  |<------ u16 LE ----->|
       sign 0 .. sign 495              | s496..s499 | norm 0..11 |
                                        bits 0-3     bits 4-15
```

496 signs fill bytes 0 to 61; the last four signs occupy the low nibble of
byte 62; and the 12-bit reciprocal-norm code occupies the top 12 bits of the
little-endian `u16` spanning bytes 62 and 63. `500 + 12 = 512` bits, so
`row_bits` is 512 and `rows_per_block` is 128.

#### The compaction handoff, which is two keys that must appear together

Compaction rewrites a namespace and leaves a durable handoff the caller has to
acknowledge. Both keys carry index 0, and both use the ordinal axis as a packed
record -- but in two different shapes.

**`COMPACTION` ( `0x40` ) is a marker plus one version.**

```text
  ord  0      presence marker, always set
  ord  1..64  source store version, bit ( o - 1 )
```

**`REMAP` ( `0x41` ) is a dense array of words indexed by new ID.** New ID `n`
owns the 64 ordinals `[ n*64, n*64 + 64 )`, and within that word:

```text
REMAP ordinal space

  new id:      0                1                2
            |<--- 64 --->|   |<--- 64 --->|   |<--- 64 --->|
  ordinal:   0 ......  63     64 ..... 127    128 ..... 191

  one word, per new id:
    bit 63     presence, always set
    bits 62..0 the old id this row came from
```

**The presence bit at 63 is what makes old ID zero representable**, the same
problem `STAT`'s marker and `META`'s sentinel solve in their own shapes: a set
of set-bit positions has no way to say "a row exists whose value is zero".

Reading the handoff validates rather than trusts, and rejects the whole thing
on any inconsistency:

```text
  marker absent                      -> no pending compaction
  max( REMAP ) != count * 64 - 1     -> reject: the array is not exactly count words
  word missing bit 63                -> reject
  word carrying bits above max_old   -> reject
```

`Index::open` **refuses a pending namespace** until `acknowledge_compaction`
removes both keys in one atomic batch. That is the point of the pair: the
caller must first durably migrate its own objects, saved ID filters and
optional floats using `old_ids[new_id]`, and until it says it has, reopening
would hand back an index whose IDs have silently moved. Repeating `compact`
returns the pending mapping rather than recompacting.

#### Block alignment is load-bearing

`BLOCK_ORDINALS` is **derived from `yesno_core::CHUNK_CARD`, not a literal that
matches it**, and `block_of(ordinal)` is `ordinal >> CHUNK_BITS`. That number is
handed straight to a chunk `seek` as a *prefix*, so a block that is not exactly
one chunk does not merely lose an optimization -- it reads the wrong chunk. The
constant once read `1 << 16` beside a comment claiming the alignment held "by
construction", when what held it was two independent sixteens agreeing.

#### There is no `SEGMENT` field

An early draft carried one to bound `Snapshot::load` on a premise that was
false -- `load` is zero-copy, so its cost is allocations rather than resident
bytes -- and upstream has since added `Snapshot::key_stream` regardless.

Every key comes from one `KeySpace` struct; nothing else hardcodes one. Keys are
numbers on disk, and two call sites computing one the same way by coincidence
means the format is whatever they happen to agree on.

## Served yesnod store boundary

`haiiie-peer` implements the same `SetStore` without opening the database
directory. Each snapshot opens a Unix-channel connection, pins one yesnod
snapshot handle, and uses that handle for scalar reads, value-paged key reads
and scoring lanes. The first receive uses ancillary-data support: arena mode
passes a sealed memfd via `SCM_RIGHTS`, while inline mode starts with the
ordinary greeting. The adapter reads `ServerHello.max_lanes`, `max_handles`
and `max_blocks`; a wide query splits into lane handles on the same snapshot.
If the handle budget is exhausted, the exact addressed block-read fallback
remains available. Notifications and snapshot-too-old faults expire that
snapshot; a new connection is never substituted underneath an old cursor.

The channel is read-only. `PeerStore::with_flight_auth` sends ordered batches
through yesnod Flight `PUT_APPLY`, which acknowledges the committed version;
`PeerStore::with_control` calls the authorized checkpoint RPC and refuses a
watermark behind this peer's last acknowledged commit. Flight lacks native
chunk patches, so the residual writer declines packed tiles before constructing
a patch and emits exact point operations. A lost Flight commit reply is
indeterminate: the writer stops rather than replaying it. The optional bearer
token files are reread at each write or checkpoint. `haiiie-core` keeps its
exact two-dependency budget; transport and runtime dependencies live in the
peer crate.

A serial binary or residual scan keeps the snapshot that supplied its last
block bound. Reopening between finding that bound and scoring could read a
newer version with live documents beyond the old bound, silently omit them,
and claim the newer version. Parallel binary scoring likewise shares that
initial snapshot across workers. A deterministic concurrent-growth test
forces this boundary.

## Two Scoring Paths, One Planner

`Gather` reads forward codes for filtered documents only; `DenseScan` streams them;
`Inverted` accumulates bit-sliced over posting lists. Cost is flat in in-Block
selectivity for `Inverted` and linear for `Gather`, which is why the filtered regime --
haiiie's reason to exist -- is served by the forward path, not the inverted one.

Forward scoring groups candidates by forward chunk and consumes them inside
`Lanes::with_block`. The yesnodb adapter lends aligned bitmap words; arrays, runs,
unaligned bitmaps and stores without borrowing retain the full-mask fallback.
The cursor still performs upstream identity, checksum and snapshot checks. This
avoids an 8 KiB copy for a 32-byte D=256 row; it does not promise that upstream
will never read the whole payload to verify its checksum.

Scoring scans also keep per-worker LIVE and filter-term cursors. Each term
occurrence has its own lane, in evaluation order, so repeated terms do not
rewind a shared lane. Serial retries rebuild admission and scoring cursors on
the new snapshot and discard partial hits before checking admission: a block
that becomes empty on retry must not retain results from the failed attempt.
`count()` uses the same admission cursor, and `explain()` holds a LIVE cursor
while walking blocks; neither rebuilds a whole-key plan per block.

Both scoring cursor families open lazily, when a block actually selects that
path. An all-forward scan does not open inverted posting lanes; an all-inverted
scan does not open forward codes. A declined cursor is remembered until retry,
not renegotiated per block. Retries reset both families before either is reused.

An opaque signed-integer row scorer extends the same forward traversal to codecs
whose ranking key is not one of the four binary metrics. Core still owns filter
admission, borrowed row lifetimes, retry isolation and exact top-k; the codec owns
only the interpretation of one row. A failed admission block's bounded local heap
is discarded before retry, because one admission block can span hundreds of
forward chunks. Large unfiltered row scans divide disjoint ID ranges across at
most eight workers sharing one immutable snapshot, merge exact local top-k lists,
and discard the whole attempt on eviction. The measured 32,768-live-row gate
keeps smaller and filtered scans serial. The residual embedding layer checks the
index's model identity before constructing this search.

The gRPC boundary names residual embeddings separately from binary codes and
transient residual embeddings. A server loads the serialized model at startup and
validates its content identity against metadata. Float documents are encoded
before entering the ordinary writer, so only their 64-byte rows commit with
liveness and attributes. Model-bound indexes refuse binary metric queries; a
residual hit carries its complete signed integer key rather than populating
binary intersection fields with invented values.

## The Unifying Result

Because `a <= w` always, every supported similarity is bounded above by a monotone
function of the inner product `a = |q AND x|` alone: `a` for inner product, `a - m` for
Hamming, `a/m` for Jaccard, `sqrt(a/m)` for cosine. So there is one accumulator, one
descent, and one refinement loop against `g_inverse(tau)`.

Hamming is linear and skips the loop: store `z = D - |x|` and maximize `S = 2a + z`,
since `-H = S - m - D`. Storing the *complement* makes the query-time step an addition
rather than a subtraction.

## Policies That Constrain Changes

**P1. Exactness is not negotiable.** See `AGENTS.md`.

**P2. Accumulators are not `Container`s.** Routing bit-planes through yesnodb's set
operators allocates per posting list per Block and pays cardinality bookkeeping nobody
reads. haiiie owns plain `[u64; 1024]` arrays. Only an allocation test can see this
decay; correctness tests cannot.

**P3. The generic implementation is the oracle and is never deleted.** Adopted from
upstream, where it is the reason two missing kernel arms were findable at all.

**P4. `unstable_arrow` is quarantined.** It is semver-exempt upstream. One module names
it; everything else goes through that module.

**P5. Set algebra belongs upstream.** If a helper would be as useful to a caller ranking
a cohort by a stored attribute as it is to us, it is yesnodb's, and the honest move is a
prescription rather than a local copy.

**P6. A statistic is a summary, and a summary can swallow a fault.** Anything that
prunes, buckets or counts needs its sabotage calibrated to survive the summarization
between the injection and the observable.
