//! The socket boundary is exercised with the same host Session yesnod serves.
//! A live embedded handle supplies the oracle; neither path opens a second Db.

use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

use haiiie_core::slice::zero_mask;
use haiiie_core::store::{SetSnapshot, SetStore};
use haiiie_core::{CodeRef, DocId, Index, Metric, PathHint, YesnoStore};
use haiiie_peer::PeerStore;
use proptest::prelude::*;
use yesno_core::{ChunkStream, Db};
use yesno_plugin::abi::Role;
use yesno_plugin::channel::{Arena, Limits, Session, send_fd, serve_blocking};
use yesno_plugin::ipc::Frame;
use yesno_plugin::{DbSlot, Host};

struct Server {
    socket: std::path::PathBuf,
    stop: Arc<AtomicBool>,
    notify_sockets: Arc<Mutex<Vec<UnixStream>>>,
    join: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn start(dir: &tempfile::TempDir, db: Db, inline: bool) -> Self {
        let socket = dir
            .path()
            .join(if inline { "inline.sock" } else { "arena.sock" });
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let notify_sockets = Arc::new(Mutex::new(Vec::new()));
        let writers = notify_sockets.clone();
        let slot: DbSlot = Arc::new(RwLock::new(Some(Arc::new(db))));
        let host = Host::new(slot, 1, Role::Leader);
        let join = thread::spawn(move || {
            let mut peers = Vec::new();
            while !stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        writers.lock().unwrap().push(stream.try_clone().unwrap());
                        let host = host.clone();
                        peers.push(thread::spawn(move || {
                            let limits = Limits {
                                max_handles: 4,
                                max_lanes: 1024,
                                max_blocks: 16,
                                ..Limits::default()
                            };
                            let mut session = if inline {
                                Session::new_inline(host, limits)
                            } else {
                                let arena = Arena::new(limits.arena_bytes()).unwrap();
                                send_fd(&stream, arena.as_fd()).unwrap();
                                Session::new(host, arena, limits)
                            };
                            if let Err(error) = serve_blocking(&mut session, stream) {
                                // An invalidation may make the peer close before
                                // reading the reply to its already-sent request.
                                assert!(
                                    matches!(
                                        error.kind(),
                                        std::io::ErrorKind::ConnectionReset
                                            | std::io::ErrorKind::BrokenPipe
                                    ),
                                    "unexpected serving error: {error}"
                                );
                            }
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            }
            for peer in peers {
                peer.join().unwrap();
            }
        });
        Self {
            socket,
            stop,
            notify_sockets,
            join: Some(join),
        }
    }

    fn disconnect(&self) {
        for socket in self.notify_sockets.lock().unwrap().iter() {
            socket.shutdown(Shutdown::Both).unwrap();
        }
    }

    fn notify(&self, frame: Frame) {
        let encoded = frame.encode().unwrap();
        for socket in self.notify_sockets.lock().unwrap().iter_mut() {
            socket.write_all(&encoded).unwrap();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.join.take().unwrap().join().unwrap();
    }
}

fn fill(db: &Db) {
    let mut batch = db.batch();
    for ordinal in [1, 7, 100, 5000, 65_535, 65_536 + 9] {
        batch.insert(10, ordinal);
    }
    for ordinal in 1000..6000 {
        batch.insert(20, ordinal);
    }
    for ordinal in (0..15_000).step_by(3) {
        batch.insert(30, ordinal);
    }
    batch.commit().unwrap();
}

#[test]
fn socket_snapshot_matches_embedded_for_all_container_shapes() {
    for inline in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("db")).unwrap();
        fill(&db);
        let server = Server::start(&dir, db.clone(), inline);
        let peer = PeerStore::new(&server.socket);
        let remote = peer.snapshot().unwrap();
        let local = db.snapshot().unwrap();
        assert_eq!(remote.version(), local.version());
        assert_eq!(remote.key_range(1, 40).unwrap(), vec![10, 20, 30]);
        assert_eq!(remote.key_range(30, 31).unwrap(), vec![30]);
        for key in [10, 20, 30, 40] {
            assert_eq!(
                remote.cardinality(key).unwrap(),
                local.cardinality(key).unwrap()
            );
            assert_eq!(remote.max(key).unwrap(), local.max(key).unwrap());
            assert_eq!(
                remote.load(key).unwrap(),
                local.load(key).unwrap().iter().collect::<Vec<_>>()
            );
            for ordinal in [0, 7, 3000, 65_536 + 9] {
                assert_eq!(
                    remote.contains(key, ordinal).unwrap(),
                    local.contains(key, ordinal).unwrap()
                );
            }
            for block in [0, 1, 2] {
                let mut actual = zero_mask();
                let mut expected = zero_mask();
                remote.load_block(key, block, &mut actual).unwrap();
                let base = block * 65_536;
                for ordinal in local.load(key).unwrap().iter() {
                    if ordinal >= base && ordinal < base + 65_536 {
                        let bit = (ordinal - base) as usize;
                        expected[bit / 64] |= 1u64 << (bit % 64);
                    }
                }
                assert_eq!(
                    actual, expected,
                    "key {key}, block {block}, inline {inline}"
                );
            }
        }
        let mut lanes = remote.open_lanes(&[10, 20, 30, 40]).unwrap().unwrap();
        for block in [0, 1, 0, 2] {
            for (lane, key) in [10, 20, 30, 40].into_iter().enumerate() {
                let mut actual = zero_mask();
                let mut expected = zero_mask();
                lanes.read(lane, block, &mut actual).unwrap();
                remote.load_block(key, block, &mut expected).unwrap();
                assert_eq!(
                    actual, expected,
                    "lane {lane}, block {block}, inline {inline}"
                );
            }
        }
        drop(lanes);
        drop(remote);
        drop(server);
    }
}

#[test]
fn persisted_nonzero_start_run_lanes_match_their_ordinals() {
    // Both old failure shapes matter: the first decoded as a shorter but
    // well-formed interval, while the second decoded as a reversed interval.
    for inline in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("db")).unwrap();
        let mut batch = db.batch();
        batch.insert_range(11, 1000, 6000);
        batch.insert_range(12, 60_000, 65_535);
        batch.commit().unwrap();
        db.checkpoint().unwrap();

