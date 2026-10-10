//! UCI -> SAN conversion for display, e.g. the engine's "e4e3 a3d3"
//! shown as "39... e3 40. Rd3". Uses shakmaty for move legality,
//! disambiguation ("Rfe1") and check/mate markers.

use shakmaty::fen::Fen;
use shakmaty::san::SanPlus;
use shakmaty::uci::UciMove;
use shakmaty::{CastlingMode, Chess, Color, Position};

/// `uci_moves` (space-separated, as in a UCI "pv") played from `fen`,
/// as numbered SAN: "12. Nf3 Nc6 13. Bb5" when White moves first,
/// "12... Nc6 13. Bb5" when Black does. If a move can't be parsed or
/// isn't legal, it and everything after it are kept in UCI form, so
/// nothing the engine said is lost.
pub fn line_to_san(fen: &str, uci_moves: &str) -> String {
    let mut moves = uci_moves.split_whitespace();
    let Some(mut pos) = position(fen) else {
        return uci_moves.to_string();
    };

    let mut out = Vec::new();
    for (i, uci) in moves.by_ref().enumerate() {
        let Some(m) = uci.parse::<UciMove>().ok().and_then(|u| u.to_move(&pos).ok()) else {
            out.push(uci.to_string());
            break;
        };
        let number = pos.fullmoves();
        match pos.turn() {
            Color::White => out.push(format!("{number}.")),
            Color::Black if i == 0 => out.push(format!("{number}...")),
            Color::Black => {}
        }
        out.push(SanPlus::from_move_and_play_unchecked(&mut pos, m).to_string());
    }
    out.extend(moves.map(str::to_string));
    out.join(" ")
}

/// A single UCI move from `fen` as SAN ("e1g1" -> "O-O"), or the UCI
/// text unchanged if it isn't legal there.
pub fn move_to_san(fen: &str, uci: &str) -> String {
    position(fen)
        .and_then(|mut pos| {
            let m = uci.parse::<UciMove>().ok()?.to_move(&pos).ok()?;
            Some(SanPlus::from_move_and_play_unchecked(&mut pos, m).to_string())
        })
        .unwrap_or_else(|| uci.to_string())
}

/// Whether the side to move in `fen` has any legal move, i.e. isn't
/// checkmated or stalemated. A FEN shakmaty can't read counts as
/// having moves, so the engine still gets asked as before.
pub fn has_legal_moves(fen: &str) -> bool {
    position(fen).map_or(true, |pos| !pos.legal_moves().is_empty())
}

/// Whether `uci` is a legal move from `fen`. A FEN shakmaty can't
/// read counts as legal, since there's nothing to check against.
pub fn is_legal_move(fen: &str, uci: &str) -> bool {
    position(fen).map_or(true, |pos| {
        uci.parse::<UciMove>()
            .ok()
            .is_some_and(|u| u.to_move(&pos).is_ok())
    })
}

fn position(fen: &str) -> Option<Chess> {
    Fen::from_ascii(fen.as_bytes())
        .ok()?
        .into_position(CastlingMode::Standard)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

    #[test]
    fn no_legal_moves_when_mated_or_stalemated() {
        assert!(has_legal_moves(START));
        // The game in test.log: White's Kf5 mated by ...Bc8#.
        assert!(!has_legal_moves("2b5/6q1/8/5K2/8/4k3/8/8 w - - 0 77"));
        // Stalemate.
        assert!(!has_legal_moves("7k/5Q2/6K1/8/8/8/8/8 b - - 0 1"));
    }

    #[test]
    fn checks_move_legality() {
        assert!(is_legal_move(START, "e2e4"));
        assert!(!is_legal_move(START, "c7d5b"));
        assert!(!is_legal_move(START, "junk"));
        let mated = "2b5/6q1/8/5K2/8/4k3/8/8 w - - 0 77";
        assert!(!is_legal_move(mated, "c7d5b"));
        // Unreadable FEN: nothing to check against.
        assert!(is_legal_move("not a fen", "c7d5b"));
    }

    #[test]
    fn numbers_a_line_from_white() {
        assert_eq!(
            line_to_san(START, "e2e4 e7e5 g1f3 b8c6 f1b5"),
            "1. e4 e5 2. Nf3 Nc6 3. Bb5"
        );
    }

    #[test]
    fn numbers_a_line_from_black() {
        let after_e4 = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1";
        assert_eq!(line_to_san(after_e4, "c7c5 g1f3"), "1... c5 2. Nf3");
    }

    #[test]
    fn marks_check_mate_castling_and_promotion() {
        // Fool's mate.
        assert_eq!(
            line_to_san(START, "f2f3 e7e5 g2g4 d8h4"),
            "1. f3 e5 2. g4 Qh4#"
        );
        let castle = "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1";
        assert_eq!(move_to_san(castle, "e1g1"), "O-O");
        let promo = "8/P7/8/8/8/8/8/k6K w - - 0 1";
        assert_eq!(move_to_san(promo, "a7a8q"), "a8=Q+");
    }

    #[test]
    fn keeps_illegal_or_garbage_moves_as_uci() {
        assert_eq!(line_to_san(START, "e2e4 e2e4 g1f3"), "1. e4 e2e4 g1f3");
        assert_eq!(move_to_san(START, "zz99"), "zz99");
        assert_eq!(line_to_san("not a fen", "e2e4"), "e2e4");
    }
}
