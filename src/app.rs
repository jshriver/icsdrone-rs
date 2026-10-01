//! Ties the ICS connection and the UCI engine together. Replaces the
//! login sequence in main() and the ProcessIcsLine/ProcessComputerLine
//! dispatch of the original.

use anyhow::Result;
use std::io::Write as _;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::board::{Relation, Style12};
use crate::book::OpeningBook;
use crate::config::{Config, ConfigFile, KibitzMode};
use crate::engine::{SearchResult, UciEngine};
use crate::ics::IcsConn;

pub struct App {
    ics: IcsConn,
    engine: UciEngine,
    /// Polyglot opening book, if one was configured. Consulted before
    /// the engine each turn in `handle_style12`.
    book: Option<OpeningBook>,
    handle: String,
    game_number: Option<i32>,
    we_are_white: Option<bool>,
    /// Name of a challenger whose "Challenge: ..." line we've seen but
    /// whose "you can accept/decline" confirmation line hasn't arrived
    /// yet. Mirrors the original's `parsingIncoming` + `name` state in
    /// fics.c's ProcessMatch.
    pending_challenger: Option<String>,
    kibitz_mode: KibitzMode,
    /// Whether the console board is ANSI-colored (highlighting the
    /// previous move's from/to squares) or plain. `ColorBoard` in
    /// config.json, default plain - see
    /// `ConfigFile::resolve_color_board`.
    color_board: bool,
    /// Search-stats line from the most recent kibitz/whisper (see
    /// `format_kibitz`), shown under the board on the following
    /// redraw so the operator can see it without scrolling back.
    /// Reset to `None` at the start of each new game (`reset_game`) -
    /// there's no "last move" to have kibitzed about yet.
    last_kibitz: Option<String>,
}

impl App {
    pub async fn connect_and_login(config: &Config) -> Result<Self> {
        // Connection details (Host/Port/Username/Password) and the
        // engine/book settings all live in the JSON config file now,
        // so it has to be loaded before we can connect at all.
        let config_file = ConfigFile::load(&config.config)?;

        let host = config_file.resolve_host();
        let port = config_file.resolve_port();
        let handle = config_file.resolve_username();
        let password = config_file.resolve_password();

        let mut ics = IcsConn::connect(
            &host,
            port,
            config.debug.as_deref(),
            config_file.resolve_timeseal(),
        )
        .await?;

        info!("Connecting to {}:{} as {}", host, port, handle);

        // Log in. Mirrors main()'s `SendToIcs("%s\n%s\n\n", handle, passwd)`.
        // If no password was supplied we log in as guest and just send
        // blank lines through any guest prompts.
        ics.send(&handle).await?;
        if let Some(pw) = &password {
            ics.send_secret(pw).await?;
        } else {
            ics.send("").await?; // guest login / "press enter"
        }
        ics.send("").await?;

        // Ask the server for machine-parseable board updates and quiet
        // down chatter, same intent as the big SendToIcs(...) block in
        // main.c, trimmed to what the MVP core loop needs.
        ics.send(
            "set style 12\nset shout 0\nset cshout 0\nset seek 0\nset width 240\n\
             iset nowrap 1\niset movecase 1\n",
        )
        .await?;

        if !config_file.engine_options.is_empty() {
            info!(
                "Loaded {} engine option(s) from {}",
                config_file.engine_options.len(),
                config.config.display()
            );
        }

        // Ponder/OwnBook/NNUE (default false/false/true) plus whatever
        // was listed explicitly under engine_options.
        let engine_options = config_file.resolve_engine_options();

        let book = match config_file.resolve_book_path() {
            Some(path) => {
                info!("Loading opening book: {}", path.display());
                Some(OpeningBook::load(path)?)
            }
            None => {
                info!("No opening book configured");
                None
            }
        };

        let engine_cmd = config_file.resolve_engine();
        info!("Spawning engine: {}", engine_cmd);
        let engine = UciEngine::spawn(&engine_cmd, &engine_options).await?;

        Ok(App {
            ics,
            engine,
            book,
            handle,
            game_number: None,
            we_are_white: None,
            pending_challenger: None,
            kibitz_mode: config_file.resolve_kibitz(),
            color_board: config_file.resolve_color_board(),
            last_kibitz: None,
        })
    }

