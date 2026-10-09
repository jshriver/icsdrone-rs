//! Ties the ICS connection and the UCI engine together. Replaces the
//! login sequence in main() and the ProcessIcsLine/ProcessComputerLine
//! dispatch of the original.

use anyhow::Result;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::board::{Relation, Style12};
use crate::book::OpeningBook;
use crate::config::{Config, ConfigFile, KibitzMode};
use crate::engine::{SearchResult, UciEngine};
use crate::gui::{GuiShared, LineKind, SearchView};
use crate::ics::IcsConn;
use crate::history::GameHistory;
use crate::pgn::{self, GameAnnouncement, PgnGame};
use crate::san;

/// A `go ponder` search in progress - see `App::ponder`.
struct Ponder {
    game_number: i32,
    base_fen: String,
    moves: Vec<String>,
}

/// How to (re)connect and log in to the ICS.
struct Login {
    host: String,
    port: u16,
    username: String,
    password: Option<String>,
    timeseal: bool,
    /// `--debug` transcript file, reopened (appended to) on reconnect.
    debug_log: Option<PathBuf>,
}

/// Waits between reconnect attempts after the connection drops; the
/// last one repeats until we're back.
const RECONNECT_DELAYS: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

pub struct App {
    ics: IcsConn,
    login: Login,
    engine: UciEngine,
    /// Polyglot opening book, if one was configured. Consulted before
    /// the engine each turn in `handle_style12`.
    book: Option<OpeningBook>,
    handle: String,
    game_number: Option<i32>,
    we_are_white: Option<bool>,
    /// Who we're playing in the current game, so a game adjourned by a
    /// dropped connection can be resumed with `match <opponent>`.
    opponent: Option<String>,
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
    /// The desktop window, when `GUI` is on. It replaces the terminal
    /// board; everything else still goes to the terminal as well.
    gui: Option<Arc<GuiShared>>,
    /// Typed commands (terminal prompt and GUI console), set by `run`.
    /// A field rather than a local so `search_or_abort` can keep
    /// serving them while the engine thinks.
    commands: Option<mpsc::UnboundedReceiver<String>>,
    /// Set when "quit" arrives mid-search, so `run` stops once the
    /// search has been abandoned.
    quit_requested: bool,
    /// `SavePGN`: file finished games are appended to, if set.
    save_pgn: Option<PathBuf>,
    /// ICS host, for the PGN "Site" tag.
    site: String,
    /// The game being recorded for `save_pgn`.
    pgn_game: Option<PgnGame>,
    /// UCI moves of the game we're playing, sent to the engine with
    /// every search so it can see repetitions.
    history: Option<GameHistory>,
    /// `"Ponder": "true"` under `engine_options`: think on the
    /// opponent's time about the reply the engine expects.
    ponder_enabled: bool,
    /// Set while the engine ponders: the game, and the position (as
    /// base FEN + moves) it's pondering, i.e. after our move and the
    /// opponent's expected reply.
    ponder: Option<Ponder>,
    /// The style12 line of the board we're to move on, kept until we've
    /// moved, so a move FICS rejects can be searched again.
    my_turn_line: Option<String>,
    /// Ply we already re-searched after an illegal move, so a second
    /// rejection doesn't loop.
    illegal_retry_ply: Option<usize>,
    /// The latest "Creating: ..." line, picked up by the next game's
    /// PGN for its ratings and game type.
    announcement: Option<GameAnnouncement>,
}

