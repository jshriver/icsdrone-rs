//! ICS (Internet Chess Server) connection handling. Replaces net.c's
//! OpenTCP/SendToIcs/ProcessRawInput. We speak plain TCP (no timeseal in
//! this MVP) and strip the small number of Telnet IAC sequences FICS
//! sends, same as the original's TS_NONE/TS_IAC/TS_CMD state machine.

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::debug;

const IAC: u8 = 255;
const WILL: u8 = 251;
const WONT: u8 = 252;
const DO: u8 = 253;
const DONT: u8 = 254;

pub struct IcsConn {
    stream: TcpStream,
    /// Raw bytes read but not yet split into lines.
    pending: Vec<u8>,
}

impl IcsConn {
    /// Connect to the ICS.
    pub async fn connect(host: &str, port: u16) -> Result<Self> {
        let stream = TcpStream::connect((host, port))
            .await
            .with_context(|| format!("failed to connect to {host}:{port}"))?;

        Ok(IcsConn {
            stream,
            pending: Vec::new(),
        })
    }

    pub async fn send(&mut self, s: &str) -> Result<()> {
        debug!("-> ics: {}", s.trim_end());
        let mut buf = s.as_bytes().to_vec();
        if !buf.ends_with(b"\n") {
            buf.push(b'\n');
        }
        self.stream.write_all(&buf).await?;
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
            anyhow::bail!("ICS connection closed");
        }
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