    /// Main event loop: read ICS lines, react to style12 board updates
    /// by asking the engine for a move and sending it back. Equivalent
    /// to MainLoop()'s select() over the ICS and engine file
    /// descriptors, simplified since in this MVP we only need to react
    /// to the engine when it's actually our move (no ponder/analysis).
    pub async fn run(&mut self) -> Result<()> {
        let mut stdin_rx = spawn_stdin_reader();
        // Flips to false once stdin hits EOF (e.g. running under a
        // supervisor with no attached terminal), so we stop polling a
        // permanently-closed channel instead of busy-looping on it.
        let mut stdin_open = true;

        loop {
            tokio::select! {
                line = self.ics.read_line() => {
                    let line = line?;
                    self.handle_ics_line(&line).await?;
                }
                cmd = stdin_rx.recv(), if stdin_open => {
                    match cmd {
                        Some(cmd) => {
                            if self.handle_stdin_command(&cmd).await? {
                                // Operator typed quit/exit - stop the
                                // event loop so main() can proceed to
                                // shutdown() and close things down
                                // cleanly (engine quit, etc.) instead
                                // of the process just being killed.
                                return Ok(());
                            }
                        }
                        None => stdin_open = false,
                    }
                }
            }
        }
    }

    async fn handle_ics_line(&mut self, line: &str) -> Result<()> {
        if line.trim().is_empty() {
            return Ok(());
        }
        tracing::debug!("<- ics: {}", line);

        // Show the operator what the server actually said. The one
        // exception is the raw "<12> ..." style12 board dump: it's a
        // long space-separated token line meant for programs, not
        // people, and we already surface game events (moves, game
        // start/end) separately - printing it too is just noise.
        if !line.contains("<12>") {
            println!("{}", line.trim_end());
        }

        if line.contains("<12>") {
            self.handle_style12(line).await?;
        } else if let Some(name) = parse_challenge_name(line) {
            // "Challenge: name (rating) [color] name2 (rating2) rated
            //  variant time inc." - just remember who's challenging;
            // the actual accept/decline happens on the confirmation
            // line below. Mirrors fics.c setting parsingIncoming=TRUE.
            info!("Incoming challenge from {}", name);
            self.pending_challenger = Some(name);
        } else if self.pending_challenger.is_some()
            && line.contains("accept")
            && line.contains("decline")
        {
            // The "You can \"accept\" or \"decline\" ..." line.
            // This MVP always accepts (no matchFilter/variant/noplay
            // checks yet, unlike fics.c's InjectChallenge path).
            if let Some(name) = self.pending_challenger.take() {
                info!("Auto-accepting challenge from {}", name);
                self.ics.send(&format!("accept {name}")).await?;
            }
        } else if is_game_over_line(line) {
            // Covers checkmate, resignation, stalemate, draws, and
            // adjournment - any "{Game N (...) ...} <result>" line,
            // e.g. "{Game 1 (jshriver vs. Erebus) jshriver
            // checkmated} 0-1". The original's ProcessIcsLine
            // handles each ending as a distinct case (SendMoveToComputer
            // "result", adjourn bookkeeping, etc.); for this MVP we
            // just need to know the game is over so we stop treating
            // any further style12 lines as belonging to it.
            info!("Game ended: {}", line.trim());
            self.reset_game();
        } else if line.contains("no longer") && line.contains("observing") {
            // benign
        } else {
            // Everything else (chat, seek ads, etc.) is out of scope
            // for the core loop; just log it.
        }

        Ok(())
    }