        let local = db.snapshot().unwrap();
        for key in [11, 12] {
            let (_, chunk) = local
                .key_stream(key)
                .unwrap()
                .next_chunk()
                .unwrap()
                .expect("persisted chunk");
            assert_eq!(chunk.kind(), yesno_core::ContainerKind::Run);
        }

        let server = Server::start(&dir, db, inline);
        let remote = PeerStore::new(&server.socket).snapshot().unwrap();
        let mut lanes = remote.open_lanes(&[11, 12]).unwrap().unwrap();
        for (lane, key, lo, hi) in [(0, 11, 1000usize, 6000usize), (1, 12, 60_000, 65_535)] {
            let mut expected = zero_mask();
            for ordinal in lo..=hi {
                expected[ordinal / 64] |= 1u64 << (ordinal % 64);
            }
            let mut actual = zero_mask();
            lanes.read(lane, 0, &mut actual).unwrap();
            assert_eq!(actual, expected, "key {key}, inline {inline}");

            let mut loaded = zero_mask();
            remote.load_block(key, 0, &mut loaded).unwrap();
            assert_eq!(
                actual, loaded,
                "lane versus load_block, key {key}, inline {inline}"
            );
        }
        drop(lanes);
        drop(remote);
        drop(server);
    }
}

#[test]
fn lifecycle_and_eviction_notifications_invalidate_the_pinned_snapshot() {
    for frame in [
        Frame::Unavailable,
        Frame::GenerationChanged { old: 1, new: 2 },
        Frame::Available { generation: 2 },
        Frame::Fault {
            status: yesno_plugin::abi::Status::SnapshotTooOld as u32,
            message: "evicted".into(),
        },
    ] {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("db")).unwrap();
        fill(&db);
        let server = Server::start(&dir, db, false);
        let peer = PeerStore::new(&server.socket);
        let snapshot = peer.snapshot().unwrap();
        let old_version = snapshot.version();
        server.notify(frame);
        let error = snapshot.cardinality(10).unwrap_err();
        assert!(
            matches!(error, haiiie_core::Error::SnapshotExpired { version } if version == old_version)
        );
        // A new channel can acquire a fresh snapshot after the old one is
        // invalidated; the stale handle never silently substitutes it.
        assert_eq!(peer.snapshot().unwrap().cardinality(10).unwrap(), 6);
        drop(snapshot);
        drop(server);
    }
}

#[test]
fn a_forced_channel_close_expires_only_the_old_snapshot() {
    // yesnod disconnects peers at follower cutover even when they did not
    // consume an Unavailable notification. A fresh connection can serve a new
    // snapshot, but the old one must fail as an eviction, not a generic I/O error.
    for inline in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("db")).unwrap();
        fill(&db);
        let server = Server::start(&dir, db, inline);
        let peer = PeerStore::new(&server.socket);
        let old = peer.snapshot().unwrap();
        let version = old.version();

        server.disconnect();
        assert!(matches!(
            old.cardinality(10),
            Err(haiiie_core::Error::SnapshotExpired { version: v }) if v == version
        ));
        assert_eq!(peer.snapshot().unwrap().cardinality(10).unwrap(), 6);
        drop(old);
        drop(server);
    }
}

