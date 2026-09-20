//! Read haiiie indexes through yesnod's served snapshot and lane channel.
//!
//! The peer never opens the database directory. One channel connection owns one
//! snapshot and all of its lane handles, so every read in a query sees one version.
//! The server may send lifecycle notifications between any two replies or
//! close a pinned channel at follower cutover. Both expire the old snapshot;
//! a fresh snapshot opens a fresh connection. The channel is read-only. Write
//! support is deliberately separate from this transport.

use std::io::{self, IoSliceMut, Read, Write};
use std::mem::MaybeUninit;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use haiiie_core::slice::BlockMask;
use haiiie_core::store::{Batch, Lanes, Op, SetSnapshot, SetStore, Version};
use yesno_flight::client::Mutation;

mod control;
mod write;
use control::ControlWorker;
use haiiie_core::{Error, Result};
use memmap2::Mmap;
use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, recvmsg};
use write::WriteWorker;
use yesno_plugin::ipc::{self, Block, Frame, Lane, LaneKind};

fn invalid(message: impl Into<String>) -> Error {
    Error::Io(io::Error::new(io::ErrorKind::InvalidData, message.into()))
}

fn unavailable(message: impl Into<String>) -> Error {
    Error::Io(io::Error::new(io::ErrorKind::NotConnected, message.into()))
}

/// A store over yesnod's Unix socket, with optional Flight writes.
///
/// A new connection is opened per snapshot, because handles are scoped to a
/// connection and a parallel query must share one version rather than reopen
/// separate, potentially different snapshots for its workers.
#[derive(Clone, Debug)]
pub struct PeerStore {
    socket: PathBuf,
    writer: Option<Arc<WriteWorker>>,
    control: Option<Arc<ControlWorker>>,
    last_write: Arc<AtomicU64>,
}

