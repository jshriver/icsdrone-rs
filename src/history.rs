//! The game's move history in UCI form, so the engine is sent
//! `position fen <base> moves <m1> ... <mN>` rather than a bare FEN of
//! the current board. UCI engines are stateless: the move list is the
//! only way they learn which positions already occurred, and without
//! it they can't see (or avoid) a threefold repetition. This is the
//! job of the original's SendMovesToComputer bookkeeping.

use crate::board::Style12;

#[derive(Debug, Clone)]
pub struct GameHistory {
    pub game_number: i32,
    /// FEN of the board the moves start from: the first board we saw,
    /// normally the initial position.
    base_fen: String,
    /// Ply of that board (0 for the initial position).
    base_ply: usize,
    moves: Vec<String>,
}

impl GameHistory {
    /// Start tracking from `board`, the first board of the game we've
    /// seen.
    pub fn new(board: &Style12) -> Self {
        GameHistory {
            game_number: board.game_number,
            base_fen: board.to_fen(),
            base_ply: board.ply(),
            moves: Vec::new(),
        }
    }

    /// Add the move that led to `board`. A repeated board (a refresh)
    /// or a takeback overwrites rather than appends, like
    /// `pgn::record_move`. If the move can't be placed - a gap in the
    /// boards we saw, a takeback past our base, or a move we can't
    /// translate - start over from `board`, so what we send the engine
    /// is always a correct position, just with less history.
    pub fn record(&mut self, board: &Style12) {
        let ply = board.ply();
        let index = ply.wrapping_sub(self.base_ply);
        match board.last_move_uci() {
            Some(mv) if ply > self.base_ply && index <= self.moves.len() + 1 => {
                self.moves.truncate(index - 1);
                self.moves.push(mv);
            }
            _ if ply == self.base_ply => self.moves.clear(),
            _ => *self = GameHistory::new(board),
        }
    }

    pub fn base_fen(&self) -> &str {
        &self.base_fen
    }

    pub fn moves(&self) -> &[String] {
        &self.moves
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "<12> rnbqkbnr pppppppp -------- -------- -------- -------- PPPPPPPP RNBQKBNR W -1 1 1 1 1 0 39 White Black 1 5 0 39 39 300 300 1 none (0:00) none 0 0 0";
    const E4: &str = "<12> rnbqkbnr pppppppp -------- -------- ----P--- -------- PPPP-PPP RNBQKBNR B 4 1 1 1 1 0 39 White Black -1 5 0 39 39 300 300 1 P/e2-e4 (0:00) e4 0 0 0";
    const E4_E5: &str = "<12> rnbqkbnr pppp-ppp -------- ----p--- ----P--- -------- PPPP-PPP RNBQKBNR W 4 1 1 1 1 0 39 White Black 1 5 0 39 39 300 300 2 P/e7-e5 (0:00) e5 0 0 0";
    const E4_C5: &str = "<12> rnbqkbnr pp-ppppp -------- --p----- ----P--- -------- PPPP-PPP RNBQKBNR W 2 1 1 1 1 0 39 White Black 1 5 0 39 39 300 300 2 P/c7-c5 (0:00) c5 0 0 0";

    fn board(line: &str) -> Style12 {
        Style12::parse(line).unwrap()
    }

    #[test]
    fn records_moves_from_the_start() {
        let mut h = GameHistory::new(&board(START));
        h.record(&board(START));
        h.record(&board(E4));
        h.record(&board(E4_E5));
        assert_eq!(h.base_fen(), board(START).to_fen());
        assert_eq!(h.moves(), ["e2e4", "e7e5"]);
    }

    #[test]
    fn refresh_and_takeback_overwrite_instead_of_appending() {
        let mut h = GameHistory::new(&board(START));
        h.record(&board(E4));
        h.record(&board(E4_E5));
        h.record(&board(E4_E5)); // refresh
        assert_eq!(h.moves(), ["e2e4", "e7e5"]);
        h.record(&board(E4)); // takeback of 1...e5
        assert_eq!(h.moves(), ["e2e4"]);
        h.record(&board(E4_C5));
        assert_eq!(h.moves(), ["e2e4", "c7c5"]);
        h.record(&board(START)); // take everything back
        assert!(h.moves().is_empty());
    }

    #[test]
    fn joining_mid_game_starts_from_that_board() {
        let mut h = GameHistory::new(&board(E4));
        h.record(&board(E4));
        h.record(&board(E4_E5));
        assert_eq!(h.base_fen(), board(E4).to_fen());
        assert_eq!(h.moves(), ["e7e5"]);
    }

    #[test]
    fn gap_or_takeback_past_base_restarts_from_current_board() {
        let mut h = GameHistory::new(&board(START));
        h.record(&board(E4_E5)); // missed 1. e4
        assert_eq!(h.base_fen(), board(E4_E5).to_fen());
        assert!(h.moves().is_empty());
        h.record(&board(E4)); // takeback past the new base
        assert_eq!(h.base_fen(), board(E4).to_fen());
        assert!(h.moves().is_empty());
    }
}