#[test]
fn exact_query_results_match_embedded_over_both_socket_transports() {
    for inline in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("db")).unwrap();
        let embedded = Index::create(YesnoStore::from_db(db.clone()), 7, 64).unwrap();
        let mut writer = embedded.writer();
        for id in 0..96u64 {
            let code = [id.wrapping_mul(0x9e37_79b9_7f4a_7c15)];
            writer.put(DocId(id), CodeRef::Dense(&code)).unwrap();
        }
        writer.commit().unwrap();
        let server = Server::start(&dir, db.clone(), inline);
        let peer = Index::open(PeerStore::new(&server.socket), 7).unwrap();
        let query = [0x0f0f_aa55_f0f0_1234];
        for metric in [
            Metric::Dot,
            Metric::Hamming,
            Metric::Jaccard,
            Metric::Cosine,
        ] {
            for path in [PathHint::Gather, PathHint::DenseScan, PathHint::Inverted] {
                let expected = embedded
                    .search()
                    .code(CodeRef::Dense(&query))
                    .metric(metric)
                    .k(10)
                    .path(path)
                    .execute()
                    .unwrap();
                let actual = peer
                    .search()
                    .code(CodeRef::Dense(&query))
                    .metric(metric)
                    .k(10)
                    .path(path)
                    .execute()
                    .unwrap();
                assert_eq!(
                    actual.hits, expected.hits,
                    "{metric:?} {path:?} inline={inline}"
                );
            }
        }
        drop(peer);
        drop(server);
    }
}

#[test]
fn partial_lane_acquisition_releases_earlier_handles_without_deadlocking() {
    use std::io::Read;
    use std::sync::mpsc;

    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("db")).unwrap();
    let socket = dir.path().join("partial.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let releases = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let released = releases.clone();
    let server = thread::spawn(move || {
        let slot: DbSlot = Arc::new(RwLock::new(Some(Arc::new(db))));
        let host = Host::new(slot, 1, Role::Leader);
        let mut session = Session::new_inline(
            host,
            Limits {
                max_handles: 4,
                max_lanes: 1024,
                max_blocks: 16,
                ..Limits::default()
            },
        );
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .write_all(&session.hello().encode().unwrap())
            .unwrap();
        let mut pending = Vec::new();
        let mut acquisitions = 0;
        loop {
            let frame = match Frame::decode(&pending) {
                Ok((frame, used)) => {
                    pending.drain(..used);
                    frame
                }
                Err(yesno_plugin::ipc::IpcError::Truncated) => {
                    let mut chunk = [0u8; 8192];
                    let n = stream.read(&mut chunk).unwrap();
                    if n == 0 {
                        break;
                    }
                    pending.extend_from_slice(&chunk[..n]);
                    continue;
                }
                Err(error) => panic!("invalid peer request: {error}"),
            };
            let reply = match &frame {
                Frame::LanesAcquire { .. } => {
                    acquisitions += 1;
                    if acquisitions == 2 {
                        Frame::Fault {
                            status: 1,
                            message: "injected second acquire failure".into(),
                        }
                    } else {
                        session.handle(frame)
                    }
                }
                Frame::LanesRelease { .. } => {
                    released.fetch_add(1, Ordering::SeqCst);
                    session.handle(frame)
                }
                _ => session.handle(frame),
            };
            stream.write_all(&reply.encode().unwrap()).unwrap();
        }
    });
    let snapshot = PeerStore::new(&socket).snapshot().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let answer = snapshot
            .open_lanes(&vec![55; 265])
            .map(|lanes| lanes.is_some());
        tx.send(answer).unwrap();
    });
    let result = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("partial acquisition deadlocked");
    assert!(result.is_err());
    server.join().unwrap();
    assert_eq!(releases.load(Ordering::SeqCst), 1);
}

#[test]
fn wide_lane_sets_use_advertised_limits_on_one_snapshot() {
    for inline in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("db")).unwrap();
        let mut batch = db.batch();
        // A bitmap lane is the widest stored kind. 265 such lanes overflowed
        // one inline frame before the host began advertising its true limit.
        for bit in 0..5_000 {
            batch.insert(55, bit);
        }
        batch.commit().unwrap();
        let server = Server::start(&dir, db, inline);
        let peer = PeerStore::new(&server.socket);
        let snapshot = peer.snapshot().unwrap();
        let keys = vec![55; 265];
        let mut lanes = snapshot.open_lanes(&keys).unwrap().unwrap();
        if inline {
            // Three split handles are live under the four-handle greeting;
            // a second wide cursor must take the exact addressed fallback.
            assert!(snapshot.open_lanes(&keys).unwrap().is_none());
        }
        for lane in [0, 127, 128, 255, 256, 264] {
            let mut mask = zero_mask();
            assert!(lanes.read(lane, 0, &mut mask).unwrap());
            assert_eq!(
                mask.iter().map(|word| word.count_ones()).sum::<u32>(),
                5_000
            );
        }
        drop(lanes);
        drop(snapshot);
        drop(server);
    }
}