impl PeerStore {
    /// The socket configured as `plugin.channel_socket` in yesnod.
    pub fn new(socket: impl AsRef<Path>) -> Self {
        Self {
            socket: socket.as_ref().to_path_buf(),
            writer: None,
            control: None,
            last_write: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Attach yesnod's Flight endpoint for ordered atomic batches. The lane
    /// channel itself remains read-only.
    pub fn with_flight(self, endpoint: impl Into<String>) -> Result<Self> {
        self.with_flight_auth(endpoint, None)
    }

    /// Attach a Flight writer with an optional bearer-token file. The token is
    /// read before every batch so rotation needs no peer restart.
    pub fn with_flight_auth(
        mut self,
        endpoint: impl Into<String>,
        token_file: Option<PathBuf>,
    ) -> Result<Self> {
        self.writer = Some(Arc::new(WriteWorker::start(endpoint.into(), token_file)?));
        Ok(self)
    }

    /// Attach yesnod's authorized control endpoint for durable checkpoints.
    /// The optional bearer-token file is read at each checkpoint, allowing
    /// token rotation without restarting the peer.
    pub fn with_control(
        mut self,
        endpoint: impl Into<String>,
        token_file: Option<PathBuf>,
    ) -> Result<Self> {
        self.control = Some(Arc::new(ControlWorker::start(endpoint.into(), token_file)?));
        Ok(self)
    }
}

struct Connection {
    socket: UnixStream,
    buffer: Vec<u8>,
    arena: Option<Arc<Mmap>>,
    generation: u64,
    max_lanes: usize,
    max_handles: usize,
    max_blocks: usize,
    active_handles: usize,
    stale: bool,
}

impl Connection {
    fn connect(path: &Path) -> Result<Self> {
        let socket = UnixStream::connect(path)?;
        // The host sends one version byte with SCM_RIGHTS *before* ServerHello
        // in arena mode. Inline mode starts directly with the frame. recvmsg is
        // required for the first read so the descriptor cannot be discarded by
        // an ordinary read; the safe rustix wrapper owns received descriptors.
        let mut bytes = [0u8; 8192];
        let mut iov = [IoSliceMut::new(&mut bytes)];
        let mut control_space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = RecvAncillaryBuffer::new(&mut control_space);
        let received = recvmsg(&socket, &mut iov, &mut control, RecvFlags::empty())
            .map_err(|error| Error::Io(io::Error::from_raw_os_error(error.raw_os_error())))?;
        if received.bytes == 0 {
            return Err(unavailable("yesnod closed before its channel greeting"));
        }
        let mut fd = None;
        for item in control.drain() {
            if let RecvAncillaryMessage::ScmRights(mut rights) = item
                && let Some(first) = rights.next()
                && (fd.replace(first).is_some() || rights.next().is_some())
            {
                return Err(invalid("yesnod sent more than one arena descriptor"));
            }
        }
        let skip = if fd.is_some() {
            if bytes[0] != ipc::VERSION {
                return Err(invalid("yesnod arena version byte differs from the codec"));
            }
            1
        } else {
            0
        };
        let mut connection = Self {
            socket,
            buffer: bytes[skip..received.bytes].to_vec(),
            arena: None,
            generation: 0,
            max_lanes: 0,
            max_handles: 0,
            max_blocks: 0,
            active_handles: 0,
            stale: false,
        };
        let greeting = connection.next_frame()?;
        let Frame::ServerHello {
            protocol,
            generation,
            arena_bytes,
            max_lanes,
            max_handles,
            max_blocks,
            ..
        } = greeting
        else {
            return Err(invalid("yesnod did not send ServerHello first"));
        };
        if protocol != u32::from(ipc::VERSION) {
            return Err(invalid(format!(
                "unsupported yesnod channel version {protocol}"
            )));
        }
        if max_lanes == 0 || max_handles == 0 || max_blocks == 0 {
            return Err(invalid("yesnod advertised a zero channel limit"));
        }
        if arena_bytes == 0 {
            if fd.is_some() {
                return Err(invalid(
                    "yesnod sent an arena fd but advertised inline mode",
                ));
            }
        } else {
            let fd = fd.ok_or_else(|| invalid("yesnod advertised an arena without an fd"))?;
            let file = std::fs::File::from(fd);
            if file.metadata()?.len() < arena_bytes {
                return Err(invalid(
                    "yesnod arena fd is shorter than its advertised size",
                ));
            }
            let len = usize::try_from(arena_bytes)
                .map_err(|_| invalid("yesnod arena exceeds this process's address space"))?;
            // SAFETY: the fd is live, sized to at least `len` above, and yesnod
            // seals it against shrink before transfer. The mapping is read-only;
            // its slots are consumed before this client advances that handle.
            let map = unsafe { memmap2::MmapOptions::new().len(len).map(&file)? };
            connection.arena = Some(Arc::new(map));
        }
        connection.generation = generation;
        connection.max_lanes = max_lanes as usize;
        connection.max_handles = max_handles as usize;
        connection.max_blocks = max_blocks as usize;
        tracing::debug!(
            generation,
            arena = connection.arena.is_some(),
            max_lanes,
            max_handles,
            max_blocks,
            "connected to yesnod lane channel"
        );
        match connection.request(
            Frame::ClientHello {
                protocol,
                name: "haiiie".into(),
            },
            0,
        )? {
            Frame::Done => Ok(connection),
            _ => Err(invalid("yesnod refused the channel hello")),
        }
    }

    fn next_frame(&mut self) -> Result<Frame> {
        loop {
            match Frame::decode(&self.buffer) {
                Ok((frame, used)) => {
                    self.buffer.drain(..used);
                    return Ok(frame);
                }
                Err(ipc::IpcError::Truncated) => {}
                Err(error) => return Err(invalid(format!("invalid yesnod frame: {error}"))),
            }
            let mut chunk = [0u8; 8192];
            let n = self.socket.read(&mut chunk)?;
            if n == 0 {
                return Err(unavailable("yesnod closed its channel"));
            }
            self.buffer.extend_from_slice(&chunk[..n]);
        }
    }

    fn request(&mut self, frame: Frame, version: Version) -> Result<Frame> {
        if self.stale {
            return Err(Error::SnapshotExpired { version });
        }
        let bytes = frame
            .encode()
            .map_err(|error| invalid(format!("cannot encode yesnod request: {error}")))?;
        self.socket.write_all(&bytes)?;
        loop {
            let reply = self.next_frame()?;
            match reply {
                Frame::Unavailable | Frame::GenerationChanged { .. } => {
                    tracing::warn!(version, "yesnod snapshot became unavailable");
                    self.stale = true;
                    return Err(Error::SnapshotExpired { version });
                }
                Frame::Available { generation } => {
                    if generation != self.generation {
                        self.stale = true;
                        return Err(Error::SnapshotExpired { version });
                    }
                }
                Frame::RoleChanged { .. } => {}
                Frame::Fault { status, message } => {
                    tracing::warn!(status, message, "yesnod channel fault");
                    if status == yesno_plugin::abi::Status::SnapshotTooOld as u32
                        || status == yesno_plugin::abi::Status::GenerationChanged as u32
                    {
                        self.stale = true;
                        return Err(Error::SnapshotExpired { version });
                    }
                    return Err(invalid(format!("yesnod channel fault {status}: {message}")));
                }
                other => return Ok(other),
            }
        }
    }
}

struct Shared {
    connection: Mutex<Connection>,
    snapshot: u64,
    version: Version,
}

impl Shared {
    fn request(&self, frame: Frame) -> Result<Frame> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| unavailable("peer connection lock was poisoned"))?;
        match connection.request(frame, self.version) {
            Err(Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotConnected
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::BrokenPipe
                        | io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::WriteZero
                ) =>
            {
                // yesnod disconnects every old session before a follower
                // cutover. It may close between a request and its reply, so no
                // notification need arrive. This snapshot cannot be read again;
                // the searcher may take a fresh one under its bounded retry rule.
                connection.stale = true;
                tracing::warn!(version = self.version, %error, "yesnod closed a pinned snapshot channel");
                Err(Error::SnapshotExpired {
                    version: self.version,
                })
            }
            result => result,
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        if let Ok(mut conn) = self.connection.lock() {
            let _ = conn.request(
                Frame::SnapshotClose {
                    snapshot: self.snapshot,
                },
                self.version,
            );
        }
    }
}

/// One version pinned by yesnod for the life of this channel connection.
#[derive(Clone)]
pub struct PeerSnapshot {
    shared: Arc<Shared>,
}

impl SetStore for PeerStore {
    type Snap = PeerSnapshot;

