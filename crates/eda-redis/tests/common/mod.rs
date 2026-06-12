//! A tiny in-process RESP2 server implementing only the handful of
//! commands [`firefly_eda_redis::RedisStreamsBroker`] uses
//! (`CLIENT SETINFO`, `XGROUP CREATE … MKSTREAM`, `XADD`, `XREADGROUP`,
//! `XACK`). It lets the broker's full lifecycle — connect, group
//! creation, publish, consume, ack — be exercised end-to-end over a real
//! TCP socket with no external Redis.
//!
//! The redis crate negotiates RESP2 for a plain `redis://` URL with no
//! password and db 0, so the connection handshake is just the two
//! `CLIENT SETINFO` commands (replied to with `+OK`). Everything else is
//! parsed as a RESP2 array of bulk strings and dispatched by command
//! name.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// One stored stream entry: an id plus its `envelope` payload bytes.
#[derive(Clone)]
pub struct StoredEntry {
    pub id: String,
    pub envelope: Vec<u8>,
}

/// Shared, observable state of the fake server: every stream's entries,
/// the next-undelivered cursor per `(stream, group)` consumer, and the
/// set of acked entry ids. Tests assert against these.
#[derive(Default)]
pub struct FakeState {
    /// Stream key -> ordered entries (in `XADD` order).
    pub streams: HashMap<String, Vec<StoredEntry>>,
    /// `(stream, group)` -> count of entries already handed out via
    /// `XREADGROUP` with the `>` id.
    pub delivered: HashMap<(String, String), usize>,
    /// `(stream, group)` -> ids that have been `XACK`-ed.
    pub acked: HashMap<(String, String), Vec<String>>,
    /// Monotonic counter so generated ids are unique and ordered.
    seq: u64,
}

impl FakeState {
    /// Total entries acked for `(stream, group)`.
    pub fn ack_count(&self, stream: &str, group: &str) -> usize {
        self.acked
            .get(&(stream.to_string(), group.to_string()))
            .map(Vec::len)
            .unwrap_or(0)
    }

    /// Number of pending (delivered-but-unacked) entries for
    /// `(stream, group)`.
    pub fn pending_count(&self, stream: &str, group: &str) -> usize {
        let key = (stream.to_string(), group.to_string());
        let delivered = self.delivered.get(&key).copied().unwrap_or(0);
        let acked = self.acked.get(&key).map(Vec::len).unwrap_or(0);
        delivered.saturating_sub(acked)
    }

    /// Injects a raw entry with arbitrary `envelope` bytes directly into
    /// `stream`, bypassing `XADD`. Lets tests stage a poison message
    /// (e.g. invalid JSON) and observe the broker's ack-and-skip path.
    pub fn push_raw_entry(&mut self, stream: &str, envelope: Vec<u8>) -> String {
        self.seq += 1;
        let id = format!("{}-0", self.seq);
        self.streams
            .entry(stream.to_string())
            .or_default()
            .push(StoredEntry {
                id: id.clone(),
                envelope,
            });
        id
    }
}

/// A running fake server. Holds the bound port and the shared state.
pub struct FakeRedis {
    pub port: u16,
    pub state: Arc<Mutex<FakeState>>,
}

impl FakeRedis {
    /// Binds an ephemeral port (`127.0.0.1:0`) and spawns the accept
    /// loop. The returned handle exposes the bound `port` and the shared
    /// `state`.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(FakeState::default()));
        let accept_state = Arc::clone(&state);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let conn_state = Arc::clone(&accept_state);
                tokio::spawn(async move {
                    let _ = serve_conn(stream, conn_state).await;
                });
            }
        });
        Self { port, state }
    }

    /// The `redis://` URL the broker should connect to.
    pub fn url(&self) -> String {
        format!("redis://127.0.0.1:{}/0", self.port)
    }
}

