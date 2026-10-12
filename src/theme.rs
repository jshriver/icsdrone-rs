//! Board and piece themes for the GUI, picked from the dropdowns above
//! the engine panel. Every image is compiled into the binary; see
//! `assets/pieces/README.md` and `assets/boards/README.md` for where
//! each came from and its license.

use std::borrow::Cow;

use eframe::egui::{self, Color32, ImageSource};

/// Piece letters in the order `PieceSet::images` holds them
/// (style12: uppercase = White).
const PIECE_ORDER: &[u8; 12] = b"KQRBNPkqrbnp";

pub struct PieceSet {
    /// Also accepted in config.json's `gui_options`; never shown.
    pub id: &'static str,
    /// As shown in the dropdown, and saved to `gui_options`.
    pub name: &'static str,
    /// `bytes://` URI and file contents, in `PIECE_ORDER`.
    images: [(&'static str, &'static [u8]); 12],
}

impl PieceSet {
    /// The image for a style12 piece letter.
    pub fn image(&self, piece: u8) -> Option<ImageSource<'static>> {
        let i = PIECE_ORDER.iter().position(|&p| p == piece)?;
        let (uri, bytes) = self.images[i];
        Some(source(uri, bytes))
    }
}

pub struct BoardTheme {
    pub id: &'static str,
    pub name: &'static str,
    /// The whole board as one picture.
    image: (&'static str, &'static [u8]),
    /// Average square colors: for coordinates, book moves, and the
    /// strip behind the material difference.
    pub light: Color32,
    pub dark: Color32,
}

impl BoardTheme {
    pub fn image(&self) -> ImageSource<'static> {
        let (uri, bytes) = self.image;
        source(uri, bytes)
    }
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn source(uri: &'static str, bytes: &'static [u8]) -> ImageSource<'static> {
    ImageSource::Bytes {
        uri: Cow::Borrowed(uri),
        bytes: egui::load::Bytes::Static(bytes),
    }
}

/// One embedded asset as (`bytes://` URI, contents). The URI keeps the
/// file extension, which is how egui picks the SVG or image loader.
macro_rules! asset {
    ($path:expr) => {
        (concat!("bytes://", $path), include_bytes!($path))
    };
}

macro_rules! piece_set {
    ($id:literal, $name:literal) => {
        PieceSet {
            id: $id,
            name: $name,
            images: [
                asset!(concat!("../assets/pieces/", $id, "/wK.svg")),
                asset!(concat!("../assets/pieces/", $id, "/wQ.svg")),
                asset!(concat!("../assets/pieces/", $id, "/wR.svg")),
                asset!(concat!("../assets/pieces/", $id, "/wB.svg")),
                asset!(concat!("../assets/pieces/", $id, "/wN.svg")),
                asset!(concat!("../assets/pieces/", $id, "/wP.svg")),
                asset!(concat!("../assets/pieces/", $id, "/bK.svg")),
                asset!(concat!("../assets/pieces/", $id, "/bQ.svg")),
                asset!(concat!("../assets/pieces/", $id, "/bR.svg")),
                asset!(concat!("../assets/pieces/", $id, "/bB.svg")),
                asset!(concat!("../assets/pieces/", $id, "/bN.svg")),
                asset!(concat!("../assets/pieces/", $id, "/bP.svg")),
            ],
        }
    };
}

macro_rules! board {
    ($id:literal, $name:literal, $file:literal, $light:literal, $dark:literal) => {
        BoardTheme {
            id: $id,
            name: $name,
            image: asset!(concat!("../assets/boards/", $file)),
            light: rgb($light),
            dark: rgb($dark),
        }
    };
}

/// The first is the default.
pub static PIECE_SETS: &[PieceSet] = &[
    piece_set!("merida", "Merida"),
    // From Lichess.
    piece_set!("cburnett", "Cburnett"),
    piece_set!("celtic", "Celtic"),
    piece_set!("chessnut", "Chessnut"),
    piece_set!("fantasy", "Fantasy"),
    piece_set!("firi", "Firi"),
    piece_set!("governor", "Governor"),
    piece_set!("kiwen-suwi", "Kiwen-suwi"),
    piece_set!("kosal", "Kosal"),
    piece_set!("letter", "Letter"),
    piece_set!("mpchess", "Mpchess"),
    piece_set!("pirouetti", "Pirouetti"),
    piece_set!("pixel", "Pixel"),
    piece_set!("rhosgfx", "Rhosgfx"),
    piece_set!("shapes", "Shapes"),
    piece_set!("spatial", "Spatial"),
    // Armando Marroquin's diagram fonts, via tools/ttf_pieces.py.
    piece_set!("adventurer", "Adventurer"),
    piece_set!("alfonso-x", "Alfonso-X"),
    piece_set!("condal", "Condal"),
    piece_set!("harlequin", "Harlequin"),
    piece_set!("kingdom", "Kingdom"),
    piece_set!("leipzig", "Leipzig"),
    piece_set!("line", "Line"),
    piece_set!("lucena", "Lucena"),
    piece_set!("magnetic", "Magnetic"),
    piece_set!("mark", "Mark"),
    piece_set!("marroquin", "Marroquin"),
    piece_set!("mediaeval", "Mediaeval"),
    piece_set!("merida-font", "Merida (font)"),
    piece_set!("millennia", "Millennia"),
    piece_set!("motif", "Motif"),
];

