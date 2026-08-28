//! UCI engine process management. Replaces computer.c's xboard/CECP
//! handling (StartComputer, SendMoveToComputer, ProcessComputerLine,
//! the "feature" negotiation, etc.) with the UCI handshake and
//! position/go/bestmove cycle.

use anyhow::{anyhow, Context, Result};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::{debug, info, warn};

pub struct UciEngine {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pub name: Option<String>,
}

/// How long `stop()` waits for the engine's post-abort "bestmove" line
/// before giving up on draining it. Generous, since engines can take a
/// moment to unwind a deep search, but bounded so a wedged engine can't
/// hang the caller forever.
const STOP_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// The result of a completed "go" search.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub bestmove: String,
    /// The engine's expected reply to `bestmove`, if it sent one.
    /// Parsed for protocol completeness; not used yet since the app
    /// doesn't do UCI pondering (starting the next search early)
    /// today - #[allow(dead_code)] to keep the build warning-free
    /// until that lands.
    #[allow(dead_code)]
    pub ponder: Option<String>,
    /// Stats from the last "info" line seen before bestmove (i.e. the
    /// deepest completed iteration), if the engine sent any. Used to
    /// kibitz a summary of the search, e.g. "depth=17 score=1.87
    /// time=8.96 node=17234760 nps=1923522 pv=...".
    pub info: Option<EngineInfo>,
}

/// Parsed fields from one UCI "info depth ... score ... nodes ... nps
/// ... time ... pv ..." line. All fields but `depth` are optional since
/// not every engine sends every field on every line.
#[derive(Debug, Clone, Default)]
pub struct EngineInfo {
    pub depth: u32,
    pub score_cp: Option<i64>,
    pub score_mate: Option<i32>,
    pub nodes: Option<u64>,
    pub nps: Option<u64>,
    pub time_ms: Option<u64>,
    pub pv: Option<String>,
}

impl EngineInfo {
    /// Parse a single "info ..." line. Returns None for lines that
    /// aren't a depth-bearing info line (e.g. "info string ...", which
    /// is handled separately in `read_line`).
    fn parse(line: &str) -> Option<Self> {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.first() != Some(&"info") {
            return None;
        }

        let mut info = EngineInfo::default();
        let mut i = 1;
        while i < tokens.len() {
            match tokens[i] {
                "depth" => {
                    if let Some(d) = tokens.get(i + 1).and_then(|s| s.parse().ok()) {
                        info.depth = d;
                    }
                    i += 2;
                }
                "score" => match tokens.get(i + 1).copied() {
                    Some("cp") => {
                        info.score_cp = tokens.get(i + 2).and_then(|s| s.parse().ok());
                        i += 3;
                    }
                    Some("mate") => {
                        info.score_mate = tokens.get(i + 2).and_then(|s| s.parse().ok());
                        i += 3;
                    }
                    _ => i += 1,
                },
                "nodes" => {
                    info.nodes = tokens.get(i + 1).and_then(|s| s.parse().ok());
                    i += 2;
                }
                "nps" => {
                    info.nps = tokens.get(i + 1).and_then(|s| s.parse().ok());
                    i += 2;
                }
                "time" => {
                    info.time_ms = tokens.get(i + 1).and_then(|s| s.parse().ok());
                    i += 2;
                }
                // "pv" is always the last field per the UCI spec - the
                // rest of the line is the move list.
                "pv" => {
                    info.pv = Some(tokens[i + 1..].join(" "));
                    break;
                }
                _ => i += 1,
            }
        }

        // A line with no depth isn't useful to us (e.g. a bare "info
        // currmove ..." during move ordering).
        if info.depth == 0 {
            None
        } else {
            Some(info)
        }
    }

    /// Format as a one-line kibitz/whisper summary, e.g.
    /// "depth=17 score=1.87 time=8.96 node=17234760 nps=1923522 pv=O-O g3 Re8 ...".
    /// Mirrors the field names/units engines already print in their own
    /// "info" line (nps, not "speed"), just with score in pawns and
    /// time in seconds for readability.
    pub fn format_kibitz(&self) -> String {
        let score = match (self.score_mate, self.score_cp) {
            (Some(m), _) if m >= 0 => format!("M{m}"),
            (Some(m), _) => format!("-M{}", -m),
            (None, Some(cp)) => format!("{:.2}", cp as f64 / 100.0),
            (None, None) => "?".to_string(),
        };
        let time_s = self.time_ms.unwrap_or(0) as f64 / 1000.0;
        format!(
            "depth={} score={} time={:.2} node={} nps={} pv={}",
            self.depth,
            score,
            time_s,
            self.nodes.unwrap_or(0),
            self.nps.unwrap_or(0),
            self.pv.as_deref().unwrap_or("")
        )
    }
}

