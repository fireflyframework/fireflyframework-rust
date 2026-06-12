//! A tiny in-process RESP2 server implementing the handful of commands
//! [`firefly_cache_redis::RedisAdapter`] issues — `GET`, `SET` (with the
//! `PX`/`NX` options), `DEL`, `EXISTS`, `SCAN MATCH`, `DBSIZE`, `FLUSHDB`,
//! and `PING`, plus the `CLIENT SETINFO` handshake the redis crate sends on
//! a fresh multiplexed connection. It lets the adapter be exercised
//! end-to-end over a real TCP socket with **no external Redis**.
//!
//! The fake stores plain key -> bytes; it does not enforce TTL expiry (unit
//! tests assert the wire behaviour of each command, and the `PX` argument
//! is captured separately so a TTL test can verify it was forwarded). The
//! glob support for `SCAN MATCH` covers the `<prefix>*` patterns the
//! adapter generates plus literal escapes.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// Shared, observable state of the fake server. Tests assert against these.
#[derive(Default)]
pub struct FakeState {
    /// Key -> stored value bytes.
    pub store: HashMap<String, Vec<u8>>,
    /// Key -> last `PX` (milliseconds) seen on a `SET`, so a TTL test can
    /// confirm the adapter forwarded the expiry.
    pub last_px: HashMap<String, u64>,
}

/// A running fake server. Holds the bound port and the shared state.
pub struct FakeRedis {
    pub port: u16,
    pub state: Arc<Mutex<FakeState>>,
}

impl FakeRedis {
    /// Binds an ephemeral port (`127.0.0.1:0`) and spawns the accept loop.
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

    /// The `redis://` URL the adapter should connect to.
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

/// Reads one RESP2 client request: an array of bulk strings. Returns `None`
/// on EOF.
async fn read_command<R>(reader: &mut R) -> io::Result<Option<Vec<Vec<u8>>>>
where
    R: AsyncReadExt + Unpin,
{
    let line = match read_line(reader).await? {
        Some(line) => line,
        None => return Ok(None),
    };
    if line.is_empty() || line[0] != b'*' {
        return Ok(Some(Vec::new()));
    }
    let count: usize = parse_len(&line[1..]);
    let mut args = Vec::with_capacity(count);
    for _ in 0..count {
        let header = read_line(reader)
            .await?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "eof in bulk header"))?;
        let len: usize = parse_len(&header[1..]);
        let mut buf = vec![0u8; len + 2]; // include trailing CRLF
        reader.read_exact(&mut buf).await?;
        buf.truncate(len);
        args.push(buf);
    }
    Ok(Some(args))
}

/// Reads a single CRLF-terminated line (without the trailing CRLF). Returns
/// `None` on EOF before any byte.
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
            let _ = reader.read(&mut byte).await?; // consume the \n
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
        "CLIENT" => reply_ok(), // CLIENT SETINFO … -> +OK
        "PING" => reply_ok(),
        "GET" => handle_get(args, state),
        "SET" => handle_set(args, state),
        "DEL" => handle_del(args, state),
        "EXISTS" => handle_exists(args, state),
        "SCAN" => handle_scan(args, state),
        "DBSIZE" => {
            let n = state.lock().unwrap().store.len() as i64;
            reply_int(n)
        }
        "FLUSHDB" => {
            let mut guard = state.lock().unwrap();
            guard.store.clear();
            guard.last_px.clear();
            reply_ok()
        }
        // Anything else: a generic OK keeps the connection healthy.
        _ => reply_ok(),
    }
}

/// `GET key` -> bulk string, or nil when absent.
fn handle_get(args: &[Vec<u8>], state: &Arc<Mutex<FakeState>>) -> Vec<u8> {
    let key = String::from_utf8_lossy(&args[1]).to_string();
    match state.lock().unwrap().store.get(&key) {
        Some(v) => reply_bulk(v),
        None => reply_nil(),
    }
}

