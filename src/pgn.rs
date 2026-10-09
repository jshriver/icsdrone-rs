//! Saving finished games as PGN (`"SavePGN": "games.pgn"` in
//! config.json). Moves are collected from the style12 board updates of
//! the game we're playing (each carries the last move in SAN), and the
//! game is appended to the file when its "{Game N ...} result" line
//! arrives.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::board::Style12;
use crate::engine::EngineInfo;

/// PGN lines are wrapped at this width (the export format's usual 80).
const LINE_WIDTH: usize = 80;

/// Put `san` into `moves` as ply number `ply` (1 = the first move
/// recorded). A repeated board (e.g. a refresh) or a takeback rewinds
/// the list first; a gap is filled with "…" placeholders. Shared with
/// the GUI's move list.
pub fn record_move(moves: &mut Vec<String>, ply: usize, san: &str) {
    if ply == 0 || san == "none" {
        return;
    }
    moves.truncate(ply - 1);
    while moves.len() < ply - 1 {
        moves.push("…".to_string());
    }
    moves.push(san.to_string());
}

/// What the server's "Creating: ..." line says about a game about to
/// start, e.g. "Creating: GriffyJr (1920) GuestHRPY (++++) unrated
/// blitz 2 12".
#[derive(Debug, Clone, PartialEq)]
pub struct GameAnnouncement {
    pub white: String,
    pub white_rating: Option<u32>,
    pub black: String,
    pub black_rating: Option<u32>,
    /// e.g. "unrated blitz"
    pub kind: String,
}

pub fn parse_creating(line: &str) -> Option<GameAnnouncement> {
    let rest = &line[line.find("Creating:")? + "Creating:".len()..];
    let mut words = rest.split_whitespace();
    let white = words.next()?.to_string();
    let white_rating = words.next()?;
    let black = words.next()?.to_string();
    let black_rating = words.next()?;
    let rated = words.next()?;
    let variant = words.next()?;
    // Guests and unrated players show "(++++)" or "(----)".
    let rating = |r: &str| r.trim_matches(|c| c == '(' || c == ')').parse().ok();
    Some(GameAnnouncement {
        white,
        white_rating: rating(white_rating),
        black,
        black_rating: rating(black_rating),
        kind: format!("{rated} {variant}"),
    })
}

/// The end of a game, from "{Game 18 (GuestJTMH vs. GuestNBPB)
/// GuestJTMH resigns} 0-1": game number, reason and result.
#[derive(Debug, Clone, PartialEq)]
pub struct GameEnd {
    pub game_number: i32,
    pub reason: String,
    pub result: String,
}

pub fn parse_game_end(line: &str) -> Option<GameEnd> {
    let rest = &line[line.find("{Game ")? + "{Game ".len()..];
    let (number, rest) = rest.split_once(' ')?;
    let (_, rest) = rest.split_once(") ")?;
    let (reason, result) = rest.split_once('}')?;
    let result = result.split_whitespace().next()?;
    if !matches!(result, "1-0" | "0-1" | "1/2-1/2" | "*") {
        return None;
    }
    Some(GameEnd {
        game_number: number.parse().ok()?,
        reason: reason.trim().to_string(),
        result: result.to_string(),
    })
}

/// A game being recorded.
pub struct PgnGame {
    pub game_number: i32,
    event: String,
    site: String,
    date: String,
    white: String,
    black: String,
    white_rating: Option<u32>,
    black_rating: Option<u32>,
    time_control: String,
    /// Set when we joined after the start (e.g. a resumed game): the
    /// position of the first board we saw, which the moves follow.
    start_fen: Option<String>,
    /// Ply of the first board we saw (0 for a game from the start).
    base_ply: usize,
    moves: Vec<String>,
    /// Clock time left (ms) after each move, by ply, for `[%clk]`.
    clocks: BTreeMap<usize, i64>,
    /// Our engine's note on each of its moves ("+0.35/18 2.1s" or
    /// "book"), by ply.
    notes: BTreeMap<usize, String>,
}

