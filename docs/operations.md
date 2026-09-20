# Operations

## What runs

In embedded mode, one haiiied process opens one data directory. The storage
engine takes an exclusive lock on it, so a second writer is refused rather than
queued. Several indexes can share one directory through namespaces; each haiiied
serves one namespace. In peer mode, yesnod alone opens the directory;
haiiied uses its served Unix channel for snapshot reads.

```console
haiiied --data-dir ./data --listen 127.0.0.1:50071 --namespace 1
```

For a yesnod-owned directory, use yesnodb revision `b712a03624d36f2a9cb9755c398387395dc3b0c3`.
That revision includes the persisted Run-lane and inherited-slab fixes,
the channel cutover and Unix-socket hardening, the shared lane encoder, and
compact WAL logging for chunk images. Use a clean checkout at that revision
for embedded builds as well. Configure the plugin channel socket and start
haiiied separately:

```console
haiiied --peer-socket /run/yesno/haiiie.sock --namespace 1 --listen 127.0.0.1:50071
```

This form serves reads without opening the data directory. yesnod binds its
channel socket before opening the database, so socket existence alone is not a
readiness check; start haiiied after yesnod reports ready, or restart it if
startup finds no database open. To enable ingest,
add `--flight-endpoint http://127.0.0.1:50051` for the same yesnod leader.
Use `--flight-token-file /path/to/token` if Flight requires a bearer token;
the file is reread for each batch. `--control-endpoint http://127.0.0.1:50052` enables `SetStore::flush` through
yesnod's authorized checkpoint RPC; use `--control-token-file /path/to/token`
when its control plane requires a bearer token. The file is reread for each
checkpoint. TLS endpoints can be used for the control connection. Keep the
Flight endpoint and channel pointed at the same database: the adapter cannot
prove that two independently configured endpoints share a version history.
At follower cutover, yesnod closes old peer channels. An in-flight strict
query may fail with snapshot expiration; the next query opens a new connection.
Queries configured to resume on eviction may retry within their retry bound.
The lane channel is read-only; Flight commits ordered point and range mutations
atomically. Flight cannot send packed chunk patches, so residual tile ingest
uses the exact point-write path. A lost Flight commit acknowledgment is
indeterminate and stops subsequent writes from this peer until it is restarted
after reconciliation. The peer obeys the channel's advertised lane and block
limits and splits wide queries across handles on one snapshot. Checkpoint
control is optional; `flush` reports an error if it is not configured or if
the returned checkpoint watermark is behind this peer's last acknowledged
Flight commit.

`--create-dims N` creates the index if the namespace is empty and opens it
otherwise. The server defaults to namespace 1. If namespace 0 contains an
index and namespace 1 is empty, startup without an explicit namespace
refuses to open or create; pass `--namespace 0` to open the earlier index or
`--namespace 1` to deliberately create a separate one. An explicit namespace is also the safest choice in
startup scripts. Each namespace reserves keys for its defined index kinds,
even when a particular index does not use every kind. Creation refuses
pre-existing data in those ranges. In namespace 0 those keys include ordinary
integers 1,048,576 ( metadata ), 2,097,152 ( live documents ) and
33,554,432 ( forward codes ); choose disjoint keys for application data.
Creation cannot protect an existing index from later writes to its keys by
other users of the same store.

## Logs and tracing

`haiiied` writes structured tracing events to standard error. The default filter
is `info`; set `RUST_LOG` to inspect request spans and search outcomes:

```console
RUST_LOG=haiiie_cli=info,haiiie_grpc=debug,yesno_core=warn haiiied --data-dir ./data --namespace 1
```

Startup and shutdown events include the index namespace, code width, model
binding, live-document count and listen address. Completed ingest events include
the committed version, document counts, chunk and packed-tile counts, and elapsed
time. Debug search events report the query kind, scored and returned document
counts, blocks visited and skipped, retries, and store-version range. Query codes,
embeddings and attribute terms are not logged. A failed ingest stream can leave an
already committed prefix; debug prefix-commit events identify its last version.

## Residual indexes require their model

`--residual-model <path>` loads the serialized 500-sign plus 12-bit norm model
used for float-document ingest and residual search. Creating one uses
`--create-dims 512`; the server binds the model's content identity into the
index metadata. Every reopen checks that identity, and a model-bound index is
refused at startup when the option is absent or names a different model.

The model is shared configuration. Document embeddings are encoded at ingest
and are not retained. Query embeddings are prepared per request and score the
stored 64-byte rows with exact signed integer arithmetic. A residual index
stores forward rows, liveness and filter attributes; binary posting lists and
weight statistics are omitted because residual scoring cannot use them.

## Shards, and why the number matters at creation

