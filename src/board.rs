//! Parsing of FICS "style 12" board update lines into a board state and
//! FEN string. This replaces the hand-rolled parser in the original
//! project's board.c / icsdrone.h (`IcsBoard`).
//!
//! Style 12 example (FICS docs):
//! <12> rnbqkbnr pppppppp -------- -------- -------- -------- PPPPPPPP
//!      RNBQKBNR B -1 1 1 1 1 0 39 GuestABCD GuestEFGH -1 5 5 39 39 300
//!      300 1 none (0:00) none 0 0 0

use anyhow::{anyhow, Context, Result};

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
    #[allow(dead_code)]
    pub initial_time_minutes: i32,
    pub increment_seconds: i32,
    #[allow(dead_code)]
    pub white_strength: i32,
    #[allow(dead_code)]
    pub black_strength: i32,
    pub white_time_ms: i64,
    pub black_time_ms: i64,
    pub next_move_number: u32,
    /// Verbose coordinate notation of the previous move, e.g. "e2-e4",
    /// or "none" for the initial position.
    #[allow(dead_code)]
    pub last_move_verbose: String,
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
            board_flipped,
        })
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