impl App {
    pub async fn connect_and_login(
        config: &Config,
        config_file: &ConfigFile,
        gui: Option<Arc<GuiShared>>,
    ) -> Result<Self> {
        let login = Login {
            host: config_file.resolve_host(),
            port: config_file.resolve_port(),
            username: config_file.resolve_username(),
            password: config_file.resolve_password(),
            timeseal: config_file.resolve_timeseal(),
            debug_log: config.debug.clone(),
        };
        let ics = open_ics(&login, gui.as_deref()).await?;
        let (host, port, handle) = (login.host.clone(), login.port, login.username.clone());

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
        let ponder_enabled = engine_options
            .get("Ponder")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));

        let book = match config_file.resolve_book_path() {
            Some(path) => {
                // A big book can take a while; without this the window
                // would sit on "Connecting…" the whole time.
                let size = std::fs::metadata(&path)
                    .map(|m| format!(" ({:.1} GB)", m.len() as f64 / 1e9))
                    .unwrap_or_default();
                let loading = format!("Loading opening book {}{size}…", path.display());
                if let Some(gui) = &gui {
                    gui.update(|s| s.status = loading.clone());
                }
                notify(gui.as_deref(), &loading);
                let started = Instant::now();
                let book = OpeningBook::load(path)?;
                notify(
                    gui.as_deref(),
                    &format!("Opening book loaded in {:.1}s", started.elapsed().as_secs_f64()),
                );
                Some(book)
            }
            None => {
                info!("No opening book configured");
                None
            }
        };

        let engine_cmd = config_file.resolve_engine();
        info!("Spawning engine: {}", engine_cmd);
        if let Some(gui) = &gui {
            gui.update(|s| s.status = format!("Starting engine {engine_cmd}…"));
        }
        let engine = UciEngine::spawn(&engine_cmd, &engine_options).await?;

        if let Some(gui) = &gui {
            gui.update(|s| {
                s.connected = true;
                s.status = format!("Connected to {host}:{port} as {handle}");
            });
        }

        Ok(App {
            ics,
            login,
            engine,
            book,
            handle,
            game_number: None,
            we_are_white: None,
            opponent: None,
            pending_challenger: None,
            kibitz_mode: config_file.resolve_kibitz(),
            color_board: config_file.resolve_color_board(),
            last_kibitz: None,
            gui,
            commands: None,
            quit_requested: false,
            save_pgn: config_file.resolve_save_pgn(),
            site: host,
            pgn_game: None,
            history: None,
            ponder_enabled,
            ponder: None,
            my_turn_line: None,
            illegal_retry_ply: None,
            announcement: None,
        })
    }

    /// Main event loop: read ICS lines, react to style12 board updates
    /// by asking the engine for a move and sending it back. Equivalent
    /// to MainLoop()'s select() over the ICS and engine file
    /// descriptors, simplified since in this MVP we only need to react
    /// to the engine when it's our move (and, with `Ponder` on, while
    /// it's the opponent's - see `handle_style12`).
    ///
    /// Commands arrive on `cmd_rx`: lines typed at the terminal prompt
    /// (fed in by a reader task using `cmd_tx`) and, with `GUI` on,
    /// lines typed in the window's console.
    pub async fn run(
        &mut self,
        cmd_tx: mpsc::UnboundedSender<String>,
        cmd_rx: mpsc::UnboundedReceiver<String>,
    ) -> Result<()> {
        // With the GUI on, its console is the only input; the terminal
        // isn't used at all.
        if self.gui.is_none() {
            spawn_stdin_reader(cmd_tx);
        }
        self.commands = Some(cmd_rx);

        // A dropped connection isn't the end: reconnect and carry on.
        // Any other error still is.
        loop {
            match self.event_loop().await {
                Err(e) if self.ics.is_dead() => self.reconnect(&e).await?,
                result => return result,
            }
            if self.quit_requested {
                return Ok(());
            }
        }
    }

    /// Read ICS lines and typed commands until quit (`Ok`) or an error.
    async fn event_loop(&mut self) -> Result<()> {
        loop {
            if self.quit_requested {
                return Ok(());
            }
            tokio::select! {
                line = self.ics.read_line() => {
                    let line = line?;
                    self.handle_ics_line(&line).await?;
                }
                cmd = next_command(&mut self.commands) => {
                    if self.handle_stdin_command(&cmd).await? {
                        // Operator typed quit/exit - stop the event
                        // loop so main() can proceed to shutdown() and
                        // close things down cleanly (engine quit, etc.)
                        // instead of the process just being killed.
                        return Ok(());
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
            show_server_line(self.gui.as_deref(), line);
        }

        match login_result(line) {
            Some(Ok(name)) => {
                // The server may assign a different name than we asked
                // for (FICS guests become "GuestXXXX").
                notify(self.gui.as_deref(), &format!("Logged in as {name}"));
                self.handle = name.trim_end_matches("(U)").to_string();
                if let Some(gui) = &self.gui {
                    gui.update(|s| {
                        s.status = format!("Logged in as {name}");
                        s.error = None;
                    });
                }
            }
            Some(Err(())) => {
                warn!("Login failed: {}", line.trim());
                if let Some(gui) = &self.gui {
                    let error = format!("Login failed: {}", line.trim());
                    gui.update(|s| s.error = Some(error));
                }
            }
            None => {}
        }

        if let Some(announcement) = pgn::parse_creating(line) {
            self.announcement = Some(announcement);
        }

        if line.contains("<12>") {
            self.handle_style12(line).await?;
        } else if let Some(name) = parse_challenge_name(line) {
            // "Challenge: name (rating) [color] name2 (rating2) rated
            //  variant time inc." - just remember who's challenging;
            // the actual accept/decline happens on the confirmation
            // line below. Mirrors fics.c setting parsingIncoming=TRUE.
            notify(self.gui.as_deref(), &format!("Incoming challenge from {name}"));
            self.pending_challenger = Some(name);
        } else if self.pending_challenger.is_some()
            && line.contains("accept")
            && line.contains("decline")
        {
            // The "You can \"accept\" or \"decline\" ..." line.
            // This MVP always accepts (no matchFilter/variant/noplay
            // checks yet, unlike fics.c's InjectChallenge path).
            if let Some(name) = self.pending_challenger.take() {
                notify(self.gui.as_deref(), &format!("Auto-accepting challenge from {name}"));
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
            notify(self.gui.as_deref(), &format!("Game ended: {}", line.trim()));
            self.stop_pondering().await;
            self.game_over(line);
        } else if line.contains("Illegal move") {
            self.retry_after_illegal_move().await?;
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
        echo_command(self.gui.as_deref(), cmd);
        if is_quit_command(cmd) {
            info!("Quit requested at prompt, logging off and shutting down");
            // Best-effort: the ICS may already be gone, or may drop us
            // the moment it sees this, so don't fail the whole shutdown
            // over an error sending it.
            let _ = self.ics.send("quit").await;
            return Ok(true);
        }

        if let Some(rest) = cmd.strip_prefix("engine ") {
            if self.ponder.is_some() {
                notify(
                    self.gui.as_deref(),
                    "The engine is pondering; send engine commands between games.",
                );
                return Ok(false);
            }
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
        self.opponent = None;
        self.last_kibitz = None;
        self.history = None;
        self.my_turn_line = None;
        self.illegal_retry_ply = None;
    }

    /// The game we're playing has ended on `line` ("{Game N (...) ...}
    /// result"): stop tracking it, and freeze the window's clocks with
    /// the result shown under the moves.
    fn game_over(&mut self, line: &str) {
        self.reset_game();
        self.save_game(line);
        if let Some(gui) = &self.gui {
            // Drop anything (e.g. a "fics% " prompt) run in front of it.
            let result = line.find("{Game").map_or(line, |i| &line[i..]).trim().to_string();
            gui.update(|s| s.game_over = Some(result));
        }
    }

    /// The board render (plus any kibitz line) as it actually gets
    /// printed, prefixed with an ANSI "clear screen, cursor home"
    /// escape only when `ansi_ok` is true.
    /// Callers pass `self.color_board` for `ansi_ok`: if a terminal
    /// can't handle the square-highlighting escapes `to_board_string`
    /// emits (that's what `ColorBoard: "No"` is for), it can't handle
    /// *any* ANSI escape sequence, clear-screen included - so
    /// `ColorBoard: "No"` needs to suppress this prefix too, not just
    /// the highlighting, or the console never actually becomes free of
    /// ANSI codes.
    fn board_frame(board_str: &str, ansi_ok: bool) -> String {
        if ansi_ok {
            format!("\x1B[2J\x1B[1;1H{board_str}")
        } else {
            board_str.to_string()
        }
    }

    /// With `SavePGN` set, append the recorded game that `line` ("{Game
    /// N (...) reason} result") ends to the PGN file. Games that ended
    /// before any move (e.g. aborted) aren't saved.
    fn save_game(&mut self, line: &str) {
        let (Some(path), Some(end)) = (&self.save_pgn, pgn::parse_game_end(line)) else {
            return;
        };
        let Some(game) = self.pgn_game.take_if(|g| g.game_number == end.game_number) else {
            return;
        };
        if !game.has_moves() {
            return;
        }
        match pgn::append(path, &game.to_pgn(&end)) {
            Ok(()) => notify(
                self.gui.as_deref(),
                &format!("Saved game {} to {}", end.game_number, path.display()),
            ),
            Err(e) => {
                let msg = format!("Could not save game to {}: {e}", path.display());
                warn!("{msg}");
                if let Some(gui) = &self.gui {
                    gui.console(LineKind::Error, msg);
                }
            }
        }
    }

    /// Print `board` FICS style 1 (players, last move and clocks
    /// included), and the last kibitz under it, to the terminal, plain
    /// or colored per `ColorBoard`.
    fn print_board(&self, board: &Style12) {
        let mut text = board.to_board_string(self.color_board);
        if let Some(kibitz) = &self.last_kibitz {
            text.push_str(&format!("\n\nKibitz: {kibitz}"));
        }
        println!("{}", Self::board_frame(&text, self.color_board));
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
            notify(
                self.gui.as_deref(),
                &format!(
                    "New game #{}: {} (white) vs {} (black)",
                    board.game_number, board.white_name, board.black_name
                ),
            );
            self.game_number = Some(board.game_number);
            let we_are_white = board.white_name == self.handle;
            self.we_are_white = Some(we_are_white);
            self.opponent = Some(if we_are_white {
                board.black_name.clone()
            } else {
                board.white_name.clone()
            });
            self.last_kibitz = None;
        }

        if self
            .ponder
            .as_ref()
            .is_some_and(|p| p.game_number != board.game_number)
        {
            self.stop_pondering().await;
        }

        let history = match self.history.take() {
            Some(mut history) if history.game_number == board.game_number => {
                history.record(&board);
                history
            }
            _ => GameHistory::new(&board),
        };
        let history = self.history.insert(history);
        let (base_fen, moves) = (history.base_fen().to_string(), history.moves().to_vec());

        if self.save_pgn.is_some() {
            if self.pgn_game.as_ref().map(|g| g.game_number) != Some(board.game_number) {
                let announcement = self.announcement.take();
                self.pgn_game = Some(PgnGame::new(&board, &self.site, announcement.as_ref()));
            }
            if let Some(game) = &mut self.pgn_game {
                game.record(&board);
            }
        }

        // Every style12 line for a game we're playing describes the
        // board right after a ply was made - by us or by the opponent,
        // whichever relation above matched. Show it to the operator
        // either way, so they can follow the game locally without
        // needing a separate ICS client observing it. With the GUI on,
        // the window shows it instead of the terminal.
        if let Some(gui) = &self.gui {
            gui.update(|s| s.new_board(board.clone()));
        } else {
            self.print_board(&board);
        }

        if !matches!(board.relation, Relation::PlayingMyMove) {
            self.my_turn_line = None;
            return Ok(()); // opponent to move (or we just moved), nothing to do
        }
        self.my_turn_line = Some(line.to_string());

        // The current board, for the book lookup and SAN display. The
        // engine instead gets the game's base position plus every move
        // since (`history` above) - a bare FEN would hide the earlier
        // positions from it, so it couldn't see repetitions.
        let fen = board.to_fen();
        let winc = board.increment_seconds as i64 * 1000;
        let game_number = board.game_number;
        // Our clock is running from here; the PGN notes the time we took.
        let started = Instant::now();

        // If the engine was pondering and the opponent played the
        // expected reply, that search is already well under way on
        // exactly this position: let it carry on as the real search.
        // Any other reply makes it useless, so stop it.
        let ponder_hit = match self.ponder.take() {
            Some(p) if p.base_fen == base_fen && p.moves == moves => {
                info!("Ponder hit");
                self.engine.ponderhit().await?;
                true
            }
            Some(_) => {
                info!("Ponder miss");
                if let Err(e) = self.engine.stop().await {
                    warn!("failed to stop pondering cleanly: {e}");
                }
                false
            }
            None => false,
        };

        // If we have an opening book and it has a move for this exact
        // position, play that instead of asking the engine to search -
        // this is the whole point of a book (instant, known-good
        // opening play). Once the position falls out of book (None),
        // every subsequent move this game goes back to the engine as
        // usual, since a Polyglot book has no concept of "resume
        // search from here".
        let book_move = if ponder_hit {
            None
        } else {
            self.book.as_ref().and_then(|b| b.best_move_from_fen(&fen))
        };
        if let Some(book_move) = book_move {
            info!("Book move: {}", book_move);
            if let Some(gui) = &self.gui {
                gui.update(|s| {
                    s.mark_book(board.ply() + 1);
                    s.search = Some(SearchView {
                        bestmove: san::move_to_san(&fen, &book_move),
                        from_book: true,
                        info: None,
                        pv: None,
                    })
                });
            }
            if let Some(game) = &mut self.pgn_game {
                game.annotate(board.ply() + 1, "book".to_string());
            }
            self.ics.send(&to_ics_move(&book_move)).await?;
            return Ok(());
        }

        if !ponder_hit {
            self.engine.set_position(&base_fen, &moves).await?;
            self.engine
                .go(board.white_time_ms, board.black_time_ms, winc, winc, false)
                .await?;
        }

        let result = match self.search_or_abort(game_number).await? {
            Some(result) => result,
            // Game ended (opponent flagged/resigned/aborted, etc.)
            // while the engine was still thinking - search_or_abort
            // has already stopped the engine and reset our game
            // tracking, so there's no move left to send.
            None => return Ok(()),
        };

        if result.from_book {
            info!("Engine plays from its own book: {}", result.bestmove);
        } else {
            info!("Engine plays: {}", result.bestmove);
        }
        if let Some(game) = &mut self.pgn_game {
            let note = if result.from_book {
                "book".to_string()
            } else {
                pgn::engine_note(result.info.as_ref(), started.elapsed())
            };
            game.annotate(board.ply() + 1, note);
        }

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
                match &self.gui {
                    Some(gui) => gui.console(LineKind::Kibitz, format!("[kibitz] {stats}")),
                    None => println!("[kibitz] {stats}"),
                }
                self.ics.send(&format!("{cmd} {stats}")).await?;
            }
        }
        if let Some(gui) = &self.gui {
            gui.update(|s| {
                if let Some(info) = &result.info {
                    s.push_score(&board, info);
                }
                if result.from_book {
                    s.mark_book(board.ply() + 1);
                }
                s.search = Some(SearchView {
                    bestmove: san::move_to_san(&fen, &result.bestmove),
                    from_book: result.from_book,
                    info: result.info.clone(),
                    pv: result
                        .info
                        .as_ref()
                        .and_then(|i| i.pv.as_deref())
                        .map(|pv| san::line_to_san(&fen, pv)),
                })
            });
        }

        self.ics.send(&to_ics_move(&result.bestmove)).await?;

        // Think on the opponent's time about the reply the engine
        // expects, from the position after it. Uses the clocks as of
        // this board, as UCI GUIs do; `ponderhit` turns it into our
        // next search if the opponent obliges.
        if let (true, Some(reply)) = (self.ponder_enabled, result.ponder) {
            let mut ponder_moves = moves;
            ponder_moves.push(result.bestmove);
            ponder_moves.push(reply.clone());
            info!("Pondering on {reply}");
            self.engine.set_position(&base_fen, &ponder_moves).await?;
            self.engine
                .go(board.white_time_ms, board.black_time_ms, winc, winc, true)
                .await?;
            self.ponder = Some(Ponder {
                game_number,
                base_fen,
                moves: ponder_moves,
            });
        }

        Ok(())
    }

    /// The ICS connection dropped (`error`). Stop the engine, then
    /// reconnect and log back in, waiting longer between each failed
    /// attempt, until it works or the operator quits. If we were in a
    /// game, the server will have adjourned it, so challenge the same
    /// opponent again, which resumes it.
    async fn reconnect(&mut self, error: &anyhow::Error) -> Result<()> {
        warn!("ICS connection lost: {error:#}");
        if let Some(gui) = &self.gui {
            gui.update(|s| {
                s.connected = false;
                s.status = "Connection lost - reconnecting…".to_string();
            });
        }
        notify(
            self.gui.as_deref(),
            &format!("Connection lost ({error:#}) - reconnecting"),
        );
        self.stop_pondering().await;
        let opponent = self.opponent.clone();
        self.reset_game();

        for attempt in 0.. {
            let delay = RECONNECT_DELAYS[attempt.min(RECONNECT_DELAYS.len() - 1)];
            notify(
                self.gui.as_deref(),
                &format!("Reconnecting in {}s…", delay.as_secs()),
            );
            // Keep serving typed commands while we wait, so "quit"
            // still works.
            let sleep = tokio::time::sleep(delay);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    () = &mut sleep => break,
                    cmd = next_command(&mut self.commands) => {
                        echo_command(self.gui.as_deref(), &cmd);
                        if is_quit_command(&cmd) {
                            self.quit_requested = true;
                            return Ok(());
                        }
                        notify(self.gui.as_deref(), "Not connected; command not sent.");
                    }
                }
            }

            match open_ics(&self.login, self.gui.as_deref()).await {
                Ok(ics) => {
                    self.ics = ics;
                    let (host, port) = (&self.login.host, self.login.port);
                    notify(self.gui.as_deref(), &format!("Reconnected to {host}:{port}"));
                    if let Some(gui) = &self.gui {
                        let status = format!("Connected to {host}:{port} as {}", self.handle);
                        gui.update(|s| {
                            s.connected = true;
                            s.status = status;
                        });
                    }
                    if let Some(opponent) = opponent {
                        // The server adjourns a game when a player
                        // drops, and challenging the same opponent
                        // resumes it, clocks and all. (There's no
                        // "resume" command on every server.)
                        notify(
                            self.gui.as_deref(),
                            &format!("Asking {opponent} to resume the adjourned game"),
                        );
                        self.ics.send(&format!("match {opponent}")).await?;
                    }
                    return Ok(());
                }
                Err(e) => {
                    warn!("reconnect failed: {e:#}");
                    notify(self.gui.as_deref(), &format!("Reconnect failed: {e:#}"));
                }
            }
        }
        unreachable!()
    }

    /// FICS rejected the move we just sent, and it's still our turn.
    /// Rather than sit there until our flag falls, search again once
    /// from the current board as a plain FEN - dropping the move
    /// history, in case that's what led the engine astray.
    async fn retry_after_illegal_move(&mut self) -> Result<()> {
        let Some(line) = self.my_turn_line.clone() else {
            return Ok(()); // not our move (e.g. a typed move was rejected)
        };
        let Ok(board) = Style12::parse(&line) else {
            return Ok(());
        };
        if self.illegal_retry_ply == Some(board.ply()) {
            warn!("FICS rejected our move again; not retrying");
            if let Some(gui) = &self.gui {
                gui.console(LineKind::Error, "FICS rejected our move again; not retrying.");
            }
            return Ok(());
        }
        self.illegal_retry_ply = Some(board.ply());
        notify(
            self.gui.as_deref(),
            "FICS rejected our move - searching again from the current position",
        );
        self.stop_pondering().await;
        self.history = None;
        self.handle_style12(&line).await
    }

    /// Stop a ponder search, if one is running (the game ended, or a
    /// new one started).
    async fn stop_pondering(&mut self) {
        if self.ponder.take().is_some() {
            if let Err(e) = self.engine.stop().await {
                warn!("failed to stop pondering cleanly: {e}");
            }
        }
    }

    /// Waits for the engine search for the current move (already
    /// started with `go`, or `ponderhit`), racing it against
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
    ///
    /// Typed commands are still served while thinking: ICS commands go
    /// out right away, "quit" abandons the search and quits (`Ok(None)`
    /// with `quit_requested` set), and "engine ..." is refused since
    /// the engine is mid-search.
    async fn search_or_abort(&mut self, game_number: i32) -> Result<Option<SearchResult>> {
        let marker = format!("Game {game_number} ");
        let mut search = Box::pin(self.engine.wait_bestmove());

        loop {
            tokio::select! {
                result = &mut search => return Ok(Some(result?)),
                line = self.ics.read_line() => {
                    let line = match line {
                        Ok(line) => line,
                        Err(e) => {
                            // Connection lost mid-search: there's no
                            // game to send the move to any more.
                            drop(search);
                            if let Err(e) = self.engine.stop().await {
                                warn!("failed to abort engine search cleanly: {e}");
                            }
                            return Err(e);
                        }
                    };
                    if !line.trim().is_empty() && !line.contains("<12>") {
                        show_server_line(self.gui.as_deref(), &line);
                    }
                    if is_game_over_line(&line) && line.contains(&marker) {
                        notify(
                            self.gui.as_deref(),
                            &format!("Game {} ended while engine was thinking - aborting search: {}", game_number, line.trim()),
                        );
                        // Drop the in-progress search first: it holds
                        // the engine's only &mut borrow, and stop()
                        // needs one of its own to send "stop" and
                        // drain the resulting bestmove.
                        drop(search);
                        if let Err(e) = self.engine.stop().await {
                            warn!("failed to abort engine search cleanly: {e}");
                        }
                        self.game_over(&line);
                        return Ok(None);
                    }
                }
                cmd = next_command(&mut self.commands) => {
                    echo_command(self.gui.as_deref(), &cmd);
                    if is_quit_command(&cmd) {
                        info!("Quit requested while engine was thinking - stopping search and logging off");
                        drop(search);
                        if let Err(e) = self.engine.stop().await {
                            warn!("failed to abort engine search cleanly: {e}");
                        }
                        let _ = self.ics.send("quit").await;
                        self.quit_requested = true;
                        return Ok(None);
                    } else if cmd.starts_with("engine ") {
                        notify(
                            self.gui.as_deref(),
                            "The engine is busy with a search; send engine commands after this move.",
                        );
                    } else {
                        info!("-> ics (manual): {}", cmd);
                        self.ics.send(&cmd).await?;
                    }
                }
            }
        }
    }

    pub async fn shutdown(self) -> Result<()> {
        self.engine.quit().await
    }
}

