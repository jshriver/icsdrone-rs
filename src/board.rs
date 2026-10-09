//! Parsing of FICS "style 12" board update lines into a board state and
//! FEN string. This replaces the hand-rolled parser in the original
//! project's board.c / icsdrone.h (`IcsBoard`).
//!
//! Style 12 example (FICS docs):
//! <12> rnbqkbnr pppppppp -------- -------- -------- -------- PPPPPPPP
//!      RNBQKBNR B -1 1 1 1 1 0 39 GuestABCD GuestEFGH -1 5 5 39 39 300
//!      300 1 none (0:00) none 0 0 0

use anyhow::{anyhow, Context, Result};

use crate::app::format_clock;

/// My relation to the game, field 19 of a style12 line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    IsolatedPosition,
    ObservingExamined,
    Examiner,
    /// Playing, it's the opponent's move.
    PlayingOpponentMove,
    /// Playing, it's my move.
    PlayingMyMove,
    Observing,
    Other(i32),
}

impl From<i32> for Relation {
    fn from(v: i32) -> Self {
        match v {
            -3 => Relation::IsolatedPosition,
            -2 => Relation::ObservingExamined,
            2 => Relation::Examiner,
            -1 => Relation::PlayingOpponentMove,
            1 => Relation::PlayingMyMove,
            0 => Relation::Observing,
            other => Relation::Other(other),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Style12 {
    /// 8 rows, rank 8 down to rank 1, each 8 chars (piece letters or '-')
    pub rows: [String; 8],
    pub to_move_white: bool,
    /// -1 if no double pawn push last move, else file (0-7) of the push
    pub double_push_file: i32,
    pub white_can_castle_short: bool,
    pub white_can_castle_long: bool,
    pub black_can_castle_short: bool,
    pub black_can_castle_long: bool,
    pub halfmove_clock: u32,
    pub game_number: i32,
    pub white_name: String,
    pub black_name: String,
    pub relation: Relation,
    // The following fields are part of the style12 protocol and parsed
    // for completeness, but the current MVP core loop doesn't need
    // them (time management uses white_time_ms/black_time_ms instead,
    // and there's no analysis/kibitz-of-strength or board-orientation
    // feature yet). Kept for when those land, rather than dropped and
    // re-added later - #[allow(dead_code)] to keep `cargo build`
    // warning-free until then.
    pub initial_time_minutes: i32,
    pub increment_seconds: i32,
    pub white_strength: i32,
    pub black_strength: i32,
    pub white_time_ms: i64,
    pub black_time_ms: i64,
    pub next_move_number: u32,
    /// Verbose coordinate notation of the previous move, e.g. "e2-e4",
    /// or "none" for the initial position.
    pub last_move_verbose: String,
    /// The previous move in algebraic notation, e.g. "Nf3", "Qxc4",
    /// "O-O" (style12 field 28), or "none" for the initial position.
    /// Used for the GUI's move list.
    pub last_move_san: String,
    /// Time the previous move took, e.g. "(0:02)" (style12 field 27).
    pub last_move_time: String,
    #[allow(dead_code)]
    pub board_flipped: bool,
}

impl Style12 {
    /// Parse a full ICS line (which may have other text before the
    /// "<12> " tag, as ICS lines are sometimes concatenated with
    /// prompts) into a Style12 struct.
    pub fn parse(line: &str) -> Result<Style12> {
        let idx = line
            .find("<12>")
            .ok_or_else(|| anyhow!("not a style12 line"))?;
        let fields: Vec<&str> = line[idx + 4..].split_whitespace().collect();
        if fields.len() < 26 {
            return Err(anyhow!(
                "style12 line has only {} fields, expected >= 26",
                fields.len()
            ));
        }

        let mut rows: [String; 8] = Default::default();
        for i in 0..8 {
            rows[i] = fields[i].to_string();
        }

        let to_move_white = fields[8] == "W";
        let double_push_file: i32 = fields[9].parse().context("double_push_file")?;
        let white_can_castle_short = fields[10] == "1";
        let white_can_castle_long = fields[11] == "1";
        let black_can_castle_short = fields[12] == "1";
        let black_can_castle_long = fields[13] == "1";
        let halfmove_clock: u32 = fields[14].parse().context("halfmove_clock")?;
        let game_number: i32 = fields[15].parse().context("game_number")?;
        let white_name = fields[16].to_string();
        let black_name = fields[17].to_string();
        let relation: Relation = fields[18].parse::<i32>().context("relation")?.into();
        let initial_time_minutes: i32 = fields[19].parse().context("initial_time")?;
        let increment_seconds: i32 = fields[20].parse().context("increment")?;
        let white_strength: i32 = fields[21].parse().context("white_strength")?;
        let black_strength: i32 = fields[22].parse().context("black_strength")?;
        let white_time_ms: i64 = fields[23].parse::<i64>().context("white_time")? * 1000;
        let black_time_ms: i64 = fields[24].parse::<i64>().context("black_time")? * 1000;
        let next_move_number: u32 = fields[25].parse().context("next_move_number")?;
        let last_move_verbose = fields.get(26).unwrap_or(&"none").to_string();
        let last_move_time = fields.get(27).unwrap_or(&"(0:00)").to_string();
        let last_move_san = fields.get(28).unwrap_or(&"none").to_string();
        let board_flipped = fields.get(29).map(|s| *s == "1").unwrap_or(false);

        Ok(Style12 {
            rows,
            to_move_white,
            double_push_file,
            white_can_castle_short,
            white_can_castle_long,
            black_can_castle_short,
            black_can_castle_long,
            halfmove_clock,
            game_number,
            white_name,
            black_name,
            relation,
            initial_time_minutes,
            increment_seconds,
            white_strength,
            black_strength,
            white_time_ms,
            black_time_ms,
            next_move_number,
            last_move_verbose,
            last_move_san,
            last_move_time,
            board_flipped,
        })
    }

    /// The ply number of the move that led to this board (1 = White's
    /// first move), or 0 for the starting position.
    pub fn ply(&self) -> usize {
        let full_moves = self.next_move_number.saturating_sub(1) as usize;
        full_moves * 2 + usize::from(!self.to_move_white)
    }

    /// Convert to a FEN string (board + side to move + castling + en
    /// passant + halfmove clock + fullmove number), replacing
    /// BoardToFen from the original board.c.
    pub fn to_fen(&self) -> String {
        let mut ranks = Vec::with_capacity(8);
        for row in &self.rows {
            let mut fen_row = String::new();
            let mut empty_run = 0;
            for c in row.chars() {
                if c == '-' {
                    empty_run += 1;
                } else {
                    if empty_run > 0 {
                        fen_row.push_str(&empty_run.to_string());
                        empty_run = 0;
                    }
                    fen_row.push(c);
                }
            }
            if empty_run > 0 {
                fen_row.push_str(&empty_run.to_string());
            }
            ranks.push(fen_row);
        }
        let board_part = ranks.join("/");

        let side = if self.to_move_white { "w" } else { "b" };

        let mut castle = String::new();
        if self.white_can_castle_short {
            castle.push('K');
        }
        if self.white_can_castle_long {
            castle.push('Q');
        }
        if self.black_can_castle_short {
            castle.push('k');
        }
        if self.black_can_castle_long {
            castle.push('q');
        }
        if castle.is_empty() {
            castle.push('-');
        }

        let ep = if self.double_push_file >= 0 && self.double_push_file <= 7 {
            let file = (b'a' + self.double_push_file as u8) as char;
            let rank = if self.to_move_white { 6 } else { 3 };
            format!("{file}{rank}")
        } else {
            "-".to_string()
        };

        // Style12's "next move number" is the move about to be played;
        // FEN's fullmove number matches that directly.
        format!(
            "{board_part} {side} {castle} {ep} {} {}",
            self.halfmove_clock, self.next_move_number
        )
    }

    /// Render the board the way FICS does with `set style 1`, from our
    /// side (Black at the bottom when we're Black), with the game's
    /// details down the right:
    ///
    /// ```text
    /// Game 39 (GuestABCD vs. GuestEFGH)
    ///
    ///        ---------------------------------
    ///     8  | *R| *N| *B| *Q| *K| *B| *N| *R|     Move # : 1 (White)
    ///        |---+---+---+---+---+---+---+---|
    ///     7  | *P| *P| *P| *P| *P| *P| *P| *P|
    ///        |---+---+---+---+---+---+---+---|
    ///     6  |   |   |   |   |   |   |   |   |
    ///        |---+---+---+---+---+---+---+---|
    ///     5  |   |   |   |   |   |   |   |   |
    ///        |---+---+---+---+---+---+---+---|
    ///     4  |   |   |   |   |   |   |   |   |     Black Clock : 5:00
    ///        |---+---+---+---+---+---+---+---|
    ///     3  |   |   |   |   |   |   |   |   |     White Clock : 5:00
    ///        |---+---+---+---+---+---+---+---|
    ///     2  | P | P | P | P | P | P | P | P |     Black Strength : 39
    ///        |---+---+---+---+---+---+---+---|
    ///     1  | R | N | B | Q | K | B | N | R |     White Strength : 39
    ///        ---------------------------------
    ///          a   b   c   d   e   f   g   h
    /// ```
    ///
    /// Black pieces are marked with `*`, as FICS does. After a move,
    /// the third rank line reads e.g. "Black Moves : 'e5'  (0:02)".
    /// Purely for the bot operator's own console - unlike `to_fen`,
    /// nothing in the engine/ICS protocol pipeline reads this.
    pub fn to_ascii_board(&self) -> String {
        self.render_style1(false)
    }

    /// Same board as `to_ascii_board`, but with the previous move's
    /// from/to squares (see `last_move_squares`) highlighted using
    /// ANSI SGR background colors - the "from" square on a light blue
    /// background, the "to" square on a light cyan background (both
    /// from the basic 16-color ANSI palette, so they stay portable
    /// rather than needing 256-color support). The background (not
    /// just the piece letter) is colored so the "from" square is
    /// still visible even though it's usually empty (the piece just
    /// left it) - a colored space on its own would be invisible.
    /// Purely cosmetic, like `to_ascii_board`; nothing downstream
    /// parses this.
    pub fn to_ansi_board(&self) -> String {
        self.render_style1(true)
    }

    /// Whether we're Black in a game we're playing, so the board is
    /// drawn with Black at the bottom.
    fn black_at_bottom(&self) -> bool {
        match self.relation {
            Relation::PlayingMyMove => !self.to_move_white,
            Relation::PlayingOpponentMove => self.to_move_white,
            _ => false,
        }
    }

    fn render_style1(&self, highlight: bool) -> String {
        const EDGE: &str = "       ---------------------------------";
        const DIVIDER: &str = "       |---+---+---+---+---+---+---+---|";
        const RESET: &str = "\x1b[0m";
        const FROM_COLOR: &str = "\x1b[30;104m"; // black on light blue background
        const TO_COLOR: &str = "\x1b[30;106m"; // black on light cyan background

        let (from_sq, to_sq) = if highlight {
            self.last_move_squares().unzip()
        } else {
            (None, None)
        };
        let side = |white: bool| if white { "White" } else { "Black" };

        // The details beside each rank line, top to bottom.
        let mut info: [String; 8] = Default::default();
        info[0] = format!(
            "Move # : {} ({})",
            self.next_move_number,
            side(self.to_move_white)
        );
        if self.last_move_san != "none" {
            info[2] = format!(
                "{} Moves : '{}'  {}",
                side(!self.to_move_white),
                self.last_move_san,
                self.last_move_time
            );
        }
        info[4] = format!("Black Clock : {}", format_clock(self.black_time_ms));
        info[5] = format!("White Clock : {}", format_clock(self.white_time_ms));
        info[6] = format!("Black Strength : {}", self.black_strength);
        info[7] = format!("White Strength : {}", self.white_strength);

        // Rows and columns of `self.rows`, top-left first.
        let order: Vec<usize> = if self.black_at_bottom() {
            (0..8).rev().collect()
        } else {
            (0..8).collect()
        };

        let mut out = format!(
            "Game {} ({} vs. {})\n\n{EDGE}\n",
            self.game_number, self.white_name, self.black_name
        );
        for (line, &i) in order.iter().enumerate() {
            if line > 0 {
                out.push_str(DIVIDER);
                out.push('\n');
            }
            out.push_str(&format!("    {}  |", 8 - i));
            let row: Vec<char> = self.rows[i].chars().collect();
            for &j in &order {
                let cell = match row.get(j).copied().unwrap_or('-') {
                    '-' => "   ".to_string(),
                    c if c.is_ascii_uppercase() => format!(" {c} "),
                    c => format!(" *{}", c.to_ascii_uppercase()),
                };
                if from_sq == Some((i, j)) {
                    out.push_str(&format!("{FROM_COLOR}{cell}{RESET}|"));
                } else if to_sq == Some((i, j)) {
                    out.push_str(&format!("{TO_COLOR}{cell}{RESET}|"));
                } else {
                    out.push_str(&format!("{cell}|"));
                }
            }
            if !info[line].is_empty() {
                out.push_str("     ");
                out.push_str(&info[line]);
            }
            out.push('\n');
        }
        out.push_str(EDGE);
        out.push('\n');
        let files: String = order
            .iter()
            .map(|&j| format!("   {}", (b'a' + j as u8) as char))
            .collect();
        out.push_str(&format!("      {files}"));
        out
    }

    /// Render the board, plain or ANSI-highlighted per `color`. This is
    /// what callers should use; `to_ascii_board`/`to_ansi_board` are
    /// kept as separate methods mainly so each is independently
    /// testable.
    pub fn to_board_string(&self, color: bool) -> String {
        if color {
            self.to_ansi_board()
        } else {
            self.to_ascii_board()
        }
    }

    /// Best-effort (row, col) of the from/to squares of the previous
    /// move, indexed the same way as `self.rows` (row 0 = rank 8, col
    /// 0 = file a), for highlighting in `to_ansi_board` and the GUI.
    ///
    /// `last_move_verbose` (style12 field 26) looks like "P/e2-e4",
    /// "N/g1-f3", "P/e7-e8=Q" (promotion), "P/d5-c6ep" (en passant),
    /// "O-O"/"O-O-O" (castling), or "none" for the initial position.
    /// Only the "<piece>/<from>-<to>..." shape has recognizable
    /// square coordinates; castling and "none" fall through to `None`,
    /// which just means no highlight - the board still renders fine.
    pub fn last_move_squares(&self) -> Option<((usize, usize), (usize, usize))> {
        // Piece prefix (e.g. "P/") is separated by '/'; take whatever
        // comes after the last one, or the whole string if there's no
        // '/' at all (e.g. "O-O", which parse_square will reject).
        let after_piece = self
            .last_move_verbose
            .rsplit('/')
            .next()
            .unwrap_or(&self.last_move_verbose);

        let mut parts = after_piece.splitn(2, '-');
        let from_tok = parts.next()?;
        let to_tok = parts.next()?;

        let from = parse_square(from_tok)?;
        let to = parse_square(to_tok)?;
        Some((from, to))
    }

    /// The previous move in UCI notation ("e2e4", "e7e8q", "e1g1"),
    /// translated from `last_move_verbose`, or `None` for the initial
    /// position or anything unrecognized. Castling ("o-o"/"o-o-o")
    /// carries no squares, so the king's are reconstructed from the
    /// side that moved - the one *not* to move now.
    pub fn last_move_uci(&self) -> Option<String> {
        let verbose = self.last_move_verbose.as_str();
        let rank = if self.to_move_white { '8' } else { '1' };
        if verbose.eq_ignore_ascii_case("o-o") {
            return Some(format!("e{rank}g{rank}"));
        }
        if verbose.eq_ignore_ascii_case("o-o-o") {
            return Some(format!("e{rank}c{rank}"));
        }
        let (piece, squares) = verbose.split_once('/')?;
        let (from, rest) = squares.split_once('-')?;
        let to = rest.get(..2)?;
        if from.len() != 2 {
            return None;
        }
        parse_square(from)?;
        let (to_row, to_col) = parse_square(to)?;
        // A pawn reaching the last rank promoted: to whatever stands on
        // that square now. The board is used rather than an "=Q" suffix,
        // which FICS doesn't reliably send - and "a7a8" without its
        // piece makes engines drop the rest of the move list.
        let promotion = if piece.eq_ignore_ascii_case("p") && (to_row == 0 || to_row == 7) {
            let promoted = self.rows[to_row].chars().nth(to_col)?;
            if !"qrbnQRBN".contains(promoted) {
                return None;
            }
            promoted.to_ascii_lowercase().to_string()
        } else {
            String::new()
        };
        Some(format!("{from}{to}{promotion}"))
    }
}

/// Parse the leading two characters of `tok` as a square in algebraic
/// notation (file 'a'-'h', rank '1'-'8') into `(row, col)` matching
/// `Style12::rows` (row 0 = rank 8, col 0 = file a). Trailing
/// characters (e.g. the "=Q" of a promotion, or the "ep" of an en
/// passant capture) are ignored rather than rejected, since callers
/// only care about the square itself.
fn parse_square(tok: &str) -> Option<(usize, usize)> {
    let bytes = tok.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let file = bytes[0];
    let rank = bytes[1];
    if !(b'a'..=b'h').contains(&file) || !(b'1'..=b'8').contains(&rank) {
        return None;
    }
    let col = (file - b'a') as usize;
    let row = 8 - (rank - b'0') as usize;
    Some((row, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_plies() {
        let tail = |side: &str, mv: u32| {
            format!("<12> rnbqkbnr pppppppp -------- -------- -------- -------- PPPPPPPP RNBQKBNR {side} -1 1 1 1 1 0 7 a b 1 1 0 39 39 60 60 {mv} none (0:00) none 0 0 0")
        };
        assert_eq!(Style12::parse(&tail("W", 1)).unwrap().ply(), 0);
        assert_eq!(Style12::parse(&tail("B", 1)).unwrap().ply(), 1);
        assert_eq!(Style12::parse(&tail("W", 2)).unwrap().ply(), 2);
        assert_eq!(Style12::parse(&tail("B", 23)).unwrap().ply(), 45);
    }

    #[test]
    fn parses_start_position() {
        let line = "<12> rnbqkbnr pppppppp -------- -------- -------- -------- PPPPPPPP RNBQKBNR W -1 1 1 1 1 0 39 GuestABCD GuestEFGH -1 5 0 39 39 300 300 1 none (0:00) none 0 0 0";
        let s12 = Style12::parse(line).unwrap();
        assert_eq!(
            s12.to_fen(),
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
        );
    }

    #[test]
    fn parses_double_push_en_passant() {
        // after 1. e4, black to move, en passant possible on e3? Actually
        // after white plays e2-e4 it is black to move and ep square is e3.
        let line = "<12> rnbqkbnr pppppppp -------- -------- ----P--- -------- PPPP-PPP RNBQKBNR B 4 1 1 1 1 0 39 GuestABCD GuestEFGH -1 5 0 39 39 300 300 1 P/e2-e4 (0:00) e4 0 0 0";
        let s12 = Style12::parse(line).unwrap();
        assert_eq!(
            s12.to_fen(),
            "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1"
        );
    }

    #[test]
    fn renders_ascii_board_for_start_position() {
        let line = "<12> rnbqkbnr pppppppp -------- -------- -------- -------- PPPPPPPP RNBQKBNR W -1 1 1 1 1 0 39 GuestABCD GuestEFGH 1 5 0 39 39 300 300 1 none (0:00) none 0 0 0";
        let s12 = Style12::parse(line).unwrap();
        let expected = "Game 39 (GuestABCD vs. GuestEFGH)

       ---------------------------------
    8  | *R| *N| *B| *Q| *K| *B| *N| *R|     Move # : 1 (White)
       |---+---+---+---+---+---+---+---|
    7  | *P| *P| *P| *P| *P| *P| *P| *P|
       |---+---+---+---+---+---+---+---|
    6  |   |   |   |   |   |   |   |   |
       |---+---+---+---+---+---+---+---|
    5  |   |   |   |   |   |   |   |   |
       |---+---+---+---+---+---+---+---|
    4  |   |   |   |   |   |   |   |   |     Black Clock : 5:00
       |---+---+---+---+---+---+---+---|
    3  |   |   |   |   |   |   |   |   |     White Clock : 5:00
       |---+---+---+---+---+---+---+---|
    2  | P | P | P | P | P | P | P | P |     Black Strength : 39
       |---+---+---+---+---+---+---+---|
    1  | R | N | B | Q | K | B | N | R |     White Strength : 39
       ---------------------------------
         a   b   c   d   e   f   g   h";
        assert_eq!(s12.to_ascii_board(), expected);
    }

    #[test]
    fn draws_the_board_from_blacks_side_when_we_are_black() {
        // Opponent (White) to move after 1. e4 e5: we're Black.
        let line = "<12> rnbqkbnr pppp-ppp -------- ----p--- ----P--- -------- PPPP-PPP RNBQKBNR W 4 1 1 1 1 0 39 GuestABCD GuestEFGH -1 5 0 39 39 298 296 2 P/e7-e5 (0:04) e5 0 0 0";
        let board = Style12::parse(line).unwrap().to_ascii_board();
        let lines: Vec<&str> = board.lines().collect();
        assert_eq!(lines[3], "    1  | R | N | B | K | Q | B | N | R |     Move # : 2 (White)");
        assert_eq!(lines[7], "    3  |   |   |   |   |   |   |   |   |     Black Moves : 'e5'  (0:04)");
        assert_eq!(lines[11], "    5  |   |   |   | *P|   |   |   |   |     Black Clock : 4:56");
        assert_eq!(lines[13], "    6  |   |   |   |   |   |   |   |   |     White Clock : 4:58");
        assert_eq!(lines.last(), Some(&"         h   g   f   e   d   c   b   a"));
    }

    #[test]
    fn ansi_board_highlights_from_and_to_squares() {
        // After 1. e4: last move is "P/e2-e4", pawn moved e2 -> e4.
        let line = "<12> rnbqkbnr pppppppp -------- -------- ----P--- -------- PPPP-PPP RNBQKBNR B 4 1 1 1 1 0 39 GuestABCD GuestEFGH -1 5 0 39 39 300 300 1 P/e2-e4 (0:00) e4 0 0 0";
        let s12 = Style12::parse(line).unwrap();
        let ansi = s12.to_ansi_board();

        // e2 is row 6, col 4 (now empty, pawn left); e4 is row 4, col 4
        // (now holds the pawn). Both get a colored *background* (not
        // just colored text) so the from square is visible even
        // though it's empty.
        assert_eq!(s12.last_move_squares(), Some(((6, 4), (4, 4))));
        assert!(ansi.contains("\x1b[30;104m   \x1b[0m|")); // from square, empty, light blue background
        assert!(ansi.contains("\x1b[30;106m P \x1b[0m|")); // to square, holds the pawn, light cyan background
        // Plain board is unaffected.
        assert!(!s12.to_ascii_board().contains('\x1b'));
    }

    #[test]
    fn ansi_board_falls_back_cleanly_for_castling_and_initial_position() {
        let line = "<12> rnbqkbnr pppppppp -------- -------- -------- -------- PPPPPPPP RNBQKBNR W -1 1 1 1 1 0 39 GuestABCD GuestEFGH -1 5 0 39 39 300 300 1 none (0:00) none 0 0 0";
        let s12 = Style12::parse(line).unwrap();
        assert_eq!(s12.last_move_squares(), None);
        // No escape codes anywhere when there's nothing to highlight.
        assert!(!s12.to_ansi_board().contains('\x1b'));

        let mut castled = s12.clone();
        castled.last_move_verbose = "O-O".to_string();
        assert_eq!(castled.last_move_squares(), None);
    }

    #[test]
    fn translates_last_move_to_uci() {
        let line = "<12> rnbqkbnr pppppppp -------- -------- ----P--- -------- PPPP-PPP RNBQKBNR B 4 1 1 1 1 0 39 GuestABCD GuestEFGH -1 5 0 39 39 300 300 1 P/e2-e4 (0:00) e4 0 0 0";
        let mut s12 = Style12::parse(line).unwrap();
        assert_eq!(s12.last_move_uci().as_deref(), Some("e2e4"));

        // Promotions take the piece from the board, with or without
        // FICS's "=Q" suffix.
        s12.rows[0] = "rnbqQbnr".to_string();
        s12.last_move_verbose = "P/e7-e8=Q".to_string();
        assert_eq!(s12.last_move_uci().as_deref(), Some("e7e8q"));
        s12.last_move_verbose = "P/e7-e8".to_string();
        assert_eq!(s12.last_move_uci().as_deref(), Some("e7e8q"));
        s12.rows[0] = "rnbqNbnr".to_string();
        assert_eq!(s12.last_move_uci().as_deref(), Some("e7e8n"));
        // No promoted piece where it should be: unknown, not a guess.
        s12.rows[0] = "rnbq-bnr".to_string();
        assert_eq!(s12.last_move_uci(), None);
        s12.last_move_verbose = "P/d5-c6ep".to_string();
        assert_eq!(s12.last_move_uci().as_deref(), Some("d5c6"));

        // Black to move now, so White just castled.
        s12.last_move_verbose = "o-o".to_string();
        assert_eq!(s12.last_move_uci().as_deref(), Some("e1g1"));
        s12.last_move_verbose = "O-O-O".to_string();
        assert_eq!(s12.last_move_uci().as_deref(), Some("e1c1"));
        s12.to_move_white = true;
        assert_eq!(s12.last_move_uci().as_deref(), Some("e8c8"));

        s12.last_move_verbose = "none".to_string();
        assert_eq!(s12.last_move_uci(), None);
    }

    #[test]
    fn to_board_string_dispatches_on_color_flag() {
        let line = "<12> rnbqkbnr pppppppp -------- -------- ----P--- -------- PPPP-PPP RNBQKBNR B 4 1 1 1 1 0 39 GuestABCD GuestEFGH -1 5 0 39 39 300 300 1 P/e2-e4 (0:00) e4 0 0 0";
        let s12 = Style12::parse(line).unwrap();
        assert_eq!(s12.to_board_string(false), s12.to_ascii_board());
        assert_eq!(s12.to_board_string(true), s12.to_ansi_board());
        assert_ne!(s12.to_board_string(false), s12.to_board_string(true));
    }
}