    /// Handle one line typed at the interactive `>` prompt. `quit` or
    /// `exit` (case-insensitive) logs off the ICS and asks `run()` to
    /// stop the event loop so the program can shut down cleanly rather
    /// than being killed; returns `Ok(true)` in that case. A bare line
    /// otherwise is sent to the ICS verbatim, same as typing it into
    /// any other ICS client (e.g. "tell someone hi", "abort", "kibitz
    /// nice game"). A line prefixed with `engine ` instead goes
    /// straight to the UCI engine's stdin unmodified (e.g. "engine
    /// setoption name Hash value 2048", "engine go depth 20") for
    /// poking at it directly.
    async fn handle_stdin_command(&mut self, cmd: &str) -> Result<bool> {
        if is_quit_command(cmd) {
            info!("Quit requested at prompt, logging off and shutting down");
            // Best-effort: the ICS may already be gone, or may drop us
            // the moment it sees this, so don't fail the whole shutdown
            // over an error sending it.
            let _ = self.ics.send("quit").await;
            return Ok(true);
        }

        if let Some(rest) = cmd.strip_prefix("engine ") {
            info!("-> engine (manual): {}", rest);
            self.engine.send_raw(rest).await?;
        } else {
            info!("-> ics (manual): {}", cmd);
            self.ics.send(cmd).await?;
        }
        Ok(false)
    }

    fn reset_game(&mut self) {
        self.game_number = None;
        self.we_are_white = None;
        self.last_kibitz = None;
    }

    /// Combines the `White`/`Black`/`Move`/`Clock`/`Kibitz` header with the board
    /// render into what actually gets printed, prefixed with an ANSI
    /// "clear screen, cursor home" escape only when `ansi_ok` is true.
    /// Callers pass `self.color_board` for `ansi_ok`: if a terminal
    /// can't handle the square-highlighting escapes `to_board_string`
    /// emits (that's what `ColorBoard: "No"` is for), it can't handle
    /// *any* ANSI escape sequence, clear-screen included - so
    /// `ColorBoard: "No"` needs to suppress this prefix too, not just
    /// the highlighting, or the console never actually becomes free of
    /// ANSI codes.
    fn board_frame(header: &str, board_str: &str, ansi_ok: bool) -> String {
        if ansi_ok {
            format!("\x1B[2J\x1B[1;1H{header}\n\n{board_str}")
        } else {
            format!("{header}\n\n{board_str}")
        }
    }