/// Connect to the ICS and log in. Mirrors main()'s `SendToIcs("%s\n%s\n\n",
/// handle, passwd)`: with no password we log in as a guest and just send
/// blank lines through any guest prompts. Used at startup and on every
/// reconnect.
async fn open_ics(login: &Login, gui: Option<&GuiShared>) -> Result<IcsConn> {
    let (host, port) = (&login.host, login.port);
    if let Some(gui) = gui {
        gui.update(|s| {
            s.status = format!("Connecting to {host}:{port}…");
            s.timeseal = login.timeseal;
        });
    }

    let mut ics =
        IcsConn::connect(host, port, login.debug_log.as_deref(), login.timeseal).await?;

    info!("Connecting to {}:{} as {}", host, port, login.username);
    ics.send(&login.username).await?;
    if let Some(pw) = &login.password {
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
    Ok(ics)
}

/// Spawn a background task that prints a `> ` prompt, reads lines typed
/// on stdin, and forwards each non-empty one down `tx`.
/// Kept as a separate task (rather than reading stdin inline in `run`)
/// so it doesn't block the select loop between keystrokes. Ends cleanly
/// on EOF (e.g. stdin redirected from /dev/null under a supervisor)
/// rather than erroring.
fn spawn_stdin_reader(tx: mpsc::UnboundedSender<String>) {
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
}

/// The next typed command. Once the sender side is gone (stdin hit EOF
/// with no GUI, e.g. under a supervisor), waits forever instead of
/// returning `None` in a busy loop.
async fn next_command(commands: &mut Option<mpsc::UnboundedReceiver<String>>) -> String {
    if let Some(rx) = commands {
        if let Some(cmd) = rx.recv().await {
            return cmd;
        }
        *commands = None;
    }
    std::future::pending().await
}

/// Show a typed command in the window's console (the terminal already
/// shows what was typed there).
fn echo_command(gui: Option<&GuiShared>, cmd: &str) {
    if let Some(gui) = gui {
        gui.console(LineKind::Sent, format!("> {cmd}"));
    }
}

/// Show a line from the ICS in the window's console, or on the
/// terminal when there's no window.
fn show_server_line(gui: Option<&GuiShared>, line: &str) {
    let line = line.trim_end();
    match gui {
        // A bare prompt is just noise in the window.
        Some(_) if line.trim() == "fics%" => {}
        Some(gui) => gui.console(LineKind::Server, line),
        None => println!("{line}"),
    }
}

/// Log a bot event (new game, game over, challenge, ...) and show it in
/// the window's console.
fn notify(gui: Option<&GuiShared>, text: &str) {
    info!("{text}");
    if let Some(gui) = gui {
        gui.console(LineKind::System, text);
    }
}

/// Remaining clock time as "m:ss", or "h:mm:ss" from an hour up.
/// Style12 clocks can go negative when a player overstays their time
/// before the flag is called, so keep the sign rather than hiding it.
pub(crate) fn format_clock(ms: i64) -> String {
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

/// Whether `line` reports the outcome of our login: `Some(Ok(name))`
/// with the name we're logged in under ("Logged in as Erebus." on
/// LaskerRevisited, "**** Starting FICS session as GuestPSPD(U) ****"
/// on FICS), `Some(Err)` for the server's rejections (wrong password,
/// unknown account, handle already in use - including the single
/// guest account), `None` for anything else.
fn login_result(line: &str) -> Option<Result<String, ()>> {
    let line = line.trim();
    let name = line
        .strip_prefix("Logged in as ")
        .map(|rest| rest.trim_end_matches('.'))
        .or_else(|| {
            let rest = &line[line.find("**** Starting FICS session as ")? + 30..];
            Some(rest.trim_end_matches('*').trim())
        });
    if let Some(name) = name {
        Some(Ok(name.to_string()))
    } else if line.starts_with("Invalid password")
        || line.starts_with("Unknown account")
        || line.contains("already logged in")
        || line.contains("already in use")
    {
        Some(Err(()))
    } else {
        None
    }
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
    fn recognizes_login_results() {
        assert_eq!(login_result("Logged in as Erebus."), Some(Ok("Erebus".to_string())));
        assert_eq!(
            login_result("**** Starting FICS session as GuestPSPD(U) ****"),
            Some(Ok("GuestPSPD(U)".to_string()))
        );
        for line in [
            "password: Invalid password.",
            "Unknown account. Use guest or try again.",
            "password: That account is already logged in.",
            "The guest account is already in use. Please use a registered account.",
        ] {
            let line = line.trim_start_matches("password: ");
            assert_eq!(login_result(line), Some(Err(())), "{line}");
        }
        assert_eq!(login_result("fics% tell bob logged in as guest"), None);
    }

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
        let frame = App::board_frame("Game 1 (alice vs. bob)", true);
        assert_eq!(frame, "\x1B[2J\x1B[1;1HGame 1 (alice vs. bob)");
    }

    #[test]
    fn board_frame_omits_clear_screen_when_ansi_not_ok() {
        // ColorBoard: "No" means the terminal can't handle ANSI at
        // all, not just the square-highlighting codes - so the
        // clear-screen escape must be left out too, or the console
        // never actually becomes free of ANSI codes.
        let frame = App::board_frame("Game 1 (alice vs. bob)", false);
        assert_eq!(frame, "Game 1 (alice vs. bob)");
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