/// Reads RESP2 commands off `stream` and dispatches each, writing the
/// matching reply. Returns when the client disconnects.
async fn serve_conn(stream: TcpStream, state: Arc<Mutex<FakeState>>) -> io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    loop {
        let args = match read_command(&mut reader).await? {
            Some(args) => args,
            None => return Ok(()), // EOF
        };
        if args.is_empty() {
            continue;
        }
        let reply = dispatch(&args, &state);
        write_half.write_all(&reply).await?;
        write_half.flush().await?;
    }
}

/// Reads one RESP2 client request: an array of bulk strings. Returns
/// `None` on EOF.
async fn read_command<R>(reader: &mut R) -> io::Result<Option<Vec<Vec<u8>>>>
where
    R: AsyncReadExt + Unpin,
{
    let line = match read_line(reader).await? {
        Some(line) => line,
        None => return Ok(None),
    };
    if line.is_empty() || line[0] != b'*' {
        // Inline commands are unused by redis-rs; ignore.
        return Ok(Some(Vec::new()));
    }
    let count: usize = parse_len(&line[1..]);
    let mut args = Vec::with_capacity(count);
    for _ in 0..count {
        let header = read_line(reader)
            .await?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "eof in bulk header"))?;
        // Bulk string: `$<len>\r\n<bytes>\r\n`.
        let len: usize = parse_len(&header[1..]);
        let mut buf = vec![0u8; len + 2]; // include trailing CRLF
        reader.read_exact(&mut buf).await?;
        buf.truncate(len);
        args.push(buf);
    }
    Ok(Some(args))
}

/// Reads a single CRLF-terminated line (without the trailing CRLF).
/// Returns `None` on EOF before any byte.
async fn read_line<R>(reader: &mut R) -> io::Result<Option<Vec<u8>>>
where
    R: AsyncReadExt + Unpin,
{
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = reader.read(&mut byte).await?;
        if n == 0 {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        if byte[0] == b'\r' {
            // consume the following \n
            let _ = reader.read(&mut byte).await?;
            return Ok(Some(line));
        }
        line.push(byte[0]);
    }
}

/// Parses a decimal length prefix, tolerating a leading sign.
fn parse_len(bytes: &[u8]) -> usize {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .map(|n| n.max(0) as usize)
        .unwrap_or(0)
}

/// Dispatches one parsed command to its RESP2 reply bytes.
fn dispatch(args: &[Vec<u8>], state: &Arc<Mutex<FakeState>>) -> Vec<u8> {
    let cmd = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
    match cmd.as_str() {
        "CLIENT" => reply_ok(), // CLIENT SETINFO ... -> +OK
        "XGROUP" => reply_ok(), // XGROUP CREATE ... MKSTREAM -> +OK
        "XADD" => handle_xadd(args, state),
        "XREADGROUP" => handle_xreadgroup(args, state),
        "XACK" => handle_xack(args, state),
        // Anything else: a generic OK keeps the connection healthy.
        _ => reply_ok(),
    }
}

/// `XADD <stream> * envelope <bytes>` -> bulk-string entry id.
fn handle_xadd(args: &[Vec<u8>], state: &Arc<Mutex<FakeState>>) -> Vec<u8> {
    let stream = String::from_utf8_lossy(&args[1]).to_string();
    // args[2] is the id token "*"; fields start at args[3].
    let mut envelope = Vec::new();
    let mut i = 3;
    while i + 1 < args.len() {
        if args[i].eq_ignore_ascii_case(b"envelope") {
            envelope = args[i + 1].clone();
        }
        i += 2;
    }
    let mut guard = state.lock().unwrap();
    guard.seq += 1;
    let id = format!("{}-0", guard.seq);
    guard.streams.entry(stream).or_default().push(StoredEntry {
        id: id.clone(),
        envelope,
    });
    reply_bulk(id.as_bytes())
}

