//! ICS (Internet Chess Server) connection handling. Replaces net.c's
//! OpenTCP/SendToIcs/ProcessRawInput. We speak plain TCP, optionally
//! with timeseal v1 encoding (see `timeseal_encode`), and strip the
//! small number of Telnet IAC sequences FICS sends, same as the
//! original's TS_NONE/TS_IAC/TS_CMD state machine.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::debug;

const IAC: u8 = 255;
const WILL: u8 = 251;
const WONT: u8 = 252;
const DO: u8 = 253;
const DONT: u8 = 254;

/// Timeseal v1 XOR key. Timeseal stamps every line we send with our
/// local clock so the server can charge us for thinking time only,
/// not for network lag - without it, a stalled connection burns our
/// clock while our move is still in flight.
const TIMESEAL_KEY: &[u8] = b"Timestamp (FICS) v1.0 - programmed by Henrik Gram.";
/// Keepalive the server embeds in its output for timeseal clients;
/// it has to be stripped from the text and answered promptly.
const TIMESEAL_PING: &[u8] = b"[G]\0";
const TIMESEAL_PONG: &[u8] = b"\x029";

pub struct IcsConn {
    stream: TcpStream,
    /// Raw bytes read but not yet split into lines.
    pending: Vec<u8>,
    /// `--debug <file>`: wire-level transcript of everything sent to
    /// and received from the server, for chasing server-side bugs.
    debug_log: Option<File>,
    /// Set when timeseal is on: the instant our timestamps count from.
    /// Only differences between timestamps matter to the server.
    timeseal_start: Option<Instant>,
}

impl IcsConn {
    /// Connect to the ICS. If `debug_log` is given, every byte sent and
    /// received is appended to that file (see `log_wire`). With
    /// `timeseal`, every line we send is timeseal-encoded, starting
    /// with the handshake.
    pub async fn connect(
        host: &str,
        port: u16,
        debug_log: Option<&Path>,
        timeseal: bool,
    ) -> Result<Self> {
        let debug_log = match debug_log {
            Some(path) => Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .with_context(|| format!("failed to open debug log {}", path.display()))?,
            ),
            None => None,
        };

        let mut conn = IcsConn {
            stream: TcpStream::connect((host, port))
                .await
                .with_context(|| format!("failed to connect to {host}:{port}"))?,
            pending: Vec::new(),
            debug_log,
            timeseal_start: timeseal.then(Instant::now),
        };
        conn.log_event(&format!("connected to {host}:{port}"));
        if timeseal {
            // Lines are logged as plain text below; the wire carries
            // their timeseal encoding.
            conn.log_event("timeseal on - outgoing lines logged before encoding");
            let hello = format!("TIMESTAMP|icsdrone-rs|{}|\n", std::env::consts::OS);
            conn.write_lines(hello.as_bytes()).await?;
        }
        Ok(conn)
    }

    pub async fn send(&mut self, s: &str) -> Result<()> {
        debug!("-> ics: {}", s.trim_end());
        let buf = with_newline(s);
        self.log_wire("->", &buf);
        self.write_lines(&buf).await
    }

    /// Like `send`, but keeps the text itself out of the logs - for the
    /// password at login, so `--debug` transcripts are safe to share.
    pub async fn send_secret(&mut self, s: &str) -> Result<()> {
        debug!("-> ics: ********");
        self.log_wire("->", b"********\n");
        self.write_lines(&with_newline(s)).await
    }

    /// Write newline-terminated text, timeseal-encoding each line
    /// separately when timeseal is on.
    async fn write_lines(&mut self, buf: &[u8]) -> Result<()> {
        let Some(start) = self.timeseal_start else {
            return self.write_raw(buf).await;
        };
        let body = buf.strip_suffix(b"\n").unwrap_or(buf);
        let mut out = Vec::new();
        for line in body.split(|&b| b == b'\n') {
            let ts = start.elapsed().as_millis() as u64;
            out.extend(timeseal_encode(line, ts, ts as usize % TIMESEAL_KEY.len(), b'1'));
            out.push(b'\n');
        }
        self.write_raw(&out).await
    }

    async fn write_raw(&mut self, buf: &[u8]) -> Result<()> {
        self.stream.write_all(buf).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Read raw bytes off the socket into our pending buffer, stripping
    /// telnet IAC WILL/WONT/DO/DONT negotiation sequences as we go.
    /// Mirrors ProcessRawInput's telnet state machine.
    async fn fill(&mut self) -> Result<usize> {
        let mut chunk = [0u8; 4096];
        let n = self.stream.read(&mut chunk).await?;
        if n == 0 {
            self.log_event("connection closed by server");
            anyhow::bail!("ICS connection closed");
        }
        // Logged before IAC stripping / line splitting so the transcript
        // shows exactly what the server put on the wire.
        self.log_wire("<-", &chunk[..n]);
        let mut i = 0;
        while i < n {
            let c = chunk[i];
            if c == IAC {
                // Expect WILL/WONT/DO/DONT + one option byte; if it's
                // something else (e.g. IAC IAC) just drop the IAC and
                // keep the next byte as data, matching the original's
                // "any other IAC command -> back to normal" behaviour.
                if i + 1 < n && matches!(chunk[i + 1], WILL | WONT | DO | DONT) {
                    i += 3; // IAC + cmd + option
                    continue;
                } else {
                    i += 1;
                    continue;
                }
            }
            self.pending.push(c);
            i += 1;
        }
        // Searched for in `pending` rather than this chunk alone, so a
        // ping split across two reads is still caught.
        if self.timeseal_start.is_some() {
            while let Some(pos) = find(&self.pending, TIMESEAL_PING) {
                self.pending.drain(pos..pos + TIMESEAL_PING.len());
                self.log_wire("->", b"<timeseal ping reply>\n");
                self.write_lines(&with_newline_bytes(TIMESEAL_PONG)).await?;
            }
        }
        Ok(n)
    }

    /// Try to pull one complete line out of `pending`. Mirrors the
    /// original's eol-scanning: after the first \n or \r, consume any
    /// further run of \r/\n that contains at most one \n, since
    /// timeseal on Windows is known to emit "\r\n\r" as a single
    /// terminator.
    fn try_extract_line(&mut self) -> Option<String> {
        let first = self.pending.iter().position(|&b| b == b'\n' || b == b'\r')?;
        let mut end = first + 1;
        let mut nl_seen = self.pending[first] == b'\n';
        while end < self.pending.len() {
            let b = self.pending[end];
            let consume = if !nl_seen {
                b == b'\r' || b == b'\n'
            } else {
                b == b'\r'
            };
            if !consume {
                break;
            }
            if b == b'\n' {
                nl_seen = true;
            }
            end += 1;
        }
        let text = String::from_utf8_lossy(&self.pending[..first]).to_string();
        self.pending.drain(..end);
        Some(text)
    }

    /// Append one direction's raw bytes to the `--debug` log, one log
    /// line per server line (split after each \\n) so it stays readable,
    /// with control bytes escaped so stray \\r, IAC etc. are visible.
    fn log_wire(&mut self, dir: &str, bytes: &[u8]) {
        let Some(file) = self.debug_log.as_mut() else {
            return;
        };
        let ts = timestamp();
        let mut out = String::new();
        for piece in bytes.split_inclusive(|&b| b == b'\n') {
            out.push_str(&format!("{ts} {dir} \"{}\"\n", escape_bytes(piece)));
        }
        let _ = file.write_all(out.as_bytes());
    }

    fn log_event(&mut self, msg: &str) {
        if let Some(file) = self.debug_log.as_mut() {
            let _ = writeln!(file, "{} -- {msg}", timestamp());
        }
    }

    /// Read one line from the server, waiting (async) until one is
    /// available. Returns the line with terminator characters stripped.
    pub async fn read_line(&mut self) -> Result<String> {
        loop {
            if let Some(line) = self.try_extract_line() {
                return Ok(line);
            }
            self.fill().await?;
        }
    }
}

