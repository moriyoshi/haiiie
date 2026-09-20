# Third-party notices

haiiie depends on the following, each under its own license. This file records
what is linked and why; it is not a substitute for the licenses themselves,
which travel with the crates.

## Runtime

| Dependency | Used by | Why |
|---|---|---|
| `yesno-core` | `haiiie-core` | The storage engine. Persistent `u64 key -> set of u64 ordinals`, Roaring-style containers, MVCC snapshots, write-ahead log. haiiie stores every byte of its index in it. |
| `thiserror` | `haiiie-core` | Error derivation. |
| `memmap2` | `haiiie-embed` | The float side-store is read by random access and mapped rather than buffered. |
| `prost`, `tonic` | `haiiie-proto`, `haiiie-grpc` | Protobuf and gRPC. |
| `tokio`, `tokio-stream` | `haiiie-grpc`, `haiiie-cli` | The server's async runtime. |
| `clap` | `haiiie-cli` | Argument parsing. |

`haiiie-core` has a **two-dependency budget**, enforced by the build gate. Arrow
types reach it through a re-export from `yesno-core` rather than a direct
dependency, so an Arrow major version bump is not a breaking change here.

## Build

| Dependency | Why |
|---|---|
| `tonic-build` | Generates the protobuf types at build time, so the `.proto` stays the single source of truth. |
| `protoc-bin-vendored` | Supplies the protobuf compiler, so a checkout builds with nothing installed on the host. |

## Test only

`proptest`, `criterion`, `tempfile`. None is linked into a shipped binary.