/// `XREADGROUP GROUP <g> <c> COUNT n BLOCK ms STREAMS <stream...> > >…`
/// -> RESP2 stream-read reply for never-delivered entries, or a null
/// array when there is nothing new.
fn handle_xreadgroup(args: &[Vec<u8>], state: &Arc<Mutex<FakeState>>) -> Vec<u8> {
    // Find GROUP <group> and the STREAMS keyword.
    let mut group = String::new();
    let mut streams_idx = None;
    let mut i = 1;
    while i < args.len() {
        let tok = String::from_utf8_lossy(&args[i]).to_ascii_uppercase();
        match tok.as_str() {
            "GROUP" if i + 2 < args.len() => {
                group = String::from_utf8_lossy(&args[i + 1]).to_string();
                i += 3;
            }
            "STREAMS" => {
                streams_idx = Some(i + 1);
                break;
            }
            _ => i += 1,
        }
    }
    let Some(streams_start) = streams_idx else {
        return reply_null_array();
    };
    // After STREAMS the args are [stream...][id...] split in half.
    let rest = &args[streams_start..];
    let n = rest.len() / 2;
    let stream_keys: Vec<String> = rest[..n]
        .iter()
        .map(|b| String::from_utf8_lossy(b).to_string())
        .collect();

    let mut guard = state.lock().unwrap();
    let mut out_streams: Vec<(String, Vec<StoredEntry>)> = Vec::new();
    for stream in stream_keys {
        let entries = guard.streams.get(&stream).cloned().unwrap_or_default();
        let key = (stream.clone(), group.clone());
        let already = guard.delivered.get(&key).copied().unwrap_or(0);
        if entries.len() > already {
            let fresh: Vec<StoredEntry> = entries[already..].to_vec();
            guard.delivered.insert(key, entries.len());
            out_streams.push((stream, fresh));
        }
    }
    drop(guard);

    if out_streams.is_empty() {
        return reply_null_array();
    }
    encode_xreadgroup(&out_streams)
}

/// `XACK <stream> <group> <id...>` -> integer count of newly-acked ids.
fn handle_xack(args: &[Vec<u8>], state: &Arc<Mutex<FakeState>>) -> Vec<u8> {
    let stream = String::from_utf8_lossy(&args[1]).to_string();
    let group = String::from_utf8_lossy(&args[2]).to_string();
    let mut guard = state.lock().unwrap();
    let acked = guard.acked.entry((stream, group)).or_default();
    let mut count = 0i64;
    for id in &args[3..] {
        let id = String::from_utf8_lossy(id).to_string();
        if !acked.contains(&id) {
            acked.push(id);
            count += 1;
        }
    }
    reply_int(count)
}

/// Encodes a RESP2 `XREADGROUP` reply:
/// `*<streams> [ *2 <key> *<entries> [ *2 <id> *<fields*2> [field value]* ] ]`.
fn encode_xreadgroup(streams: &[(String, Vec<StoredEntry>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("*{}\r\n", streams.len()).as_bytes());
    for (key, entries) in streams {
        out.extend_from_slice(b"*2\r\n");
        out.extend(bulk(key.as_bytes()));
        out.extend_from_slice(format!("*{}\r\n", entries.len()).as_bytes());
        for entry in entries {
            out.extend_from_slice(b"*2\r\n");
            out.extend(bulk(entry.id.as_bytes()));
            // Fields: one pair, envelope -> bytes.
            out.extend_from_slice(b"*2\r\n");
            out.extend(bulk(b"envelope"));
            out.extend(bulk(&entry.envelope));
        }
    }
    out
}

fn bulk(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 16);
    out.extend_from_slice(format!("${}\r\n", bytes.len()).as_bytes());
    out.extend_from_slice(bytes);
    out.extend_from_slice(b"\r\n");
    out
}

fn reply_bulk(bytes: &[u8]) -> Vec<u8> {
    bulk(bytes)
}

fn reply_ok() -> Vec<u8> {
    b"+OK\r\n".to_vec()
}

fn reply_int(n: i64) -> Vec<u8> {
    format!(":{n}\r\n").into_bytes()
}

fn reply_null_array() -> Vec<u8> {
    b"*-1\r\n".to_vec()
}
