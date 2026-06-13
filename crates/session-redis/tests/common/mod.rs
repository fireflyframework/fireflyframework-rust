// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! A tiny in-process RESP2 server implementing the handful of commands
//! [`firefly_session_redis::RedisSessionRegistry`] issues — `ZADD`,
//! `ZRANGE … WITHSCORES`, `ZREM`, `ZCARD`, `EXPIRE`, and `PING`, plus the
//! `CLIENT SETINFO` handshake the redis crate sends on a fresh multiplexed
//! connection. It lets the adapter be exercised end-to-end over a real TCP
//! socket with **no external Redis**, so the contract tests are always-on.
//!
//! The fake models each key as a sorted set: a map of member → score. `ZADD`
//! inserts/updates a member's score, `ZRANGE 0 -1 WITHSCORES` returns members
//! ascending by score (oldest-first, with ties broken lexicographically, like
//! Redis), `ZREM` removes a member (deleting the key when it becomes empty,
//! like Redis), and `ZCARD` reports the cardinality. `EXPIRE` is accepted and
//! the requested TTL is recorded per key so a test can assert the adapter
//! forwarded it, but the fake does not actually expire keys on a timer (TTL
//! semantics are Redis's, not the adapter's, responsibility).

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// Shared, observable state of the fake server. Tests assert against these.
#[derive(Default)]
pub struct FakeState {
    /// Key → (member → score) sorted-set contents.
    pub sets: HashMap<String, HashMap<String, f64>>,
    /// Key → last `EXPIRE` TTL (seconds) seen, so a test can confirm the
    /// adapter forwarded the sliding TTL.
    pub last_expire: HashMap<String, i64>,
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

/// Reads RESP2 commands off `stream` and dispatches each, writing the matching
/// reply. Returns when the client disconnects.
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

/// Reads one RESP2 client request: an array of bulk strings. Returns `None` on
/// EOF.
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
        "CLIENT" => reply_ok(), // CLIENT SETINFO … → +OK
        "PING" => reply_ok(),
        "ZADD" => handle_zadd(args, state),
        "ZREM" => handle_zrem(args, state),
        "ZRANGE" => handle_zrange(args, state),
        "ZCARD" => handle_zcard(args, state),
        "EXPIRE" => handle_expire(args, state),
        // Anything else: a generic OK keeps the connection healthy.
        _ => reply_ok(),
    }
}

/// `ZADD key score member [score member …]` → integer count of *newly added*
/// members (updates of an existing member's score do not count), matching
/// Redis without the `CH` flag.
fn handle_zadd(args: &[Vec<u8>], state: &Arc<Mutex<FakeState>>) -> Vec<u8> {
    let key = String::from_utf8_lossy(&args[1]).to_string();
    let mut guard = state.lock().unwrap();
    let set = guard.sets.entry(key).or_default();
    let mut added = 0i64;
    let mut i = 2;
    while i + 1 < args.len() {
        let score: f64 = std::str::from_utf8(&args[i])
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        let member = String::from_utf8_lossy(&args[i + 1]).to_string();
        if set.insert(member, score).is_none() {
            added += 1;
        }
        i += 2;
    }
    reply_int(added)
}

/// `ZREM key member [member …]` → integer count removed. An emptied set is
/// deleted (Redis drops empty sorted sets).
fn handle_zrem(args: &[Vec<u8>], state: &Arc<Mutex<FakeState>>) -> Vec<u8> {
    let key = String::from_utf8_lossy(&args[1]).to_string();
    let mut guard = state.lock().unwrap();
    let mut removed = 0i64;
    if let Some(set) = guard.sets.get_mut(&key) {
        for raw in &args[2..] {
            let member = String::from_utf8_lossy(raw).to_string();
            if set.remove(&member).is_some() {
                removed += 1;
            }
        }
        if set.is_empty() {
            guard.sets.remove(&key);
        }
    }
    reply_int(removed)
}

/// `ZRANGE key start stop [WITHSCORES]` over the full `0 -1` range the adapter
/// uses, ascending by score (ties broken lexicographically by member, like
/// Redis). Replies as a flat array `[member, score, member, score, …]` when
/// `WITHSCORES` is present, else just the members.
fn handle_zrange(args: &[Vec<u8>], state: &Arc<Mutex<FakeState>>) -> Vec<u8> {
    let key = String::from_utf8_lossy(&args[1]).to_string();
    let withscores = args[2..]
        .iter()
        .any(|a| String::from_utf8_lossy(a).eq_ignore_ascii_case("WITHSCORES"));
    let guard = state.lock().unwrap();
    let mut entries: Vec<(String, f64)> = guard
        .sets
        .get(&key)
        .map(|s| s.iter().map(|(m, sc)| (m.clone(), *sc)).collect())
        .unwrap_or_default();
    // Ascending by score, tie-break by member — Redis's sorted-set order.
    entries.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut out = Vec::new();
    let n = if withscores {
        entries.len() * 2
    } else {
        entries.len()
    };
    out.extend_from_slice(format!("*{n}\r\n").as_bytes());
    for (member, score) in entries {
        out.extend(bulk(member.as_bytes()));
        if withscores {
            // Redis formats integral scores without a trailing ".0".
            let s = if score.fract() == 0.0 {
                format!("{}", score as i64)
            } else {
                format!("{score}")
            };
            out.extend(bulk(s.as_bytes()));
        }
    }
    out
}

/// `ZCARD key` → integer cardinality.
fn handle_zcard(args: &[Vec<u8>], state: &Arc<Mutex<FakeState>>) -> Vec<u8> {
    let key = String::from_utf8_lossy(&args[1]).to_string();
    let guard = state.lock().unwrap();
    let n = guard.sets.get(&key).map_or(0, HashMap::len) as i64;
    reply_int(n)
}

/// `EXPIRE key ttl` → `:1` (or `:0` if the key is missing). The TTL is recorded
/// so a test can assert the adapter forwarded the sliding expiry.
fn handle_expire(args: &[Vec<u8>], state: &Arc<Mutex<FakeState>>) -> Vec<u8> {
    let key = String::from_utf8_lossy(&args[1]).to_string();
    let ttl: i64 = std::str::from_utf8(&args[2])
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut guard = state.lock().unwrap();
    let exists = guard.sets.contains_key(&key);
    guard.last_expire.insert(key, ttl);
    reply_int(if exists { 1 } else { 0 })
}

fn bulk(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 16);
    out.extend_from_slice(format!("${}\r\n", bytes.len()).as_bytes());
    out.extend_from_slice(bytes);
    out.extend_from_slice(b"\r\n");
    out
}

fn reply_ok() -> Vec<u8> {
    b"+OK\r\n".to_vec()
}

fn reply_int(n: i64) -> Vec<u8> {
    format!(":{n}\r\n").into_bytes()
}