impl PgnGame {
    /// Start recording from `board`, the first board of the game we've
    /// seen. `announcement` is the matching "Creating:" line, if any.
    pub fn new(board: &Style12, site: &str, announcement: Option<&GameAnnouncement>) -> Self {
        let announcement = announcement
            .filter(|a| a.white == board.white_name && a.black == board.black_name);
        let base_ply = board.ply();
        PgnGame {
            game_number: board.game_number,
            event: announcement
                .map(|a| capitalize(&format!("{} game", a.kind)))
                .unwrap_or_else(|| "?".to_string()),
            site: site.to_string(),
            date: utc_date(SystemTime::now()),
            white: board.white_name.clone(),
            black: board.black_name.clone(),
            white_rating: announcement.and_then(|a| a.white_rating),
            black_rating: announcement.and_then(|a| a.black_rating),
            time_control: format!(
                "{}+{}",
                board.initial_time_minutes * 60,
                board.increment_seconds
            ),
            start_fen: (base_ply > 0).then(|| board.to_fen()),
            base_ply,
            moves: Vec::new(),
            clocks: BTreeMap::new(),
            notes: BTreeMap::new(),
        }
    }

    /// Add the move that led to `board`.
    pub fn record(&mut self, board: &Style12) {
        let ply = board.ply();
        if ply > self.base_ply {
            record_move(&mut self.moves, ply - self.base_ply, &board.last_move_san);
            // Anything recorded past this ply was taken back.
            self.clocks.split_off(&(ply + 1));
            self.notes.split_off(&(ply + 1));
            // The side that just moved is the one not to move now.
            let clock = if board.to_move_white {
                board.black_time_ms
            } else {
                board.white_time_ms
            };
            self.clocks.insert(ply, clock);
        }
    }

    /// Attach our engine's `note` (see `engine_note`) to ply `ply`, the
    /// move it's about to play.
    pub fn annotate(&mut self, ply: usize, note: String) {
        self.notes.insert(ply, note);
    }

    pub fn has_moves(&self) -> bool {
        !self.moves.is_empty()
    }

    /// The game as PGN text, ending with a blank line.
    pub fn to_pgn(&self, end: &GameEnd) -> String {
        let mut out = String::new();
        let mut tag = |name: &str, value: &str| {
            let value = value.replace('\\', "\\\\").replace('"', "\\\"");
            out.push_str(&format!("[{name} \"{value}\"]\n"));
        };
        tag("Event", &self.event);
        tag("Site", &self.site);
        tag("Date", &self.date);
        tag("Round", "-");
        tag("White", &self.white);
        tag("Black", &self.black);
        tag("Result", &end.result);
        if let Some(r) = self.white_rating {
            tag("WhiteElo", &r.to_string());
        }
        if let Some(r) = self.black_rating {
            tag("BlackElo", &r.to_string());
        }
        tag("TimeControl", &self.time_control);
        if let Some(fen) = &self.start_fen {
            tag("SetUp", "1");
            tag("FEN", fen);
        }
        out.push('\n');

        let mut tokens = Vec::new();
        for (i, san) in self.moves.iter().enumerate() {
            let ply = self.base_ply + i + 1;
            let number = ply.div_ceil(2);
            if ply % 2 == 1 {
                tokens.push(format!("{number}."));
            } else if i == 0 {
                tokens.push(format!("{number}..."));
            }
            tokens.push(san.clone());
            let comment: Vec<String> = self
                .notes
                .get(&ply)
                .cloned()
                .into_iter()
                .chain(self.clocks.get(&ply).map(|&ms| format!("[%clk {}]", clk(ms))))
                .collect();
            if !comment.is_empty() {
                tokens.push(format!("{{{}}}", comment.join(" ")));
            }
        }
        if !end.reason.is_empty() {
            tokens.push(format!("{{{}}}", end.reason.replace('}', ")")));
        }
        tokens.push(end.result.clone());
        out.push_str(&wrap(&tokens));
        out.push_str("\n\n");
        out
    }
}

/// Our engine's note for a move: score from its point of view, search
/// depth, and the time we spent, e.g. "+0.35/18 2.1s" or "-M3/22
/// 0.4s" - the format cutechess and other GUIs write.
pub fn engine_note(info: Option<&EngineInfo>, elapsed: Duration) -> String {
    let secs = format!("{:.1}s", elapsed.as_secs_f64());
    match info {
        Some(info) => {
            let score = info.score_string();
            let sign = if score.starts_with(['-', '?']) { "" } else { "+" };
            format!("{sign}{score}/{} {secs}", info.depth)
        }
        None => secs,
    }
}