    async fn handle_style12(&mut self, line: &str) -> Result<()> {
        let board = match Style12::parse(line) {
            Ok(b) => b,
            Err(e) => {
                warn!("failed to parse style12 line: {e}");
                return Ok(());
            }
        };

        match board.relation {
            Relation::PlayingMyMove | Relation::PlayingOpponentMove => {}
            _ => return Ok(()), // not a game we're actively playing
        }

        // Detect a new game (game number changed) and reset our
        // tracking - before the board print below, so a fresh game's
        // first board doesn't show a stale kibitz line left over from
        // the previous game.
        if self.game_number != Some(board.game_number) {
            info!(
                "New game #{}: {} (white) vs {} (black)",
                board.game_number, board.white_name, board.black_name
            );
            self.game_number = Some(board.game_number);
            self.we_are_white = Some(board.white_name == self.handle);
            self.last_kibitz = None;
        }

        // Every style12 line for a game we're playing describes the
        // board right after a ply was made - by us or by the opponent,
        // whichever relation above matched. Show it to the operator
        // either way, so they can follow the game locally without
        // needing a separate ICS client observing it.
        let mut header = format!(
            "White: {}  Black: {}\nMove: {}\nClock: White {}  Black {}",
            board.white_name,
            board.black_name,
            board.last_move_verbose,
            format_clock(board.white_time_ms),
            format_clock(board.black_time_ms)
        );
        if let Some(kibitz) = &self.last_kibitz {
            header.push_str(&format!("\nKibitz: {kibitz}"));
        }

        let board_str = board.to_board_string(self.color_board);
        println!("{}", Self::board_frame(&header, &board_str, self.color_board));

        if !matches!(board.relation, Relation::PlayingMyMove) {
            return Ok(()); // opponent to move (or we just moved), nothing to do
        }

        // Style12 always describes the *current* full board, so we can
        // hand the engine a fresh FEN each turn instead of maintaining
        // our own move list - simpler than the original's incremental
        // SendMovesToComputer/SendBoardToComputer bookkeeping.
        let fen = board.to_fen();

        // If we have an opening book and it has a move for this exact
        // position, play that instead of asking the engine to search -
        // this is the whole point of a book (instant, known-good
        // opening play). Once the position falls out of book (None),
        // every subsequent move this game goes back to the engine as
        // usual, since a Polyglot book has no concept of "resume
        // search from here".
        if let Some(book_move) = self.book.as_ref().and_then(|b| b.best_move_from_fen(&fen)) {
            info!("Book move: {}", book_move);
            self.ics.send(&to_ics_move(&book_move)).await?;
            return Ok(());
        }

        self.engine.set_position(&fen, &[]).await?;

        let winc = board.increment_seconds as i64 * 1000;
        let game_number = board.game_number;
        let result = match self
            .search_or_abort(game_number, board.white_time_ms, board.black_time_ms, winc)
            .await?
        {
            Some(result) => result,
            // Game ended (opponent flagged/resigned/aborted, etc.)
            // while the engine was still thinking - search_or_abort
            // has already stopped the engine and reset our game
            // tracking, so there's no move left to send.
            None => return Ok(()),
        };

        info!("Engine plays: {}", result.bestmove);

        // Announce the search stats behind this move, if enabled -
        // mirrors what a human kibitzing their own analysis would post,
        // e.g. "depth=17 score=1.87 time=8.96 node=17234760
        // nps=1923522 pv=O-O g3 Re8 ...". Sent *before* the move itself
        // so observers see the reasoning land right alongside it. Also
        // echoed to our own console: the ICS whisper is only visible to
        // observers of the game, so without this the operator running
        // the bot would never see it unless they were also observing
        // from a separate ICS client. `last_kibitz` is remembered
        // (regardless of `Kibitz`/whisper being on) so the next board
        // redraw's "Kibitz:" header line has it even if whispering to
        // ICS is turned off.
        if let Some(info) = result.info.as_ref() {
            let stats = info.format_kibitz();
            self.last_kibitz = Some(stats.clone());
            if let Some(cmd) = self.kibitz_mode.ics_command() {
                println!("[kibitz] {stats}");
                self.ics.send(&format!("{cmd} {stats}")).await?;
            }
        }

        self.ics.send(&to_ics_move(&result.bestmove)).await?;

        Ok(())
    }

    /// Runs the engine search for the current move, racing it against
    /// further ICS lines: if a "game over" line for `game_number`
    /// arrives before the engine finishes thinking (we flagged, the
    /// opponent resigned, the game was aborted, etc.), the search is
    /// stopped via `UciEngine::stop` instead of playing a move into a
    /// game that's already over, and `Ok(None)` is returned. Otherwise
    /// returns `Ok(Some(result))` once the engine settles on a move.
    ///
    /// Any other line seen while thinking (chat, other games, etc.) is
    /// echoed the same way the main loop does but not otherwise acted
    /// on - full dispatch resumes once this move is decided, same as
    /// before this method existed.
    async fn search_or_abort(
        &mut self,
        game_number: i32,
        white_time_ms: i64,
        black_time_ms: i64,
        increment_ms: i64,
    ) -> Result<Option<SearchResult>> {
        let marker = format!("Game {game_number} ");
        let mut search = Box::pin(self.engine.go_and_wait(
            white_time_ms,
            black_time_ms,
            increment_ms,
            increment_ms,
        ));

        loop {
            tokio::select! {
                result = &mut search => return Ok(Some(result?)),
                line = self.ics.read_line() => {
                    let line = line?;
                    if !line.trim().is_empty() && !line.contains("<12>") {
                        println!("{}", line.trim_end());
                    }
                    if is_game_over_line(&line) && line.contains(&marker) {
                        info!("Game {} ended while engine was thinking - aborting search: {}", game_number, line.trim());
                        // Drop the in-progress search first: it holds
                        // the engine's only &mut borrow, and stop()
                        // needs one of its own to send "stop" and
                        // drain the resulting bestmove.
                        drop(search);
                        if let Err(e) = self.engine.stop().await {
                            warn!("failed to abort engine search cleanly: {e}");
                        }
                        self.reset_game();
                        return Ok(None);
                    }
                }
            }
        }
    }