fn with_newline(s: &str) -> Vec<u8> {
    with_newline_bytes(s.as_bytes())
}

fn with_newline_bytes(s: &[u8]) -> Vec<u8> {
    let mut buf = s.to_vec();
    if !buf.ends_with(b"\n") {
        buf.push(b'\n');
    }
    buf
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Timeseal v1 encoding of one line (without its newline): append
/// "\x18<timestamp>\x19", pad to a multiple of 12 bytes with `filler`,
/// swap three byte pairs within each 12-byte block, then XOR with
/// `TIMESEAL_KEY` starting at `offset`, and finish with the offset
/// byte so the server can undo it. `offset` and `filler` are arbitrary
/// - the server reads the offset back and drops the padding.
fn timeseal_encode(line: &[u8], timestamp: u64, offset: usize, filler: u8) -> Vec<u8> {
    let mut buf = line.to_vec();
    buf.extend(format!("\x18{timestamp}\x19").bytes());
    let pad = 12 - buf.len() % 12;
    buf.resize(buf.len() + pad, filler);
    for block in buf.chunks_mut(12) {
        block.swap(0, 11);
        block.swap(2, 9);
        block.swap(4, 7);
    }
    for (i, b) in buf.iter_mut().enumerate() {
        let key = TIMESEAL_KEY[(i + offset) % TIMESEAL_KEY.len()];
        *b = ((*b | 0x80) ^ key).wrapping_sub(32);
    }
    buf.push(0x80 | offset as u8);
    buf
}

/// UTC wall-clock time as "HH:MM:SS.mmm", so the transcript can be
/// lined up against the server's own logs.
fn timestamp() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        now.subsec_millis()
    )
}

/// Printable ASCII as-is; \\r, \\n, \\t, \\\\ and \\" escaped; anything else
/// (telnet IAC sequences, stray high bytes) as \\xNN.
fn escape_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'\r' => out.push_str("\\r"),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    // Vectors from a reference encoder that nightmare-chess.nl accepted.
    #[test]
    fn timeseal_encodes_known_vectors() {
        assert_eq!(
            hex(&timeseal_encode(b"guest", 12345, 7, b'1')),
            "d46574bbd4b1d2877aa5a3b67f61717c71a1a3beb6a3b0bc87"
        );
        assert_eq!(
            hex(&timeseal_encode(b"e2e4", 987654, 49, b'1')),
            "97c6bcb9b2aaacd9bb7574add7d8d2c27871a7607f61717cb1"
        );
    }

    #[test]
    fn timeseal_pads_full_block_when_already_aligned() {
        // "abcd" + "\x18" + "123456" + "\x19" is exactly 12 bytes, so a
        // whole 12-byte block of padding is added, plus the offset byte.
        assert_eq!(timeseal_encode(b"abcd", 123456, 0, b'1').len(), 25);
    }

    #[test]
    fn escapes_control_and_high_bytes() {
        assert_eq!(
            escape_bytes(b"fics% \n\r\xff\xfb\x01a\"b\\"),
            "fics% \\n\\r\\xff\\xfb\\x01a\\\"b\\\\"
        );
    }
}