/// The first is the default. Colors are each picture's average light
/// and dark square.
pub static BOARDS: &[BoardTheme] = &[
    // The default, from Lichess.
    board!("blue3", "Blue 3", "blue3.jpg", 0xcad3dc, 0x568ab6),
    // From Lichess.
    board!("blue", "Blue", "blue.png", 0xdee3e6, 0x8ca2ad),
    board!("blue2", "Blue 2", "blue2.jpg", 0x88a1b6, 0x657e91),
    board!("blue-marble", "Blue marble", "blue-marble.jpg", 0xe5e1d7, 0x959daa),
    board!("brown", "Brown", "brown.png", 0xf0d9b5, 0xb58863),
    board!("canvas", "Canvas", "canvas2.jpg", 0xcccfe1, 0x71819b),
    board!("green", "Green", "green.png", 0xffffdd, 0x86a666),
    board!("green-plastic", "Green plastic", "green-plastic.png", 0xf1f6b2, 0x59935d),
    board!("grey", "Grey", "grey.jpg", 0xa9a9a9, 0x878787),
    board!("ic", "IC", "ic.png", 0xececec, 0xc1c18e),
    board!("leather", "Leather", "leather.jpg", 0xcecec6, 0xc08b12),
    board!("maple", "Maple", "maple.jpg", 0xdfbe92, 0xb97642),
    board!("maple2", "Maple 2", "maple2.jpg", 0xe0c69e, 0xac765c),
    board!("marble", "Marble", "marble.jpg", 0x839984, 0x647b64),
    board!("metal", "Metal", "metal.jpg", 0xc8c8c8, 0x919191),
    board!("ncf", "NCF", "ncf-board.png", 0xbbcfff, 0x5477ca),
    board!("newspaper", "Newspaper", "newspaper.svg", 0xdbdbdb, 0x5f5f5f),
    board!("olive", "Olive", "olive.jpg", 0xada694, 0x837b69),
    board!("pink", "Pink pyramid", "pink-pyramid.png", 0xedeebf, 0xf27575),
    board!("purple", "Purple", "purple.png", 0x9f90b0, 0x7d4a8d),
    board!("purple-diag", "Purple diagonal", "purple-diag.png", 0xe3d8ee, 0x977cb3),
    board!("wood", "Wood", "wood.jpg", 0xd8a257, 0x975221),
    board!("wood2", "Wood 2", "wood2.jpg", 0x9d8355, 0x7e6435),
    board!("wood3", "Wood 3", "wood3.jpg", 0xbfb5ac, 0x8f6e47),
    board!("wood4", "Wood 4", "wood4.jpg", 0xc4a470, 0x7e5532),
    // Hatched squares from the Marroquin font, via tools/ttf_pieces.py.
    // Darker than its average, so coordinates read on white squares.
    board!("diagram", "Diagram", "diagram.svg", 0xffffff, 0x5f5f5f),
];

/// Does config.json's `wanted` name this theme? Its shown name or its
/// id, ignoring case and surrounding spaces.
fn named(wanted: &str, name: &str, id: &str) -> bool {
    let wanted = wanted.trim();
    wanted.eq_ignore_ascii_case(name) || wanted.eq_ignore_ascii_case(id)
}

/// Index of the set config.json names, or the default.
pub fn piece_set_index(wanted: Option<&str>) -> usize {
    wanted
        .and_then(|w| PIECE_SETS.iter().position(|s| named(w, s.name, s.id)))
        .unwrap_or(0)
}

/// Index of the board config.json names, or the default.
pub fn board_index(wanted: Option<&str>) -> usize {
    wanted
        .and_then(|w| BOARDS.iter().position(|b| named(w, b.name, b.id)))
        .unwrap_or(0)
}