    fn snapshot(&self) -> Result<Self::Snap> {
        let mut connection = Connection::connect(&self.socket)?;
        let Frame::SnapshotOpened { snapshot, version } =
            connection.request(Frame::SnapshotOpen, 0)?
        else {
            return Err(invalid("yesnod did not open the requested snapshot"));
        };
        tracing::debug!(snapshot, version, "opened served snapshot");
        Ok(PeerSnapshot {
            shared: Arc::new(Shared {
                connection: Mutex::new(connection),
                snapshot,
                version,
            }),
        })
    }

    fn write(&self, batch: &Batch) -> Result<Version> {
        let Some(writer) = &self.writer else {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::Unsupported,
                "the yesnod lane channel is read-only; configure a Flight writer",
            )));
        };
        let mut mutations = Vec::with_capacity(batch.len());
        for op in batch.ops() {
            mutations.push(match op {
                Op::Insert(key, ordinal) => Mutation::Insert {
                    key: *key,
                    ordinal: *ordinal,
                },
                Op::Remove(key, ordinal) => Mutation::Remove {
                    key: *key,
                    ordinal: *ordinal,
                },
                Op::RemoveRange(key, lo, hi) => Mutation::RemoveRange {
                    key: *key,
                    lo: *lo,
                    hi: *hi,
                },
                Op::DeleteKey(key) => Mutation::DeleteKey { key: *key },
                Op::PatchChunk(..) => {
                    return Err(Error::Io(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "Flight PUT_APPLY cannot encode a packed chunk patch",
                    )));
                }
            });
        }
        let version = writer.apply(mutations)?;
        self.last_write.fetch_max(version, Ordering::AcqRel);
        tracing::debug!(version, operations = batch.len(), "Flight batch committed");
        Ok(version)
    }

    fn flush(&self) -> Result<()> {
        let target = self.last_write.load(Ordering::Acquire);
        let watermark = self
            .control
            .as_ref()
            .ok_or_else(|| {
                Error::Io(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "yesnod checkpoint control is not configured for this peer",
                ))
            })?
            .checkpoint()?;
        if watermark < target {
            return Err(Error::Io(io::Error::other(format!(
                "yesnod checkpoint watermark {watermark} is behind this peer's last Flight commit {target}"
            ))));
        }
        Ok(())
    }
}

impl PeerSnapshot {
    fn request(&self, frame: Frame) -> Result<Frame> {
        self.shared.request(frame)
    }

    fn load_page(&self, key: u64, after: Option<u64>) -> Result<(Vec<u64>, bool)> {
        let frame = Frame::SnapshotLoad {
            snapshot: self.shared.snapshot,
            key,
            after: after.unwrap_or(0),
            has_after: u8::from(after.is_some()),
            limit: ipc::MAX_PAGE as u32,
        };
        match self.request(frame)? {
            Frame::Ordinals { values, more } if more <= 1 => Ok((values, more == 1)),
            _ => Err(invalid("yesnod returned the wrong load-page response")),
        }
    }
}