#[test]
fn wide_binary_query_is_exact_across_split_handles() {
    for inline in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("db")).unwrap();
        let embedded = Index::create(YesnoStore::from_db(db.clone()), 7, 256).unwrap();
        let mut writer = embedded.writer();
        for id in 0..32u64 {
            writer
                .put(DocId(id), CodeRef::Dense(&[id, !id, id * 3, !(id * 5)]))
                .unwrap();
        }
        writer.commit().unwrap();
        let server = Server::start(&dir, db, inline);
        let peer = Index::open(PeerStore::new(&server.socket), 7).unwrap();
        let query = [u64::MAX; 4];
        let expected = embedded
            .search()
            .code(CodeRef::Dense(&query))
            .k(10)
            .path(PathHint::Inverted)
            .execute()
            .unwrap();
        let actual = peer
            .search()
            .code(CodeRef::Dense(&query))
            .k(10)
            .path(PathHint::Inverted)
            .execute()
            .unwrap();
        assert_eq!(actual.hits, expected.hits, "inline={inline}");
        drop(peer);
        drop(server);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    fn arena_mapping_round_trips_random_memberships(bits in proptest::collection::vec(0u16..65535, 0..400)) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("db")).unwrap();
        let mut batch = db.batch();
        for &bit in &bits { batch.insert(99, bit as u64); }
        batch.commit().unwrap();
        let server = Server::start(&dir, db, false);
        let peer = PeerStore::new(&server.socket);
        let snapshot = peer.snapshot().unwrap();
        let mut lanes = snapshot.open_lanes(&[99]).unwrap().unwrap();
        let mut actual = zero_mask();
        lanes.read(0, 0, &mut actual).unwrap();
        let mut expected = zero_mask();
        for bit in bits { expected[bit as usize / 64] |= 1u64 << (bit % 64); }
        prop_assert_eq!(actual, expected);
        drop(lanes);
        drop(snapshot);
        drop(server);
    }
}

#[test]
fn flight_writer_commits_one_batch_visible_to_the_channel() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("db")).unwrap();
    let server = Server::start(&dir, db.clone(), false);
    let address = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let service = yesno_flight::YesnoFlightService::new(Arc::new(db.clone()));
    let flight = runtime.spawn(async move {
        tonic::transport::Server::builder()
            .add_service(arrow_flight::flight_service_server::FlightServiceServer::new(service))
            .serve(address)
            .await
            .unwrap();
    });
    for _ in 0..100 {
        if std::net::TcpStream::connect(address).is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let token_file = dir.path().join("flight.token");
    std::fs::write(&token_file, "first-token\n").unwrap();
    let peer = PeerStore::new(&server.socket)
        .with_flight_auth(format!("http://{address}"), Some(token_file.clone()))
        .unwrap();
    let index = Index::create(peer, 7, 64).unwrap();
    let mut writer = index.writer();
    for (id, code) in [(2, 0x1245u64), (5, 0x90abu64), (9, 0xff00u64)] {
        writer.put(DocId(id), CodeRef::Dense(&[code])).unwrap();
    }
    let version = writer.commit().unwrap();
    assert!(version > 0);
    std::fs::remove_file(&token_file).unwrap();
    let mut missing_token = index.writer();
    missing_token
        .put(DocId(12), CodeRef::Dense(&[0x1234]))
        .unwrap();
    assert!(missing_token.commit().is_err());
    std::fs::write(&token_file, "rotated-token\n").unwrap();
    let mut after_rotation = index.writer();
    after_rotation
        .put(DocId(12), CodeRef::Dense(&[0x1234]))
        .unwrap();
    after_rotation.commit().unwrap();
    let embedded = Index::open(YesnoStore::from_db(db.clone()), 7).unwrap();
    let query = [0x12abu64];
    let actual = index
        .search()
        .code(CodeRef::Dense(&query))
        .k(3)
        .execute()
        .unwrap();
    let expected = embedded
        .search()
        .code(CodeRef::Dense(&query))
        .k(3)
        .execute()
        .unwrap();
    assert_eq!(actual.hits, expected.hits);
    drop(embedded);
    drop(index);
    drop(server);
    flight.abort();
    drop(runtime);
}

#[test]
fn peer_declines_packed_residual_tile_before_building_a_patch() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("db")).unwrap();
    Index::create_with_model_id(YesnoStore::from_db(db.clone()), 9, 512, [1; 32]).unwrap();
    let server = Server::start(&dir, db, false);
    let peer = Index::open(PeerStore::new(&server.socket), 9).unwrap();
    let rows = (0..128)
        .map(|id| (DocId(id), [0u64; 8]))
        .collect::<Vec<_>>();
    assert!(!peer.writer().try_put_residual_tile(&rows).unwrap());
    drop(peer);
    drop(server);
}