impl UciEngine {
    /// Spawn the engine process and run the UCI handshake:
    /// `uci` -> ... -> `uciok`, then apply `options` via `setoption`
    /// (e.g. Hash/Threads/SyzygyPath from config.json), then `isready`
    /// -> `readyok`. Equivalent to StartComputer() sending
    /// "xboard"/"protover 2" and waiting for "feature ... done=1" in the
    /// original, plus applying any options the original would have set
    /// via its own config mechanism.
    pub async fn spawn(command_line: &str, options: &BTreeMap<String, String>) -> Result<Self> {
        let mut parts = command_line.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| anyhow!("empty engine command"))?;
        let args: Vec<&str> = parts.collect();

        info!("Starting engine: {}", command_line);
        let mut child = Command::new(program)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn engine `{program}`"))?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stdout = BufReader::new(stdout);

        // Engines occasionally write diagnostics to stderr (e.g.
        // Stockfish logging tablebase loading). Previously this was
        // Stdio::inherit()'d straight to the terminal, which meant raw,
        // unlabeled, untimestamped text could show up interleaved with
        // the interactive prompt and everything else. Route it through
        // tracing instead, clearly tagged, so it's visible but doesn't
        // look like it came from the ICS or the prompt.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => warn!("[engine stderr] {}", line),
                        Ok(None) => break,
                        Err(e) => {
                            warn!("error reading engine stderr: {e}");
                            break;
                        }
                    }
                }
            });
        }

        let mut engine = UciEngine {
            child,
            stdin,
            stdout,
            name: None,
        };

        engine.send("uci").await?;
        loop {
            let line = engine.read_line().await?;
            if let Some(name) = line.strip_prefix("id name ") {
                engine.name = Some(name.trim().to_string());
                info!("Engine identifies as: {}", name.trim());
            } else if line.trim() == "uciok" {
                break;
            }
        }

        // Apply configured UCI options (Hash, Threads, SyzygyPath, or
        // anything else the engine supports) before the first isready,
        // so they're in effect for the whole session.
        for (name, value) in options {
            info!("Setting engine option {} = {}", name, value);
            engine.set_option(name, value).await?;
        }

        engine.send("isready").await?;
        loop {
            let line = engine.read_line().await?;
            if line.trim() == "readyok" {
                break;
            }
        }

        engine.send("ucinewgame").await?;
        engine.send("isready").await?;
        loop {
            let line = engine.read_line().await?;
            if line.trim() == "readyok" {
                break;
            }
        }

        Ok(engine)
    }

    async fn send(&mut self, line: &str) -> Result<()> {
        debug!("-> engine: {}", line);
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read_line(&mut self) -> Result<String> {
        let mut buf = String::new();
        let n = self.stdout.read_line(&mut buf).await?;
        if n == 0 {
            return Err(anyhow!("engine closed stdout (process likely exited)"));
        }
        let trimmed = buf.trim_end();
        debug!("<- engine: {}", trimmed);
        // "info string" is the UCI channel engines use for one-off
        // human-readable diagnostics (e.g. Stockfish's "Loaded N Syzygy
        // file(s)..."), as opposed to "info depth ... score ... pv ..."
        // search progress, which fires many times per second and would
        // flood the console if surfaced the same way. Show only the
        // former, clearly tagged so it doesn't look like ICS or prompt
        // output.
        if let Some(rest) = trimmed.strip_prefix("info string ") {
            info!("[engine] {}", rest);
        }
        Ok(buf)
    }

    /// Send `setoption name <name> value <value>` to the engine, e.g.
    /// for Hash, Threads, or SyzygyPath. Can also be used later for
    /// interactive setoption commands typed at the `>` prompt.
    pub async fn set_option(&mut self, name: &str, value: &str) -> Result<()> {
        self.send(&format!("setoption name {name} value {value}"))
            .await
    }

    /// Send a raw line straight to the engine's stdin, unmodified. Used
    /// for interactive `engine <command>` input at the `>` prompt so the
    /// operator can poke the engine directly (e.g. "engine go depth 20").
    pub async fn send_raw(&mut self, line: &str) -> Result<()> {
        self.send(line).await
    }

    /// Set the position via FEN plus a list of moves already played from
    /// that FEN (empty for "just this position"). Mirrors
    /// SendBoardToComputer + SendMovesToComputer.
    pub async fn set_position(&mut self, fen: &str, moves: &[String]) -> Result<()> {
        let cmd = if moves.is_empty() {
            format!("position fen {fen}")
        } else {
            format!("position fen {fen} moves {}", moves.join(" "))
        };
        self.send(&cmd).await
    }

    /// Start a timed search and wait for `bestmove`. Mirrors Go() +
    /// SendTimeToComputer() + waiting for the "move"/"<n> ... move" line
    /// in ProcessComputerLine.
    pub async fn go_and_wait(
        &mut self,
        white_time_ms: i64,
        black_time_ms: i64,
        winc_ms: i64,
        binc_ms: i64,
    ) -> Result<SearchResult> {
        let cmd = format!(
            "go wtime {} btime {} winc {} binc {}",
            white_time_ms.max(0),
            black_time_ms.max(0),
            winc_ms.max(0),
            binc_ms.max(0)
        );
        self.send(&cmd).await?;

        let mut last_info: Option<EngineInfo> = None;
        loop {
            let line = self.read_line().await?;
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("bestmove ") {
                let mut parts = rest.split_whitespace();
                let bestmove = parts
                    .next()
                    .ok_or_else(|| anyhow!("bestmove line missing move"))?
                    .to_string();
                let ponder = parts.nth(1).map(|s| s.to_string()); // skip the literal "ponder"
                return Ok(SearchResult {
                    bestmove,
                    ponder,
                    info: last_info,
                });
            }
            // info lines (depth/score/pv/etc.) are logged but otherwise
            // ignored in this MVP - feedback/resign heuristics come
            // later. We do keep the latest parsed one around so the
            // caller can kibitz a summary of the search once it's done.
            if line.starts_with("info ") {
                debug!("engine info: {}", line);
                if let Some(parsed) = EngineInfo::parse(line) {
                    last_info = Some(parsed);
                }
            }
        }
    }

    /// Tell the engine to stop searching immediately (e.g. because the
    /// opponent flagged, resigned, or the game otherwise ended while we
    /// were still thinking) and drain the "bestmove ..." line the UCI
    /// spec requires the engine to send in response, so the next
    /// `go_and_wait` doesn't pick up that stale line as its own result.
    /// Best-effort: an engine that's wedged and never responds to
    /// "stop" only costs us up to `STOP_DRAIN_TIMEOUT`, not a hang.
    pub async fn stop(&mut self) -> Result<()> {
        self.send("stop").await?;
        match tokio::time::timeout(STOP_DRAIN_TIMEOUT, self.drain_until_bestmove()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                warn!(
                    "engine didn't respond to \"stop\" within {:?}; proceeding anyway",
                    STOP_DRAIN_TIMEOUT
                );
                Ok(())
            }
        }
    }

    /// Read and discard engine output until (and including) the next
    /// "bestmove ..." line. Used by `stop` to clear out the response an
    /// aborted search still owes us.
    async fn drain_until_bestmove(&mut self) -> Result<()> {
        loop {
            let line = self.read_line().await?;
            if line.trim().starts_with("bestmove") {
                return Ok(());
            }
        }
    }

    pub async fn quit(mut self) -> Result<()> {
        let _ = self.send("quit").await;
        match self.child.wait().await {
            Ok(status) => debug!("engine exited: {status}"),
            Err(e) => warn!("error waiting for engine exit: {e}"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a tiny scripted fake UCI engine (python3, since it's
    /// available in the test environment and easy to reason about line
    /// by line) that answers uci/isready/go/stop and logs every line it
    /// receives - in particular setoption - to `log_path` so the test
    /// can assert on what was sent and in what order.
    fn write_fake_engine() -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("icsdrone_fake_uci_engine_{}.py", std::process::id()));
        let script = r#"#!/usr/bin/env python3
import sys

log = open(sys.argv[1], "w")
for line in sys.stdin:
    line = line.strip()
    log.write(line + "\n")
    log.flush()
    if line == "uci":
        print("id name FakeEngine")
        print("uciok")
    elif line == "isready":
        print("readyok")
    elif line.startswith("go"):
        # Real engines are fast enough here that both lines are
        # typically already sitting in the pipe before a caller gets
        # around to reading them - exactly what drain_until_bestmove
        # needs to handle.
        print("info depth 1 score cp 10 nodes 100 nps 100 time 10 pv e2e4")
        print("bestmove e2e4")
    elif line == "quit":
        break
    sys.stdout.flush()
"#;
        std::fs::write(&path, script).unwrap();
        path
    }

    #[test]
    fn parses_info_line_with_cp_score() {
        let line = "info depth 17 seldepth 24 multipv 1 score cp 187 nodes 17234760 \
                     nps 1923522 tbhits 0 time 8960 pv e1g1 g6g3 f8e8 f1g2 f7f5";
        let info = EngineInfo::parse(line).expect("should parse");
        assert_eq!(info.depth, 17);
        assert_eq!(info.score_cp, Some(187));
        assert_eq!(info.score_mate, None);
        assert_eq!(info.nodes, Some(17234760));
        assert_eq!(info.nps, Some(1923522));
        assert_eq!(info.time_ms, Some(8960));
        assert_eq!(info.pv.as_deref(), Some("e1g1 g6g3 f8e8 f1g2 f7f5"));
    }

    #[test]
    fn parses_info_line_with_mate_score() {
        let line = "info depth 12 score mate 3 nodes 500000 nps 900000 time 500 pv d1h5 g7g6 h5h6";
        let info = EngineInfo::parse(line).expect("should parse");
        assert_eq!(info.score_mate, Some(3));
        assert_eq!(info.score_cp, None);
    }

    #[test]
    fn ignores_non_info_and_depthless_lines() {
        assert!(EngineInfo::parse("bestmove e2e4").is_none());
        assert!(EngineInfo::parse("info string Loaded tablebases").is_none());
        assert!(EngineInfo::parse("info currmove e2e4 currmovenumber 1").is_none());
    }

    #[test]
    fn formats_kibitz_line_matching_engine_field_names() {
        let info = EngineInfo {
            depth: 17,
            score_cp: Some(187),
            score_mate: None,
            nodes: Some(17234760),
            nps: Some(1923522),
            time_ms: Some(8960),
            pv: Some("e1g1 g6g3 f8e8 f1g2 f7f5".to_string()),
        };
        assert_eq!(
            info.format_kibitz(),
            "depth=17 score=1.87 time=8.96 node=17234760 nps=1923522 pv=e1g1 g6g3 f8e8 f1g2 f7f5"
        );
    }

    #[test]
    fn formats_kibitz_line_with_mate_score() {
        let info = EngineInfo {
            depth: 12,
            score_cp: None,
            score_mate: Some(-3),
            nodes: Some(1000),
            nps: Some(2000),
            time_ms: Some(100),
            pv: Some("h5g6".to_string()),
        };
        assert_eq!(
            info.format_kibitz(),
            "depth=12 score=-M3 time=0.10 node=1000 nps=2000 pv=h5g6"
        );
    }

    #[tokio::test]
    async fn sends_configured_options_before_first_isready() {
        let script_path = write_fake_engine();
        let log_path = std::env::temp_dir()
            .join(format!("icsdrone_fake_uci_engine_log_{}.txt", std::process::id()));

        let mut options = BTreeMap::new();
        options.insert("Hash".to_string(), "1024".to_string());
        options.insert("Threads".to_string(), "4".to_string());
        options.insert("SyzygyPath".to_string(), "/tbs".to_string());

        let command_line = format!("python3 {} {}", script_path.display(), log_path.display());
        let engine = UciEngine::spawn(&command_line, &options).await.unwrap();
        engine.quit().await.unwrap();

        let log = std::fs::read_to_string(&log_path).unwrap();
        std::fs::remove_file(&script_path).ok();
        std::fs::remove_file(&log_path).ok();

        assert!(log.contains("setoption name Hash value 1024"));
        assert!(log.contains("setoption name Threads value 4"));
        assert!(log.contains("setoption name SyzygyPath value /tbs"));

        let first_isready = log.find("isready").expect("engine never got isready");
        let hash_idx = log
            .find("setoption name Hash value 1024")
            .expect("Hash setoption not sent");
        assert!(
            hash_idx < first_isready,
            "setoption must be sent before the first isready"
        );
    }

    #[tokio::test]
    async fn stop_drains_a_bestmove_left_unread_on_the_pipe() {
        let script_path = write_fake_engine();
        let log_path = std::env::temp_dir()
            .join(format!("icsdrone_fake_uci_engine_log_{}.txt", std::process::id()));

        let command_line = format!("python3 {} {}", script_path.display(), log_path.display());
        let mut engine = UciEngine::spawn(&command_line, &BTreeMap::new()).await.unwrap();

        engine.set_position("startpos", &[]).await.unwrap();
        // Bypass go_and_wait so its "info ..." and "bestmove e2e4"
        // response sit unread in the pipe - simulating a search we
        // abandoned (e.g. because the game ended) without ever reading
        // its result.
        engine.send_raw("go wtime 60000 btime 60000").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // stop() must drain that stray "bestmove" itself.
        engine.stop().await.unwrap();

        // A fresh search should see *its own* bestmove, proving the
        // earlier one was actually drained rather than just timed out.
        let result = engine.go_and_wait(60000, 60000, 0, 0).await.unwrap();
        assert_eq!(result.bestmove, "e2e4");

        engine.quit().await.unwrap();
        std::fs::remove_file(&script_path).ok();
        std::fs::remove_file(&log_path).ok();
    }
}