impl SetSnapshot for PeerSnapshot {
    fn version(&self) -> Version {
        self.shared.version
    }

    fn load(&self, key: u64) -> Result<Vec<u64>> {
        let mut all = Vec::new();
        let mut after = None;
        loop {
            let (page, more) = self.load_page(key, after)?;
            if page.windows(2).any(|w| w[0] >= w[1])
                || page
                    .first()
                    .is_some_and(|&first| after.is_some_and(|a| first <= a))
                || (more && page.is_empty())
            {
                return Err(invalid("yesnod load pages did not advance in order"));
            }
            after = page.last().copied();
            all.extend(page);
            if !more {
                return Ok(all);
            }
        }
    }

    fn key_range(&self, lo: u64, hi: u64) -> Result<Vec<u64>> {
        let mut all = Vec::new();
        let mut next = lo;
        while next < hi {
            let frame = Frame::SnapshotKeyRange {
                snapshot: self.shared.snapshot,
                lo: next,
                hi,
                // yesnod bounds enumeration per page; request its largest page
                // to keep round trips low when an index has many keys.
                limit: ipc::MAX_PAGE as u32,
            };
            let Frame::Keys { values, more } = self.request(frame)? else {
                return Err(invalid("yesnod returned the wrong key-range response"));
            };
            if more > 1
                || values.windows(2).any(|w| w[0] >= w[1])
                || values.first().is_some_and(|&first| first < next)
                || values.last().is_some_and(|&last| last >= hi)
                || (more == 1 && values.is_empty())
            {
                return Err(invalid("yesnod key pages did not advance in range"));
            }
            next = match values.last().copied() {
                Some(last) => last + 1, // last < hi <= u64::MAX, so no overflow.
                None => hi,
            };
            all.extend(values);
            if more == 0 {
                break;
            }
        }
        Ok(all)
    }

    fn cardinality(&self, key: u64) -> Result<u64> {
        match self.request(Frame::SnapshotCardinality {
            snapshot: self.shared.snapshot,
            key,
        })? {
            Frame::Count { value } => Ok(value),
            _ => Err(invalid("yesnod returned the wrong cardinality response")),
        }
    }

    fn contains(&self, key: u64, ordinal: u64) -> Result<bool> {
        match self.request(Frame::SnapshotContains {
            snapshot: self.shared.snapshot,
            key,
            ordinal,
        })? {
            Frame::Bool { value } if value <= 1 => Ok(value == 1),
            _ => Err(invalid("yesnod returned the wrong contains response")),
        }
    }

    fn max(&self, key: u64) -> Result<Option<u64>> {
        match self.request(Frame::SnapshotMax {
            snapshot: self.shared.snapshot,
            key,
        })? {
            Frame::Ordinal { present: 0, .. } => Ok(None),
            Frame::Ordinal { present: 1, value } => Ok(Some(value)),
            _ => Err(invalid("yesnod returned the wrong max response")),
        }
    }

    fn load_block(&self, key: u64, block: u64, out: &mut BlockMask) -> Result<bool> {
        out.fill(0);
        let base = block
            .checked_mul(65_536)
            .ok_or_else(|| invalid("block address exceeds the ordinal space"))?;
        let end = base.saturating_add(65_536);
        let mut after = base.checked_sub(1);
        let mut any = false;
        loop {
            let (page, more) = self.load_page(key, after)?;
            let mut past = false;
            for ordinal in page.iter().copied() {
                if ordinal >= end {
                    past = true;
                    break;
                }
                if ordinal < base || after.is_some_and(|a| ordinal <= a) {
                    return Err(invalid("yesnod block page moved backwards"));
                }
                let bit = (ordinal - base) as usize;
                out[bit / 64] |= 1u64 << (bit % 64);
                any = true;
            }
            if past || !more {
                return Ok(any);
            }
            after = page.last().copied();
            if after.is_none() {
                return Err(invalid("yesnod empty block page claims more data"));
            }
        }
    }