/// "h:mm:ss", the `[%clk]` format.
fn clk(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// Append `pgn` to the file at `path`, creating it if needed.
pub fn append(path: &Path, pgn: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(pgn.as_bytes())
}

/// Join tokens with spaces, breaking lines before `LINE_WIDTH`.
fn wrap(tokens: &[String]) -> String {
    let mut out = String::new();
    let mut line_len = 0;
    for token in tokens {
        if line_len > 0 && line_len + 1 + token.len() > LINE_WIDTH {
            out.push('\n');
            line_len = 0;
        } else if line_len > 0 {
            out.push(' ');
            line_len += 1;
        }
        out.push_str(token);
        line_len += token.len();
    }
    out
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// "YYYY.MM.DD" (UTC), the PGN date format.
fn utc_date(time: SystemTime) -> String {
    let days = time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() / 86_400;
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}.{month:02}.{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn board(rows: &str, tail: &str) -> Style12 {
        Style12::parse(&format!("<12> {rows} {tail}")).unwrap()
    }

    const START: &str =
        "rnbqkbnr pppppppp -------- -------- -------- -------- PPPPPPPP RNBQKBNR";
    const AFTER_E4: &str =
        "rnbqkbnr pppppppp -------- -------- ----P--- -------- PPPP-PPP RNBQKBNR";
    const AFTER_E5: &str =
        "rnbqkbnr pppp-ppp -------- ----p--- ----P--- -------- PPPP-PPP RNBQKBNR";

    #[test]
    fn record_move_appends_rewinds_and_fills_gaps() {
        let mut moves = Vec::new();
        record_move(&mut moves, 0, "none");
        assert!(moves.is_empty());
        record_move(&mut moves, 1, "e4");
        record_move(&mut moves, 2, "e5");
        assert_eq!(moves, ["e4", "e5"]);
        // Same board again (refresh): unchanged.
        record_move(&mut moves, 2, "e5");
        assert_eq!(moves, ["e4", "e5"]);
        // Takeback to ply 1 then a different reply.
        record_move(&mut moves, 2, "c5");
        assert_eq!(moves, ["e4", "c5"]);
        // Joined mid-game.
        let mut joined = Vec::new();
        record_move(&mut joined, 3, "Nf3");
        assert_eq!(joined, ["…", "…", "Nf3"]);
    }

    #[test]
    fn parses_creating_lines() {
        assert_eq!(
            parse_creating("Creating: GriffyJr (1920) GuestHRPY (++++) unrated blitz 2 12"),
            Some(GameAnnouncement {
                white: "GriffyJr".into(),
                white_rating: Some(1920),
                black: "GuestHRPY".into(),
                black_rating: None,
                kind: "unrated blitz".into(),
            })
        );
        assert_eq!(parse_creating("fics% tell bob hi"), None);
    }

    #[test]
    fn parses_game_end_lines() {
        assert_eq!(
            parse_game_end("fics% {Game 18 (GuestJTMH vs. GuestNBPB) GuestJTMH resigns} 0-1"),
            Some(GameEnd {
                game_number: 18,
                reason: "GuestJTMH resigns".into(),
                result: "0-1".into(),
            })
        );
        assert_eq!(
            parse_game_end("{Game 7 (a vs. b) Game drawn by repetition} 1/2-1/2").map(|e| e.result),
            Some("1/2-1/2".to_string())
        );
        // The start-of-game line isn't an end.
        assert_eq!(
            parse_game_end("{Game 18 (GuestJTMH vs. GuestNBPB) Creating unrated lightning match.}"),
            None
        );
    }

    #[test]
    fn writes_a_complete_game() {
        let tail = |side: &str, rel: i32, mv: u32, verbose: &str, san: &str| {
            format!("{side} -1 1 1 1 1 0 18 Alice Bob {rel} 1 0 39 39 60 60 {mv} {verbose} (0:00) {san} 0 0 0")
        };
        let first = board(START, &tail("W", 1, 1, "none", "none"));
        let announce = parse_creating("Creating: Alice (1500) Bob (++++) unrated lightning 1 0");
        let mut game = PgnGame::new(&first, "freechess.org", announce.as_ref());
        game.record(&first);
        game.record(&board(AFTER_E4, &tail("B", -1, 1, "P/e2-e4", "e4")));
        game.record(&board(AFTER_E5, &tail("W", 1, 2, "P/e7-e5", "e5")));
        assert!(game.has_moves());

        let end = parse_game_end("{Game 18 (Alice vs. Bob) Bob resigns} 1-0").unwrap();
        let pgn = game.to_pgn(&end);
        let date = utc_date(SystemTime::now());
        assert_eq!(
            pgn,
            format!(
                "[Event \"Unrated lightning game\"]\n[Site \"freechess.org\"]\n[Date \"{date}\"]\n\
                 [Round \"-\"]\n[White \"Alice\"]\n[Black \"Bob\"]\n[Result \"1-0\"]\n\
                 [WhiteElo \"1500\"]\n[TimeControl \"60+0\"]\n\n\
                 1. e4 {{[%clk 0:01:00]}} e5 {{[%clk 0:01:00]}} {{Bob resigns}} 1-0\n\n"
            )
        );
    }

    #[test]
    fn game_joined_midway_starts_from_its_fen() {
        let joined = board(
            AFTER_E4,
            "B -1 1 1 1 1 0 9 Alice Bob 1 1 0 39 39 60 60 1 P/e2-e4 (0:00) e4 0 0 0",
        );
        let mut game = PgnGame::new(&joined, "x", None);
        game.record(&joined);
        game.record(&board(
            AFTER_E5,
            "W -1 1 1 1 1 0 9 Alice Bob -1 1 0 39 39 60 60 2 P/e7-e5 (0:00) e5 0 0 0",
        ));
        let pgn = game.to_pgn(&GameEnd {
            game_number: 9,
            reason: String::new(),
            result: "*".into(),
        });
        assert!(pgn.contains("[Event \"?\"]"));
        assert!(pgn.contains("[SetUp \"1\"]\n[FEN \"rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b"));
        assert!(pgn.ends_with("1... e5 {[%clk 0:01:00]} *\n\n"), "{pgn}");
    }

    #[test]
    fn writes_engine_notes_and_clocks() {
        let tail = |side: &str, mv: u32, wt: u32, bt: u32, verbose: &str, san: &str| {
            format!("{side} -1 1 1 1 1 0 5 Bot Opp 1 5 0 39 39 {wt} {bt} {mv} {verbose} (0:00) {san} 0 0 0")
        };
        let first = board(START, &tail("W", 1, 300, 300, "none", "none"));
        let mut game = PgnGame::new(&first, "x", None);
        game.record(&first);
        game.annotate(1, "book".into());
        game.record(&board(AFTER_E4, &tail("B", 1, 299, 300, "P/e2-e4", "e4")));
        game.record(&board(AFTER_E5, &tail("W", 2, 299, 3725, "P/e7-e5", "e5")));
        // Annotated, then taken back before it was recorded again.
        game.annotate(3, "stale".into());
        game.record(&board(AFTER_E5, &tail("W", 2, 299, 3725, "P/e7-e5", "e5")));
        let pgn = game.to_pgn(&GameEnd {
            game_number: 5,
            reason: String::new(),
            result: "*".into(),
        });
        assert!(
            pgn.ends_with("1. e4 {book [%clk 0:04:59]} e5 {[%clk 1:02:05]} *\n\n"),
            "{pgn}"
        );
    }

    #[test]
    fn formats_engine_notes() {
        let info = |cp, mate| EngineInfo {
            depth: 18,
            score_cp: cp,
            score_mate: mate,
            nodes: None,
            nps: None,
            time_ms: None,
            pv: None,
        };
        let t = Duration::from_millis(2140);
        assert_eq!(engine_note(Some(&info(Some(35), None)), t), "+0.35/18 2.1s");
        assert_eq!(engine_note(Some(&info(Some(-120), None)), t), "-1.20/18 2.1s");
        assert_eq!(engine_note(Some(&info(None, Some(3))), t), "+M3/18 2.1s");
        assert_eq!(engine_note(None, t), "2.1s");
    }

    #[test]
    fn wraps_long_move_text() {
        let tokens: Vec<String> = (0..40).map(|i| format!("move{i}")).collect();
        assert!(wrap(&tokens).lines().all(|l| l.len() <= LINE_WIDTH));
    }

    #[test]
    fn formats_utc_dates() {
        assert_eq!(utc_date(UNIX_EPOCH), "1970.01.01");
        // 2026-10-01 00:00 UTC
        assert_eq!(utc_date(UNIX_EPOCH + Duration::from_secs(1_790_812_800)), "2026.10.01");
        // Leap day 2024-02-29
        assert_eq!(utc_date(UNIX_EPOCH + Duration::from_secs(1_709_164_800)), "2024.02.29");
    }
}