    pub async fn shutdown(self) -> Result<()> {
        self.engine.quit().await
    }
}

/// Spawn a background task that prints a `> ` prompt, reads lines typed
/// on stdin, and forwards each non-empty one down the returned channel.
/// Kept as a separate task (rather than reading stdin inline in `run`)
/// so it doesn't block the select loop between keystrokes. Ends cleanly
/// on EOF (e.g. stdin redirected from /dev/null under a supervisor)
/// rather than erroring.
fn spawn_stdin_reader() -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        loop {
            print!("> ");
            if std::io::stdout().flush().is_err() {
                break;
            }
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    if tx.send(line).is_err() {
                        break; // App has shut down; nothing left to send to.
                    }
                }
                Ok(None) => break, // EOF
                Err(e) => {
                    warn!("stdin read error: {e}");
                    break;
                }
            }
        }
    });
    rx
}

/// Remaining clock time as "m:ss", or "h:mm:ss" from an hour up.
/// Style12 clocks can go negative when a player overstays their time
/// before the flag is called, so keep the sign rather than hiding it.
fn format_clock(ms: i64) -> String {
    let sign = if ms < 0 { "-" } else { "" };
    let secs = ms.abs() / 1000;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{sign}{h}:{m:02}:{s:02}")
    } else {
        format!("{sign}{m}:{s:02}")
    }
}

/// True for the `>` prompt's own quit command - "quit" or "exit",
/// case-insensitive, with no extra arguments. Deliberately exact (not
/// just a prefix check) so it can't misfire on an ICS command that
/// happens to start the same way.
fn is_quit_command(cmd: &str) -> bool {
    cmd.eq_ignore_ascii_case("quit") || cmd.eq_ignore_ascii_case("exit")
}

/// Convert a UCI/Polyglot coordinate move into the form the ICS accepts.
/// Plain moves ("e2e4", and castling as "e1g1") are fine as-is, but a
/// promotion's bare piece suffix ("a2a1q") isn't recognized as a move
/// at all ("a2a1q: Command not found") - the ICS wants it as "a2a1=Q".
fn to_ics_move(mv: &str) -> String {
    match mv.as_bytes() {
        [_, _, _, _, piece @ (b'q' | b'r' | b'b' | b'n')] => {
            format!("{}={}", &mv[..4], piece.to_ascii_uppercase() as char)
        }
        _ => mv.to_string(),
    }
}

/// Extract the challenger's handle from a line containing a "Challenge:
/// ..." announcement, e.g. `"Challenge: jshriver (1738) [white] Erebus
/// (1798) rated blitz 5 1."` -> `Some("jshriver")`. Looks for
/// "Challenge:" anywhere in the line (rather than only at the start)
/// since the ICS sometimes runs a "fics% " prompt into the following
/// line with no newline in between.
fn parse_challenge_name(line: &str) -> Option<String> {
    let idx = line.find("Challenge:")?;
    let rest = &line[idx + "Challenge:".len()..];
    rest.trim().split_whitespace().next().map(|s| s.to_string())
}