`--shards N` sets how many storage shards a **new** embedded data directory gets.
It cannot be set in peer mode, where yesnod owns the directory. The
count is recorded in the directory, so reopening ignores the flag and the only
way to change it is to rebuild the index.

It is not only a storage setting. The storage layer guards each shard with a
mutex and takes it once per chunk read, and a query issues thousands of chunk
reads — so the shard count is the number of threads that can read at the same
time, and it is the ceiling on query parallelism no matter how many cores the
machine has. Measured at two million documents on a twenty-core machine, with
the storage layer's own default of 8 for comparison:

| Shards | Ingest | Checkpoint | Serial query | 8 threads | 20 threads |
|---|---|---|---|---|---|
| 8 | 37 700 /s | 0.2 s | 8.4 ms | 5.9 ms | 7.9 ms |
| 32 (default) | 33 200 /s | 0.6 s | 7.7 ms | 3.7 ms | 3.7 ms |
| 64 | 28 600 /s | 1.1 s | 8.4 ms | 3.0 ms | 2.9 ms |

Serial latency is flat across the range, so nothing is lost by a single-threaded
caller. Choose by how the index will be used: an index written once and queried
by many threads wants more shards, and one under continuous ingest that is
queried by one thread wants fewer.

Residual scoring uses `--threads` for unfiltered indexes with at least 32,768
live rows, capped at eight workers. On reopened 512-bit rows, with 128 queries
per timed sample and five samples per arm, 32,768 rows measured 1.503-1.691 ms
serial and 0.879-1.282 ms with eight configured threads; 44,356 rows measured
2.036-2.240 ms and 1.097-1.452 ms. At 16,384 rows the wider production runs
overlapped, so the larger boundary is deliberate. Filtered residual searches
stay serial: spawning workers lost badly on the 694-row control, while the
serial word-mask path measured 0.046-0.093 ms clustered and 0.114-0.119 ms
scattered.

### Scan width, and why more threads stop helping

`--threads N` sets how many threads a single query's scan uses. Without it the
server uses the host's core count.

Read the table above across a row rather than down a column. At the default
shard count, eight threads and twenty measure the same — 3.7 ms both — because
the shard count, not the core count, is the ceiling. Going from serial to eight
is worth 7.7 ms to 3.7; going from eight to twenty is worth nothing.

So on a machine with many cores the default puts threads on a query that cannot
use them. Setting `--threads 8` there answers in the same time with fewer
threads occupied, which leaves them for other queries. Set it lower than eight
and latency rises toward the serial figure, which is the cost of the trade and
is also the right choice for a server answering many concurrent queries rather
than one at a time: threads spent widening a single scan are threads not
answering someone else.

### Shards also decide the shape of the tail under ingest

A checkpoint stalls queries that are in flight (see below). How *much* total
stall a checkpoint causes does not depend on the shard count — measured across a
thirty-two-fold range it is flat. What the shard count changes is how that total
is distributed, and the two ends are quite different to live with:

| Shards | Queries affected | How badly |
|---|---|---|
| 1 | few | severely — p99 15 ms, worst 543 ms |
| 32 | more | mildly — p99 105 ms, worst 135 ms |

Both rows carry about the same total delay; one concentrates it and the other
spreads it. If you are serving a latency objective stated as a percentile, more
shards is the wrong choice and fewer is right — the opposite of what the table
above recommends for throughput. If you care about the worst request anyone
sees, more shards is right.

This is a real trade and it has no default answer. It is stated here rather than
chosen for you because the two goals genuinely conflict, and because the shard
count cannot be changed after an index is created.

## Statistics

Per-block statistics tighten the bound the ratio metrics prune with. Rebuild
them after a bulk ingest:

```console
haiiied --data-dir ./data --refresh-stats
```

They are **invalidated by a code write or live-document delete** in the block
they describe, in the same atomic commit. Deleting an absent ID or changing
only attributes leaves them intact. The server does not refresh statistics
while it runs; restart with `--refresh-stats` after bulk ingest or when
`explain` shows too few blocks carrying them. A block without statistics is
scored with a looser bound — slower, never wrong. That asymmetry
is deliberate: a stale bound would drop results silently, with no error and no
change in the result count.

`explain` reports how many blocks currently carry statistics, which is the way
to tell whether a rebuild is due.

## Snapshots and long queries

A query reads through a consistent snapshot. A snapshot that lives long enough
can be evicted by the storage engine to bound space amplification, and a scan
handles that in one of two ways:

* **Resume** (the default) — take a fresh snapshot and redo the block that
  failed. The result then spans versions: a document deleted between them may
  still appear, and one inserted may be missed. The response reports the version
  range and how many resumes happened, so this is visible rather than silent.
* **Strict** — one snapshot for the whole scan; an eviction is returned as an
  error and the caller decides.