/// `SET key value [PX ms] [NX]` -> `+OK`, or nil when an `NX` write is
/// rejected because the key already exists.
fn handle_set(args: &[Vec<u8>], state: &Arc<Mutex<FakeState>>) -> Vec<u8> {
    let key = String::from_utf8_lossy(&args[1]).to_string();
    let value = args[2].clone();
    let mut nx = false;
    let mut px: Option<u64> = None;
    let mut i = 3;
    while i < args.len() {
        let tok = String::from_utf8_lossy(&args[i]).to_ascii_uppercase();
        match tok.as_str() {
            "NX" => {
                nx = true;
                i += 1;
            }
            "PX" if i + 1 < args.len() => {
                px = std::str::from_utf8(&args[i + 1])
                    .ok()
                    .and_then(|s| s.parse().ok());
                i += 2;
            }
            "EX" if i + 1 < args.len() => {
                px = std::str::from_utf8(&args[i + 1])
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|s| s * 1000);
                i += 2;
            }
            _ => i += 1,
        }
    }
    let mut guard = state.lock().unwrap();
    if nx && guard.store.contains_key(&key) {
        return reply_nil(); // NX rejected
    }
    guard.store.insert(key.clone(), value);
    if let Some(ms) = px {
        guard.last_px.insert(key, ms);
    }
    reply_ok()
}

/// `DEL key…` -> integer count of keys actually removed.
fn handle_del(args: &[Vec<u8>], state: &Arc<Mutex<FakeState>>) -> Vec<u8> {
    let mut guard = state.lock().unwrap();
    let mut count = 0i64;
    for raw in &args[1..] {
        let key = String::from_utf8_lossy(raw).to_string();
        if guard.store.remove(&key).is_some() {
            guard.last_px.remove(&key);
            count += 1;
        }
    }
    reply_int(count)
}

/// `EXISTS key…` -> integer count of keys present.
fn handle_exists(args: &[Vec<u8>], state: &Arc<Mutex<FakeState>>) -> Vec<u8> {
    let guard = state.lock().unwrap();
    let count = args[1..]
        .iter()
        .filter(|raw| {
            guard
                .store
                .contains_key(&String::from_utf8_lossy(raw).to_string())
        })
        .count() as i64;
    reply_int(count)
}

/// `SCAN cursor [MATCH pattern] [COUNT n]` -> a two-element array: the next
/// cursor (bulk string) and an array of matching keys. This fake returns
/// every match in one pass and a `0` cursor, which is a valid SCAN
/// implementation (the cursor contract permits any traversal that visits
/// every key at least once).
fn handle_scan(args: &[Vec<u8>], state: &Arc<Mutex<FakeState>>) -> Vec<u8> {
    let mut pattern = "*".to_string();
    let mut i = 2; // args[1] is the cursor
    while i < args.len() {
        let tok = String::from_utf8_lossy(&args[i]).to_ascii_uppercase();
        match tok.as_str() {
            "MATCH" if i + 1 < args.len() => {
                pattern = String::from_utf8_lossy(&args[i + 1]).to_string();
                i += 2;
            }
            "COUNT" if i + 1 < args.len() => i += 2,
            _ => i += 1,
        }
    }
    // Only return matches on the first cursor; a non-zero incoming cursor
    // means the client is paginating a traversal we already completed.
    let cursor = String::from_utf8_lossy(&args[1]).to_string();
    let keys: Vec<String> = if cursor == "0" {
        let guard = state.lock().unwrap();
        guard
            .store
            .keys()
            .filter(|k| glob_match(&pattern, k))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    // *2\r\n $1\r\n 0\r\n *<n>\r\n <bulk keys…>
    let mut out = Vec::new();
    out.extend_from_slice(b"*2\r\n");
    out.extend(bulk(b"0")); // next cursor: always 0 (single pass)
    out.extend_from_slice(format!("*{}\r\n", keys.len()).as_bytes());
    for k in keys {
        out.extend(bulk(k.as_bytes()));
    }
    out
}

/// Minimal Redis glob matcher supporting the `*` wildcard, `?`, and `\`
/// escapes — enough for the adapter's `<escaped-prefix>*` patterns plus
/// literal-prefix escapes. `*` matches any run (including empty); `?`
/// matches one char; `\x` matches the literal `x`.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn rec(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => rec(&p[1..], t) || (!t.is_empty() && rec(p, &t[1..])),
            Some('?') => !t.is_empty() && rec(&p[1..], &t[1..]),
            Some('\\') => match (p.get(1), t.first()) {
                (Some(esc), Some(c)) if esc == c => rec(&p[2..], &t[1..]),
                _ => false,
            },
            Some(c) => matches!(t.first(), Some(tc) if tc == c) && rec(&p[1..], &t[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    rec(&p, &t)
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

fn reply_nil() -> Vec<u8> {
    b"$-1\r\n".to_vec()
}