/// True for any ICS line announcing that a game has ended: checkmate,
/// resignation, stalemate, draw, or adjournment. These always take the
/// shape `{Game N (white vs. black) <reason>} <result>` where result is
/// one of "1-0", "0-1", "1/2-1/2", or "*" (adjourned/aborted with no
/// decisive result yet).
fn is_game_over_line(line: &str) -> bool {
    if !line.contains("{Game") {
        return false;
    }
    line.contains("1-0")
        || line.contains("0-1")
        || line.contains("1/2-1/2")
        || line.contains("adjourned")
        || line.contains("aborted")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_promotions_to_ics_form() {
        assert_eq!(to_ics_move("a2a1q"), "a2a1=Q");
        assert_eq!(to_ics_move("e7e8n"), "e7e8=N");
        assert_eq!(to_ics_move("b7a8r"), "b7a8=R");
        assert_eq!(to_ics_move("h2g1b"), "h2g1=B");
    }

    #[test]
    fn leaves_non_promotion_moves_unchanged() {
        assert_eq!(to_ics_move("e2e4"), "e2e4");
        assert_eq!(to_ics_move("e1g1"), "e1g1");
        assert_eq!(to_ics_move("(none)"), "(none)");
    }

    #[test]
    fn board_frame_prefixes_clear_screen_when_ansi_ok() {
        let frame = App::board_frame("White: alice  Black: bob", "  +---+", true);
        assert_eq!(frame, "\x1B[2J\x1B[1;1HWhite: alice  Black: bob\n\n  +---+");
    }

    #[test]
    fn board_frame_omits_clear_screen_when_ansi_not_ok() {
        // ColorBoard: "No" means the terminal can't handle ANSI at
        // all, not just the square-highlighting codes - so the
        // clear-screen escape must be left out too, or the console
        // never actually becomes free of ANSI codes.
        let frame = App::board_frame("White: alice  Black: bob", "  +---+", false);
        assert_eq!(frame, "White: alice  Black: bob\n\n  +---+");
        assert!(!frame.contains('\x1B'));
    }

    #[test]
    fn detects_checkmate() {
        assert!(is_game_over_line(
            "{Game 1 (jshriver vs. Erebus) jshriver checkmated} 0-1"
        ));
    }

    #[test]
    fn detects_resignation() {
        assert!(is_game_over_line(
            "{Game 4 (Erebus vs. bob) bob resigns} 1-0"
        ));
    }

    #[test]
    fn detects_draw() {
        assert!(is_game_over_line(
            "{Game 7 (a vs. b) Game drawn by mutual agreement} 1/2-1/2"
        ));
    }

    #[test]
    fn detects_adjournment() {
        assert!(is_game_over_line(
            "{Game 2 (a vs. b) a has adjourned} *"
        ));
    }

    #[test]
    fn ignores_unrelated_lines() {
        assert!(!is_game_over_line("fics% hello there"));
        assert!(!is_game_over_line(
            "Movelist for game 1: {Game 1 (a vs. b) Move list}"
        ));
    }

    #[test]
    fn parses_challenge_name_with_color() {
        assert_eq!(
            parse_challenge_name(
                "Challenge: jshriver (1738) [white] Erebus (1798) rated blitz 5 1."
            ),
            Some("jshriver".to_string())
        );
    }

    #[test]
    fn parses_challenge_name_without_color() {
        assert_eq!(
            parse_challenge_name("Challenge: someguy (1500) Erebus (1798) unrated blitz 5 0."),
            Some("someguy".to_string())
        );
    }

    #[test]
    fn ignores_non_challenge_lines() {
        assert_eq!(parse_challenge_name("fics% hello there"), None);
    }

    #[test]
    fn formats_clock_times() {
        assert_eq!(format_clock(0), "0:00");
        assert_eq!(format_clock(65_000), "1:05");
        assert_eq!(format_clock(300_000), "5:00");
        assert_eq!(format_clock(3_725_000), "1:02:05");
        assert_eq!(format_clock(-3_000), "-0:03");
    }

    #[test]
    fn parses_challenge_with_prompt_prefix() {
        assert_eq!(
            parse_challenge_name("fics% Challenge: bob (1600) Erebus (1798) rated blitz 5 0."),
            Some("bob".to_string())
        );
    }

    #[test]
    fn recognizes_quit_and_exit_case_insensitively() {
        assert!(is_quit_command("quit"));
        assert!(is_quit_command("Quit"));
        assert!(is_quit_command("QUIT"));
        assert!(is_quit_command("exit"));
        assert!(is_quit_command("Exit"));
    }

    #[test]
    fn does_not_treat_other_commands_as_quit() {
        assert!(!is_quit_command("quitters never win"));
        assert!(!is_quit_command("tell someone quit"));
        assert!(!is_quit_command("exiting"));
        assert!(!is_quit_command(""));
    }
}