    fn open_lanes(&self, keys: &[u64]) -> Result<Option<Box<dyn Lanes>>> {
        let mut connection = self
            .shared
            .connection
            .lock()
            .map_err(|_| unavailable("peer connection lock was poisoned"))?;
        // All groups share this snapshot. The server advertises limits that
        // are valid for its actual arena or inline transport, so no client-side
        // transport constant belongs here.
        let groups_needed = keys.len().div_ceil(connection.max_lanes);
        if groups_needed == 0 || groups_needed > connection.max_handles - connection.active_handles
        {
            return Ok(None);
        }
        let arena = connection.arena.clone();
        let max_lanes = connection.max_lanes;
        let max_blocks = connection.max_blocks;
        // Keep raw handles until the mutex is released. Constructing a
        // PeerLanes earlier would make a partial-acquisition error drop it
        // while holding the same mutex that its Drop uses to release a handle.
        let mut acquired = Vec::with_capacity(groups_needed);
        let mut failure = None;
        for chunk in keys.chunks(max_lanes) {
            match connection.request(
                Frame::LanesAcquire {
                    snapshot: self.shared.snapshot,
                    keys: chunk.to_vec(),
                },
                self.shared.version,
            ) {
                Ok(Frame::LanesAcquired { lanes, arena_off }) => {
                    connection.active_handles += 1;
                    acquired.push((chunk.to_vec(), lanes, arena_off));
                }
                Ok(_) => {
                    failure = Some(invalid("yesnod did not acquire the requested lanes"));
                    break;
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        if let Some(error) = failure {
            for (_, lanes, _) in acquired {
                let _ = connection.request(Frame::LanesRelease { lanes }, self.shared.version);
                connection.active_handles -= 1;
            }
            return Err(error);
        }
        drop(connection);
        let groups = acquired
            .into_iter()
            .map(|(keys, lanes, arena_off)| PeerLanes {
                shared: self.shared.clone(),
                keys,
                last_requested: None,
                handle: lanes,
                arena_off: arena_off as usize,
                arena: arena.clone(),
                max_lanes,
                max_blocks,
                descriptors: Vec::new(),
                inline: Vec::new(),
                inline_offsets: Vec::new(),
                cursor: 0,
                done: false,
            })
            .collect::<Vec<_>>();
        tracing::debug!(
            lanes = keys.len(),
            handles = groups.len(),
            "opened served lanes"
        );
        Ok(Some(Box::new(CompositeLanes { groups, max_lanes })))
    }
}

struct CompositeLanes {
    groups: Vec<PeerLanes>,
    max_lanes: usize,
}

impl Lanes for CompositeLanes {
    fn read(&mut self, lane: usize, block: u64, out: &mut BlockMask) -> Result<bool> {
        self.groups
            .get_mut(lane / self.max_lanes)
            .ok_or_else(|| invalid("lane index is outside this cursor"))?
            .read(lane % self.max_lanes, block, out)
    }
}

struct PeerLanes {
    shared: Arc<Shared>,
    keys: Vec<u64>,
    last_requested: Option<u64>,
    handle: u64,
    arena_off: usize,
    arena: Option<Arc<Mmap>>,
    max_lanes: usize,
    max_blocks: usize,
    descriptors: Vec<Block>,
    inline: Vec<u8>,
    inline_offsets: Vec<Vec<usize>>,
    cursor: usize,
    done: bool,
}

impl PeerLanes {
    fn fetch(&mut self) -> Result<()> {
        if self.done {
            return Ok(());
        }
        let response = self.shared.request(Frame::BlockAdvanceMany {
            lanes: self.handle,
            max_blocks: self.max_blocks as u32,
        })?;
        let (blocks, payload) = match response {
            Frame::Blocks { blocks } if self.arena.is_some() => (blocks, Vec::new()),
            Frame::BlocksInline { blocks, payload } if self.arena.is_none() => (blocks, payload),
            _ => return Err(invalid("yesnod returned the wrong lane-batch response")),
        };
        if blocks.iter().any(|b| b.lanes.len() > self.max_lanes)
            || blocks.windows(2).any(|w| w[0].prefix >= w[1].prefix)
        {
            return Err(invalid("yesnod lane batch has invalid shape"));
        }
        self.done = blocks.is_empty();
        self.inline_offsets.clear();
        let mut offset = 0usize;
        for block in &blocks {
            let mut lane_offsets = Vec::with_capacity(block.lanes.len());
            for lane in &block.lanes {
                lane_offsets.push(offset);
                offset += lane.kind.payload_bytes(lane.count);
            }
            self.inline_offsets.push(lane_offsets);
        }
        if self.arena.is_none() && offset != payload.len() {
            return Err(invalid(
                "yesnod inline payload differs from lane descriptors",
            ));
        }
        self.inline = payload;
        self.descriptors = blocks;
        self.cursor = 0;
        Ok(())
    }

    fn payload(&self, block_index: usize, lane_index: usize, lane: Lane) -> Result<&[u8]> {
        let len = lane.kind.payload_bytes(lane.count);
        if len > ipc::LANE_BYTES {
            return Err(invalid("yesnod lane exceeds its fixed payload slot"));
        }
        if let Some(arena) = &self.arena {
            let offset = self.arena_off
                + block_index * self.max_lanes * ipc::LANE_BYTES
                + lane_index * ipc::LANE_BYTES;
            arena
                .get(offset..offset + len)
                .ok_or_else(|| invalid("yesnod arena lane lies outside the mapping"))
        } else {
            let offset = self.inline_offsets[block_index][lane_index];
            self.inline
                .get(offset..offset + len)
                .ok_or_else(|| invalid("yesnod inline lane lies outside the frame"))
        }
    }
}

impl Drop for PeerLanes {
    fn drop(&mut self) {
        if let Ok(mut conn) = self.shared.connection.lock() {
            let _ = conn.request(
                Frame::LanesRelease { lanes: self.handle },
                self.shared.version,
            );
            conn.active_handles = conn.active_handles.saturating_sub(1);
        }
    }
}

impl Lanes for PeerLanes {
    fn read(&mut self, lane: usize, block: u64, out: &mut BlockMask) -> Result<bool> {
        out.fill(0);
        let key = *self
            .keys
            .get(lane)
            .ok_or_else(|| invalid("lane index is outside this cursor"))?;
        if self.last_requested.is_some_and(|previous| block < previous) {
            return PeerSnapshot {
                shared: self.shared.clone(),
            }
            .load_block(key, block, out);
        }
        self.last_requested = Some(block);
        loop {
            if self.cursor >= self.descriptors.len() {
                if self.done {
                    return Ok(false);
                }
                self.fetch()?;
                continue;
            }
            let current = self.descriptors[self.cursor].prefix;
            if current < block {
                self.cursor += 1;
                continue;
            }
            if current > block {
                return Ok(false);
            }
            let descriptor = *self.descriptors[self.cursor]
                .lanes
                .get(lane)
                .ok_or_else(|| invalid("lane index is outside this cursor"))?;
            let bytes = self.payload(self.cursor, lane, descriptor)?;
            return decode_lane(descriptor, bytes, out);
        }
    }
}

fn decode_lane(lane: Lane, bytes: &[u8], out: &mut BlockMask) -> Result<bool> {
    match lane.kind {
        LaneKind::Absent => Ok(false),
        LaneKind::Bitmap => {
            if lane.count != 1024 || bytes.len() != 8192 {
                return Err(invalid("yesnod bitmap lane has the wrong width"));
            }
            for (word, chunk) in out.iter_mut().zip(bytes.chunks_exact(8)) {
                *word = u64::from_le_bytes(chunk.try_into().expect("eight-byte chunk"));
            }
            Ok(out.iter().any(|&w| w != 0))
        }
        LaneKind::Array => {
            for pair in bytes.chunks_exact(2) {
                let bit = u16::from_le_bytes([pair[0], pair[1]]) as usize;
                out[bit / 64] |= 1u64 << (bit % 64);
            }
            Ok(lane.count != 0)
        }
        LaneKind::Run => {
            for quad in bytes.chunks_exact(4) {
                let lo = u16::from_le_bytes([quad[0], quad[1]]) as usize;
                let hi = u16::from_le_bytes([quad[2], quad[3]]) as usize;
                if lo > hi {
                    return Err(invalid("yesnod run lane has a reversed interval"));
                }
                fill_range(out, lo, hi);
            }
            Ok(lane.count != 0)
        }
    }
}

// Expand inclusive run intervals by whole words. LIVE commonly stores a
// single 65,536-bit run; setting every bit separately would spend 65,536
// iterations per block where the embedded adapter spends about 1,024 writes.
fn fill_range(out: &mut BlockMask, lo: usize, hi: usize) {
    let (first, last) = (lo >> 6, hi >> 6);
    let head = u64::MAX << (lo & 63);
    let tail = u64::MAX >> (63 - (hi & 63));
    if first == last {
        out[first] |= head & tail;
    } else {
        out[first] |= head;
        out[first + 1..last].fill(u64::MAX);
        out[last] |= tail;
    }
}