A resumed result is exact for each block against the version that block was read
at, which is the most a resumed scan can promise.

## Deletes and compaction

A delete removes the document and retires its id until an explicit compaction.
The embedded `compact(directory, namespace)` API rewrites one entire namespace,
packing live documents into ids `0..N` in descending code-weight order, with old id
ascending for ties. It rebuilds codes, liveness and attributes atomically,
plus postings and statistics for a binary index. Residual indexes keep their
forward-only layout after compaction. Deleted documents' leftover attributes
are removed. Other namespaces are unchanged. No ingest operation reorders ids.

This is offline maintenance: stop the server and close every database handle first.
The database's exclusive directory lock refuses compaction while an ordinary
`YesnoStore` handle remains. If the application wraps upstream foreign read-only
handles, stop those explicitly too: they bypass the writer lock.
There is currently no CLI or gRPC compaction command: the `haiiie` CLI is a
remote query client, and `haiiied` has no maintenance mode. Use the embedded
API from a short-lived maintenance program. The example below keeps both calls
in one process for clarity; that releases retired IDs but does not return
compaction's reusable slab space to the filesystem.

```rust,no_run
let mapping = haiiie_core::compact("my-index", 0)?;
// mapping.old_ids[new_id] gives that document's previous ID.
// Durably migrate application objects, saved ID filters and optional float rows.
// Only after that migration is complete:
haiiie_core::acknowledge_compaction("my-index", 0, mapping.source_version)?;
# Ok::<(), haiiie_core::Error>(())
```

The mapping is committed with the rewritten data and remains recoverable until
acknowledged. Opening that namespace is refused in the meantime. If the process
stops or an I/O error occurs, call `compact` again: a pending operation returns
the same mapping rather than compacting again. Make external migration resumable,
or build replacement objects and float files separately before switching them into
use. Copy the mapping before acknowledging if historical ids are still needed.
Acknowledgement checks the source version and deletes the recovery mapping; it
asserts that the application has completed its part, it cannot inspect that work.

Scores stay exact, but tied documents can change order because ties use the new ids.
After acknowledgement, ids at or above `N` may be assigned again without inheriting
old codes or attributes. Do not reuse ids retired by subsequent deletes until the
next compaction. Future inserts and updates can widen the block weight ranges again.

Compaction holds all live codes and one atomic rewrite batch in memory, so
budget memory and downtime for the entire namespace. It checkpoints after the
rewrite. Returning allocated disk space also depends on the **process boundary**:
with yesnodb at commit `03cd5a3` or a descendant, run `compact` in a process
that has never opened the directory, let that process exit, migrate the caller's
references durably, then run `acknowledge_compaction` in another fresh process.
Stop the server and close its handles before each step. Calling these APIs from
an embedder or a `haiiied` process that opened the directory earlier makes
freed space reusable by yesnodb, but does not return it to the filesystem.
Idle checkpoints do not punch that space; the next dirty checkpoint does.

In a measured 262,144-document D=256 delete/refill/compact cycle, with each
phase in its own process, allocated space settled at 26.82 MB after the next
checkpoint, or 1.47 times a fresh 18.24 MB store. The same cycle within one
process settled at 58.77 MB. Compaction briefly used 55.76-60.05 MB in the
fresh-process run, so budget for that peak. These figures measure allocated
bytes, not apparent sparse-file length, and do not promise the same reduction
on a different corpus or directory. A separate maintenance invocation can use
the embedded API today; there is no built-in compaction CLI command.

## Disk footprint and backup

Storage shard files are sparse. An empty 32-shard index measured 34.4 GB
apparent size but only 0.30 MB allocated; an 8-shard index measured 8.6 GB
apparent. Use a sparse-aware backup or copy method and verify allocated space
after restore. In a 262,144-document D=256 point-ingest run, sampled allocated
disk peaked at 153.74 MB before checkpoint versus 18.24 MB afterward ( 8.4
times ). Provision temporary headroom for bulk ingestion; this measurement did
not cover packed residual ingest.


haiiie stores the index and its header in the storage engine. Backing up the
data directory by the engine's documented procedure captures every per-document
value. The residual model is shared configuration rather than document state;
back it up with the service configuration and verify its content identity when
restoring the index.

## What this does not do

* **No authentication, no TLS, no authorization.** Do not expose the server to
  anything you do not trust. There is no plan recorded for adding it.
* **No clustering, replication or sharding.** One process, one directory. The
  design exists and is deliberately unbuilt.
* **No automatic statistics maintenance.** See above.
* **No online or automatic compaction.** The embedded offline operation above is explicit.
* **No rate limiting, quotas or query timeouts.** A sufficiently large `k` or a
  sufficiently unselective filter will run until it finishes.
