//! Optional Polyglot opening book support, via the `polyglot-book-rs`
//! crate. When a book is configured (`"Book"` in config.json), it's
//! consulted for a move before falling back to the engine each turn -
//! see `App::handle_style12`. This only ever *shortcuts* a search; it
//! never changes what the engine itself does.

use anyhow::{Context, Result};
use polyglot_book_rs::PolyglotBook;
use std::path::Path;
use tracing::info;

/// A loaded Polyglot (`.bin`) opening book, ready to be queried by FEN.
pub struct OpeningBook {
    book: PolyglotBook,
}

impl OpeningBook {
    /// Load a Polyglot book from `path`. Unlike `ConfigFile::load`, a
    /// missing/unreadable/malformed file here is always an error - the
    /// book path is only present in config.json at all if the operator
    /// asked for one, so silently playing without it would be
    /// surprising.
    pub fn load(path: &Path) -> Result<Self> {
        let path_str = path
            .to_str()
            .with_context(|| format!("book path {} is not valid UTF-8", path.display()))?;
        let book = PolyglotBook::load(path_str)
            .with_context(|| format!("failed to load opening book {}", path.display()))?;
        info!(
            "Loaded opening book {} ({} entries)",
            path.display(),
            book.entry_count()
        );
        Ok(OpeningBook { book })
    }

    /// Look up the highest-weighted book move for a position, in the
    /// same "e2e4"/"e7e8q" move-string form the engine's own `bestmove`
    /// uses, so it can be sent to the ICS the same way. Returns `None`
    /// if the position isn't in the book (i.e. we're out of book and
    /// should defer to the engine).
    pub fn best_move_from_fen(&self, fen: &str) -> Option<String> {
        self.book
            .get_best_move_from_fen(fen)
            .map(|entry| normalize_castling(&entry.move_string))
    }
}

/// The Polyglot *file format* itself (not this crate specifically)
/// encodes castling as the king "capturing" its own rook - e1h1 for
/// white short, e1a1 for white long, e8h8/e8a8 for black - rather than
/// the king's actual destination square, a convention carried over
/// from its Chess960 support. See the spec:
/// <https://hgm.nubati.net/book_format.html> ("Castling moves are
/// represented somewhat unconventially..."). `polyglot-book-rs`
/// returns that raw encoding as-is, but FICS (and UCI engines) expect
/// the king's real destination (e1g1/e1c1/e8g8/e8c8), so translate
/// before handing a book move back to the caller.
fn normalize_castling(mv: &str) -> String {
    match mv {
        "e1h1" => "e1g1".to_string(),
        "e1a1" => "e1c1".to_string(),
        "e8h8" => "e8g8".to_string(),
        "e8a8" => "e8c8".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Hand-assembles a minimal Polyglot `.bin` book (the format is a
    /// flat array of big-endian 16-byte records: 8-byte position key,
    /// 2-byte move, 2-byte weight, 4-byte learn) from a list of
    /// (position hash, from-square, to-square, weight) entries, so the
    /// wrapper can be tested without depending on a real book file.
    /// Square numbers follow the format's `rank*8+file` convention
    /// (a1=0 ... h1=7, a8=56 ... h8=63).
    fn write_test_book(entries: &[(u64, u8, u8, u16)]) -> std::path::PathBuf {
        // Tests run in parallel within one process, so the pid alone
        // isn't enough to keep each test's book file to itself.
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "icsdrone_test_book_{}_{}.bin",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut f = std::fs::File::create(&path).unwrap();
        for &(key, from_sq, to_sq, weight) in entries {
            let mv: u16 = ((from_sq as u16) << 6) | to_sq as u16;
            let learn: u32 = 0;
            f.write_all(&key.to_be_bytes()).unwrap();
            f.write_all(&mv.to_be_bytes()).unwrap();
            f.write_all(&weight.to_be_bytes()).unwrap();
            f.write_all(&learn.to_be_bytes()).unwrap();
        }
        path
    }

    // Polyglot key for the standard starting position, per the test
    // vectors in the format spec.
    const STARTING_POS_KEY: u64 = 0x463b96181691fc9c;
    const STARTING_POS_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

    #[test]
    fn finds_book_move_for_starting_position() {
        // e2e4: from=e2 (sq 12), to=e4 (sq 28), no promotion/castling.
        let path = write_test_book(&[(STARTING_POS_KEY, 12, 28, 100)]);
        let book = OpeningBook::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(book.best_move_from_fen(STARTING_POS_FEN), Some("e2e4".to_string()));
    }

    #[test]
    fn returns_none_for_position_not_in_book() {
        let path = write_test_book(&[(STARTING_POS_KEY, 12, 28, 100)]);
        let book = OpeningBook::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        let random_pos = "8/8/8/4k3/8/8/8/4K3 w - - 0 1";
        assert_eq!(book.best_move_from_fen(random_pos), None);
    }

    #[test]
    fn missing_book_file_is_an_error() {
        let result = OpeningBook::load(Path::new("/nonexistent/definitely-not-here.bin"));
        assert!(result.is_err());
    }

    #[test]
    fn normalizes_white_short_castle() {
        // e1 (sq 4) "captures" h1 (sq 7) per the Polyglot encoding.
        let path = write_test_book(&[(STARTING_POS_KEY, 4, 7, 100)]);
        let book = OpeningBook::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(book.best_move_from_fen(STARTING_POS_FEN), Some("e1g1".to_string()));
    }

    #[test]
    fn normalizes_white_long_castle() {
        // e1 (sq 4) "captures" a1 (sq 0).
        let path = write_test_book(&[(STARTING_POS_KEY, 4, 0, 100)]);
        let book = OpeningBook::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(book.best_move_from_fen(STARTING_POS_FEN), Some("e1c1".to_string()));
    }

    #[test]
    fn normalizes_black_short_castle() {
        // e8 (sq 60) "captures" h8 (sq 63).
        let path = write_test_book(&[(STARTING_POS_KEY, 60, 63, 100)]);
        let book = OpeningBook::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(book.best_move_from_fen(STARTING_POS_FEN), Some("e8g8".to_string()));
    }

    #[test]
    fn normalizes_black_long_castle() {
        // e8 (sq 60) "captures" a8 (sq 56).
        let path = write_test_book(&[(STARTING_POS_KEY, 60, 56, 100)]);
        let book = OpeningBook::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(book.best_move_from_fen(STARTING_POS_FEN), Some("e8c8".to_string()));
    }

    #[test]
    fn leaves_non_castling_moves_unchanged() {
        assert_eq!(normalize_castling("e2e4"), "e2e4");
        assert_eq!(normalize_castling("e7e8q"), "e7e8q");
        assert_eq!(normalize_castling("g1f3"), "g1f3");
    }
}

