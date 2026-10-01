use anyhow::{Context, Result};
use clap::Parser;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Default ICS hostname, used when `Host` is absent from config.json.
const DEFAULT_ICS_HOST: &str = "nightmare-chess.nl";
/// Default ICS port, used when `Port` is absent from config.json.
const DEFAULT_ICS_PORT: u16 = 5000;
/// Default UCI engine command, used when neither `--engine` on the
/// command line nor `Engine` in config.json is set.
const DEFAULT_ENGINE_COMMAND: &str = "engine";

/// icsdrone-rs: bridges an Internet Chess Server (ICS, e.g. FICS) to a
/// UCI chess engine. Rust rewrite of icsdroneng, using UCI instead of
/// the original xboard/CECP protocol to talk to the engine.
///
/// ICS connection details (host, port, username, password) live in
/// the JSON config file (see `--config`), not on the command line -
/// see `ConfigFile`.
#[derive(Parser, Debug, Clone)]
#[command(name = "icsdrone-rs", version, about)]
pub struct Config {
    /// Command line used to launch the UCI engine, e.g. "engine" or
    /// "/path/to/engine --some-flag". Overrides `Engine` in config.json
    /// if both are set; falls back to `Engine` from config.json, then
    /// to "engine", if omitted.
    #[arg(long)]
    pub engine: Option<String>,

    /// Base time in minutes for the match clock (used only for our own
    /// bookkeeping / time management, not sent at login)
    #[arg(long, default_value_t = 5)]
    pub base_minutes: u32,

    /// Increment in seconds
    #[arg(long, default_value_t = 0)]
    pub increment_seconds: u32,

    /// Log level: error, warn, info, debug, trace
    #[arg(long, default_value = "info")]
    pub log_level: String,

    /// Path to a JSON config file with the ICS connection details
    /// (Host/Port/Username/Password), UCI engine options, and an
    /// opening book. Silently uses defaults for anything not present
    /// if the file doesn't exist; an error if it exists but is
    /// malformed.
    #[arg(long, default_value = "config.json")]
    pub config: PathBuf,

    /// Append a raw transcript of everything sent to and received from
    /// the ICS to this file (timestamped, control bytes escaped). The
    /// password is masked, so the log is safe to share.
    #[arg(long, value_name = "FILE")]
    pub debug: Option<PathBuf>,
}

/// Whether to announce search stats after each move as
/// "depth=<d> score=<pawns> time=<s> node=<n> nps=<n> pv=<moves>"
/// via ICS "whisper" (visible only to observers of the game) and on
/// our own console (visible to the operator running the bot, even if
/// they aren't separately observing the game). Controlled entirely by
/// `Kibitz` in config.json (Yes/No) - there is deliberately no way to
/// broadcast to the whole channel/room, only on/off. Whisper on by
/// default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KibitzMode {
    /// Don't announce anything.
    Off,
    /// Send via ICS "whisper" (visible only to observers of the game).
    #[default]
    Whisper,
}

impl KibitzMode {
    /// The ICS command word to prefix the announcement with, if any.
    pub fn ics_command(self) -> Option<&'static str> {
        match self {
            KibitzMode::Off => None,
            KibitzMode::Whisper => Some("whisper"),
        }
    }

    /// Parse a `Kibitz` value from config.json. Case-insensitive and
    /// tolerant of surrounding whitespace, so "Yes", "yes", "YES",
    /// " yEs " etc. all mean on, and "No"/"no"/"NO" etc. all mean off.
    /// Anything else is treated as "No" (safe default: no chatter).
    fn from_str_flatten(s: &str) -> Self {
        if s.trim().eq_ignore_ascii_case("yes") {
            KibitzMode::Whisper
        } else {
            KibitzMode::Off
        }
    }
}

/// Contents of the optional JSON config file (`--config`, default
/// `config.json`). Carries the ICS connection details, UCI options to
/// send to the engine at startup via `setoption name <k> value <v>`,
/// and an optional Polyglot opening book, e.g.:
///
/// ```json
/// {
///   "Host": "nightmare-chess.nl",
///   "Port": 5000,
///   "Username": "myhandle",
///   "Password": "mypassword",
///   "Engine": "engine",
///   "Kibitz": "Yes",
///   "ColorBoard": "Yes",
///   "DisplayBoard": "True",
///   "Timeseal": "Yes",
///   "engine_options": {
///     "Hash": "1024",
///     "Threads": "4",
///     "SyzygyPath": "/path/to/syzygy",
///     "Ponder": "false",
///     "OwnBook": "false",
///     "NNUE": "true"
///   },
///   "Book": "file.bin"
/// }
/// ```
///
/// `Host` and `Port` default to nightmare-chess.nl:5000 if absent.
/// `Username`/`Password` fall back to $FICSHANDLE/$ICSHANDLE and
/// $FICSPASSWD/$ICSPASSWD respectively, then to a guest login if
/// still unset - see `resolve_username`/`resolve_password`.
///
/// `Engine` is the command line used to launch the UCI engine, same
/// idea as `--engine` on the command line. `--engine`, if given, takes
/// precedence over `Engine` here; if neither is set, it falls back to
/// "engine" - see `resolve_engine`.
///
/// `Kibitz` turns search-stat announcements (via ICS "whisper", never
/// the public "kibitz" channel) on or off. Accepts "Yes"/"No" in any
/// case or spacing ("yes", "YES", " yEs " all mean on); defaults to
/// "Yes" (whisper on) if absent - see `resolve_kibitz`.
///
/// `ColorBoard` turns ANSI move-highlighting on the console board on
/// or off. Same "Yes"/"No" parsing as `Kibitz`; defaults to "Yes"
/// (colored) if absent - see `resolve_color_board`.
///
/// `DisplayBoard` turns the console board display itself on or off -
/// when "False", the board is never printed at all, regardless of
/// `ColorBoard`. Accepts "True"/"False" in any case or spacing;
/// defaults to "True" if absent - see `resolve_display_board`.
///
/// `Timeseal` turns on timeseal v1 encoding of everything we send, so
/// the server charges us for thinking time only, not network lag.
/// "Yes"/"No" like `Kibitz`; defaults to "No", since a server without
/// timeseal support would read the encoded lines as garbage - see
/// `resolve_timeseal`.
///
/// Keys/values in `engine_options` are sent to the engine as-is via
/// `setoption name <k> value <v>`, so any option the engine supports
/// can be set this way, not just Hash/Threads/SyzygyPath. `Ponder`,
/// `OwnBook`, and `NNUE` are UCI "check" (boolean) options like any
/// other - they just come with built-in defaults (`"false"`,
/// `"false"`, and `"true"` respectively) applied when the key isn't
/// present under `engine_options` at all, so they don't need to be
/// listed explicitly for the common case - see
/// `resolve_engine_options`.
///
/// `Book` is a path (absolute, or relative to the current working
/// directory) to a Polyglot (`.bin`) opening book. When set, it's
/// consulted for a book move before invoking the engine each turn -
/// see `book::OpeningBook`.
#[derive(serde::Deserialize, Debug, Clone, Default)]
pub struct ConfigFile {
    #[serde(default, rename = "Host")]
    pub host: Option<String>,

    #[serde(default, rename = "Port")]
    pub port: Option<u16>,

    #[serde(default, rename = "Username")]
    pub username: Option<String>,

    #[serde(default, rename = "Password")]
    pub password: Option<String>,

    #[serde(default, rename = "Engine")]
    pub engine: Option<String>,

    #[serde(default, rename = "Kibitz")]
    pub kibitz: Option<String>,

    /// Whether the console board (`to_board_string` via `println!` in
    /// `app.rs`) is drawn with ANSI colors highlighting the previous
    /// move's from/to squares, or left as the plain uncolored board.
    /// Accepts "Yes"/"No" in any case or spacing, same as `Kibitz`;
    /// defaults to "Yes" (colored) if absent - see
    /// `resolve_color_board`.
    #[serde(default, rename = "ColorBoard")]
    pub color_board: Option<String>,

    /// Whether the console board is displayed at all. Unlike
    /// `ColorBoard` (which only controls styling), setting this to
    /// `"False"` suppresses the board output entirely. Accepts
    /// "True"/"False" in any case or spacing, same parsing as
    /// `ColorBoard`/`Kibitz`; defaults to "True" (display it, as
    /// before this option existed) if absent - see
    /// `resolve_display_board`.
    #[serde(default, rename = "DisplayBoard")]
    pub display_board: Option<String>,

    /// Whether to timeseal-encode our output - see `resolve_timeseal`.
    #[serde(default, rename = "Timeseal")]
    pub timeseal: Option<String>,

    #[serde(default)]
    pub engine_options: BTreeMap<String, String>,

    #[serde(default, rename = "Book")]
    pub book: Option<PathBuf>,
}

impl ConfigFile {
    /// Load from `path`. A missing file is not an error - it just means
    /// every field falls back to its default - but a file that exists
    /// and fails to parse is. Either way, logs which happened: a
    /// missing config file and a present-but-typo'd filename both
    /// silently fall back to defaults otherwise, which is confusing to
    /// debug (e.g. seeing a guest login and a default engine command
    /// with no indication config.json was never read at all).
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let cfg = serde_json::from_str(&contents)
                    .with_context(|| format!("failed to parse {}", path.display()))?;
                tracing::info!("Loaded config from {}", path.display());
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    "Config file {} not found - using defaults (guest login, default engine)",
                    path.display()
                );
                Ok(ConfigFile::default())
            }
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    /// ICS hostname to connect to: `Host` from config.json, or
    /// nightmare-chess.nl if unset.
    pub fn resolve_host(&self) -> String {
        self.host
            .clone()
            .unwrap_or_else(|| DEFAULT_ICS_HOST.to_string())
    }

    /// ICS port to connect to: `Port` from config.json, or 5000 if
    /// unset.
    pub fn resolve_port(&self) -> u16 {
        self.port.unwrap_or(DEFAULT_ICS_PORT)
    }

    /// ICS handle (login name): `Username` from config.json, falling
    /// back to $FICSHANDLE / $ICSHANDLE, then to "guest".
    pub fn resolve_username(&self) -> String {
        self.username
            .clone()
            .or_else(|| std::env::var("FICSHANDLE").ok())
            .or_else(|| std::env::var("ICSHANDLE").ok())
            .unwrap_or_else(|| "guest".to_string())
    }

    /// ICS password: `Password` from config.json, falling back to
    /// $FICSPASSWD / $ICSPASSWD. Leave unset (on all three) to log in
    /// as guest.
    pub fn resolve_password(&self) -> Option<String> {
        self.password
            .clone()
            .or_else(|| std::env::var("FICSPASSWD").ok())
            .or_else(|| std::env::var("ICSPASSWD").ok())
    }

    /// Command line used to launch the UCI engine: `--engine` on the
    /// command line if given (it always wins, even over `Engine` in
    /// config.json), else `Engine` from config.json, else
    /// `DEFAULT_ENGINE_COMMAND`.
    pub fn resolve_engine(&self, cli_engine: Option<&str>) -> String {
        cli_engine
            .map(str::to_string)
            .or_else(|| self.engine.clone())
            .unwrap_or_else(|| DEFAULT_ENGINE_COMMAND.to_string())
    }

    /// Path to the configured Polyglot opening book, or `None` if no
    /// book should be used. Treats `Book` being absent from
    /// config.json *and* `Book` being present but blank (`""` or
    /// whitespace-only - e.g. a template config that left the field
    /// empty rather than removing it) the same way: no book. A
    /// non-blank path is returned as-is and is not checked for
    /// existence here - that happens (and is an error if it fails)
    /// when the caller actually tries to load it.
    pub fn resolve_book_path(&self) -> Option<&Path> {
        self.book.as_deref().filter(|p| {
            p.to_str().map(|s| !s.trim().is_empty()).unwrap_or(true)
        })
    }

    /// Whether to whisper search stats after each move: `Kibitz` from
    /// config.json, flattened case-/whitespace-insensitively to
    /// Yes/No ("yes", "YES", " yEs " etc. all count as "Yes"; anything
    /// else, including an unrecognized value, is treated as "No").
    /// Defaults to "Yes" (whisper on) if `Kibitz` is absent entirely.
    pub fn resolve_kibitz(&self) -> KibitzMode {
        match &self.kibitz {
            Some(s) => KibitzMode::from_str_flatten(s),
            None => KibitzMode::default(),
        }
    }

    /// Whether to draw the console board with ANSI move highlighting:
    /// `ColorBoard` from config.json, case-/whitespace-insensitively
    /// flattened to Yes/No. Defaults to "Yes" (colored) if
    /// `ColorBoard` is absent entirely. Unlike `Kibitz` (where an
    /// unrecognized value falls back to the quieter "No"), an
    /// unrecognized `ColorBoard` value falls back to the default
    /// "Yes" - the display is purely cosmetic, so there's no safety
    /// reason to prefer the plain board, and only an explicit "No"
    /// should turn coloring off.
    pub fn resolve_color_board(&self) -> bool {
        match &self.color_board {
            Some(s) => !s.trim().eq_ignore_ascii_case("no"),
            None => true,
        }
    }

    /// Whether to display the console board at all: `DisplayBoard`
    /// from config.json, defaulting to "True" (display it, matching
    /// behavior before this option existed) if absent. Same
    /// "unrecognized value falls back to the default" reasoning as
    /// `resolve_color_board` above - only an explicit "False" should
    /// turn the board off.
    pub fn resolve_display_board(&self) -> bool {
        match &self.display_board {
            Some(s) => !s.trim().eq_ignore_ascii_case("false"),
            None => true,
        }
    }

    /// Whether to speak timeseal: `Timeseal` from config.json. Only an
    /// explicit "Yes" (any case/spacing) turns it on - unlike the
    /// display options, guessing wrong here breaks the connection, so
    /// absent or unrecognized values mean plain text.
    pub fn resolve_timeseal(&self) -> bool {
        self.timeseal
            .as_deref()
            .is_some_and(|s| s.trim().eq_ignore_ascii_case("yes"))
    }

    /// The full set of UCI options to send to the engine at startup:
    /// `engine_options` from config.json, with `Ponder`, `OwnBook`,
    /// and `NNUE` (UCI "check"/boolean options) defaulted to
    /// `"false"`, `"false"`, and `"true"` respectively for any of
    /// those three keys not already present - so they don't have to
    /// be listed explicitly for the common case, but an explicit
    /// `"Ponder": "false"` (etc.) under `engine_options` always wins
    /// over the default.
    pub fn resolve_engine_options(&self) -> BTreeMap<String, String> {
        let mut opts = self.engine_options.clone();
        opts.entry("Ponder".to_string())
            .or_insert_with(|| "false".to_string());
        opts.entry("OwnBook".to_string())
            .or_insert_with(|| "false".to_string());
        opts.entry("NNUE".to_string())
            .or_insert_with(|| "true".to_string());
        opts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_file_is_not_an_error() {
        let cfg = ConfigFile::load(Path::new("/nonexistent/definitely-not-here.json")).unwrap();
        assert!(cfg.engine_options.is_empty());
        assert!(cfg.book.is_none());
        assert_eq!(cfg.resolve_host(), "nightmare-chess.nl");
        assert_eq!(cfg.resolve_port(), 5000);
    }

    #[test]
    fn parses_engine_options() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("icsdrone-test-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{"engine_options": {"Hash": "1024", "Threads": "4", "SyzygyPath": "/tbs"}}"#,
        )
        .unwrap();

        let cfg = ConfigFile::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(cfg.engine_options.get("Hash").map(String::as_str), Some("1024"));
        assert_eq!(cfg.engine_options.get("Threads").map(String::as_str), Some("4"));
        assert_eq!(
            cfg.engine_options.get("SyzygyPath").map(String::as_str),
            Some("/tbs")
        );
    }

    #[test]
    fn parses_book_path() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("icsdrone-test-book-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"Book": "file.bin"}"#).unwrap();

        let cfg = ConfigFile::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(cfg.book, Some(PathBuf::from("file.bin")));
    }

    #[test]
    fn book_is_none_when_unset() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("icsdrone-test-nobook-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"engine_options": {"Hash": "1024"}}"#).unwrap();

        let cfg = ConfigFile::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert!(cfg.book.is_none());
        assert!(cfg.resolve_book_path().is_none());
    }

    #[test]
    fn resolve_book_path_treats_blank_book_as_none() {
        // A template config.json that left "Book" present but empty
        // (or whitespace-only) shouldn't be treated as "load a book
        // from an empty path" - that should behave exactly like the
        // key being absent entirely.
        for blank in ["", "   ", "\t"] {
            let dir = std::env::temp_dir();
            let path = dir.join(format!(
                "icsdrone-test-blankbook-{}-{}.json",
                std::process::id(),
                blank.len()
            ));
            // Build the JSON via serde_json rather than interpolating
            // `blank` straight into a format! string: the "\t" case is
            // a literal tab character, and an unescaped control
            // character inside a JSON string is invalid JSON (it must
            // be written as the two characters `\t`) - serde_json's
            // serializer escapes it correctly, a hand-written format!
            // string does not.
            let json = serde_json::json!({ "Book": blank }).to_string();
            std::fs::write(&path, json).unwrap();

            let cfg = ConfigFile::load(&path).unwrap();
            std::fs::remove_file(&path).ok();

            assert!(
                cfg.resolve_book_path().is_none(),
                "blank Book value {blank:?} should resolve to no book"
            );
        }
    }

    #[test]
    fn resolve_book_path_returns_configured_path() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("icsdrone-test-realbook-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"Book": "file.bin"}"#).unwrap();

        let cfg = ConfigFile::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(cfg.resolve_book_path(), Some(Path::new("file.bin")));
    }

    #[test]
    fn parses_connection_fields() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("icsdrone-test-conn-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{"Host": "chess.example.com", "Port": 23, "Username": "bot1", "Password": "hunter2"}"#,
        )
        .unwrap();

        let cfg = ConfigFile::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(cfg.resolve_host(), "chess.example.com");
        assert_eq!(cfg.resolve_port(), 23);
        assert_eq!(cfg.resolve_username(), "bot1");
        assert_eq!(cfg.resolve_password(), Some("hunter2".to_string()));
    }

    #[test]
    fn connection_fields_default_when_absent() {
        let cfg = ConfigFile::default();
        assert_eq!(cfg.resolve_host(), "nightmare-chess.nl");
        assert_eq!(cfg.resolve_port(), 5000);
    }

    #[test]
    fn resolve_engine_prefers_cli_over_config_file() {
        let mut cfg = ConfigFile::default();
        cfg.engine = Some("config-engine".to_string());
        assert_eq!(cfg.resolve_engine(Some("cli-engine")), "cli-engine");
    }

    #[test]
    fn resolve_engine_falls_back_to_config_file() {
        let mut cfg = ConfigFile::default();
        cfg.engine = Some("config-engine".to_string());
        assert_eq!(cfg.resolve_engine(None), "config-engine");
    }

    #[test]
    fn resolve_engine_defaults_to_default_engine_command() {
        let cfg = ConfigFile::default();
        assert_eq!(cfg.resolve_engine(None), DEFAULT_ENGINE_COMMAND);
    }

    #[test]
    fn resolve_kibitz_defaults_to_whisper_when_absent() {
        let cfg = ConfigFile::default();
        assert_eq!(cfg.resolve_kibitz(), KibitzMode::Whisper);
    }

    #[test]
    fn resolve_kibitz_flattens_yes_variants() {
        for s in ["Yes", "yes", "YES", "yEs", " Yes ", "yes\n"] {
            let mut cfg = ConfigFile::default();
            cfg.kibitz = Some(s.to_string());
            assert_eq!(cfg.resolve_kibitz(), KibitzMode::Whisper, "input was {s:?}");
        }
    }

    #[test]
    fn resolve_kibitz_flattens_no_variants() {
        for s in ["No", "no", "NO", "nO", " No "] {
            let mut cfg = ConfigFile::default();
            cfg.kibitz = Some(s.to_string());
            assert_eq!(cfg.resolve_kibitz(), KibitzMode::Off, "input was {s:?}");
        }
    }

    #[test]
    fn resolve_kibitz_treats_garbage_as_no() {
        let mut cfg = ConfigFile::default();
        cfg.kibitz = Some("banana".to_string());
        assert_eq!(cfg.resolve_kibitz(), KibitzMode::Off);
    }

    #[test]
    fn resolve_timeseal_only_on_for_explicit_yes() {
        let mut cfg = ConfigFile::default();
        assert!(!cfg.resolve_timeseal());
        for (s, want) in [("Yes", true), (" yEs ", true), ("No", false), ("banana", false)] {
            cfg.timeseal = Some(s.to_string());
            assert_eq!(cfg.resolve_timeseal(), want, "input was {s:?}");
        }
    }

    #[test]
    fn resolve_color_board_defaults_to_yes_when_absent() {
        let cfg = ConfigFile::default();
        assert!(cfg.resolve_color_board());
    }

    #[test]
    fn resolve_color_board_flattens_no_variants() {
        for s in ["No", "no", "NO", "nO", " No ", "no\n"] {
            let mut cfg = ConfigFile::default();
            cfg.color_board = Some(s.to_string());
            assert!(!cfg.resolve_color_board(), "input was {s:?}");
        }
    }

    #[test]
    fn resolve_color_board_flattens_yes_variants() {
        for s in ["Yes", "yes", "YES", "yEs", " Yes "] {
            let mut cfg = ConfigFile::default();
            cfg.color_board = Some(s.to_string());
            assert!(cfg.resolve_color_board(), "input was {s:?}");
        }
    }

    #[test]
    fn resolve_color_board_treats_garbage_as_yes() {
        // Unlike Kibitz, an unrecognized ColorBoard value should keep
        // the (colored) default rather than silently turning it off.
        let mut cfg = ConfigFile::default();
        cfg.color_board = Some("banana".to_string());
        assert!(cfg.resolve_color_board());
    }

    #[test]
    fn parses_color_board_field() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("icsdrone-test-colorboard-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"ColorBoard": "No"}"#).unwrap();

        let cfg = ConfigFile::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert!(!cfg.resolve_color_board());
    }

    #[test]
    fn resolve_display_board_defaults_to_true_when_absent() {
        let cfg = ConfigFile::default();
        assert!(cfg.resolve_display_board());
    }

    #[test]
    fn resolve_display_board_flattens_false_variants() {
        for s in ["False", "false", "FALSE", "fAlSe", " False "] {
            let mut cfg = ConfigFile::default();
            cfg.display_board = Some(s.to_string());
            assert!(!cfg.resolve_display_board(), "input was {s:?}");
        }
    }

    #[test]
    fn resolve_display_board_flattens_true_variants() {
        for s in ["True", "true", "TRUE", "tRuE", " True "] {
            let mut cfg = ConfigFile::default();
            cfg.display_board = Some(s.to_string());
            assert!(cfg.resolve_display_board(), "input was {s:?}");
        }
    }

    #[test]
    fn resolve_display_board_treats_garbage_as_true() {
        // Same reasoning as ColorBoard: an unrecognized DisplayBoard
        // value should keep the (displayed) default rather than
        // silently turning the board off.
        let mut cfg = ConfigFile::default();
        cfg.display_board = Some("banana".to_string());
        assert!(cfg.resolve_display_board());
    }

    #[test]
    fn parses_display_board_field() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("icsdrone-test-displayboard-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"DisplayBoard": "False"}"#).unwrap();

        let cfg = ConfigFile::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert!(!cfg.resolve_display_board());
    }

    #[test]
    fn resolve_engine_options_fills_in_defaults() {
        let cfg = ConfigFile::default();
        let opts = cfg.resolve_engine_options();
        assert_eq!(opts.get("Ponder").map(String::as_str), Some("false"));
        assert_eq!(opts.get("OwnBook").map(String::as_str), Some("false"));
        assert_eq!(opts.get("NNUE").map(String::as_str), Some("true"));
    }

    #[test]
    fn resolve_engine_options_explicit_engine_options_wins() {
        // An explicit entry under `engine_options` for Ponder/OwnBook/
        // NNUE overrides the built-in default for that key.
        let mut cfg = ConfigFile::default();
        cfg.engine_options
            .insert("NNUE".to_string(), "false".to_string());
        let opts = cfg.resolve_engine_options();
        assert_eq!(opts.get("NNUE").map(String::as_str), Some("false"));
    }

    #[test]
    fn resolve_engine_options_preserves_other_engine_options() {
        let mut cfg = ConfigFile::default();
        cfg.engine_options
            .insert("Hash".to_string(), "1024".to_string());
        let opts = cfg.resolve_engine_options();
        assert_eq!(opts.get("Hash").map(String::as_str), Some("1024"));
        assert_eq!(opts.get("Ponder").map(String::as_str), Some("false"));
    }

    #[test]
    fn parses_ponder_own_book_nnue_from_engine_options() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("icsdrone-test-uciopts-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{"engine_options": {"Ponder": "true", "OwnBook": "true", "NNUE": "false"}}"#,
        )
        .unwrap();

        let cfg = ConfigFile::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        let opts = cfg.resolve_engine_options();
        assert_eq!(opts.get("Ponder").map(String::as_str), Some("true"));
        assert_eq!(opts.get("OwnBook").map(String::as_str), Some("true"));
        assert_eq!(opts.get("NNUE").map(String::as_str), Some("false"));
    }

    #[test]
    fn malformed_config_file_is_an_error() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("icsdrone-test-bad-{}.json", std::process::id()));
        std::fs::write(&path, "not json").unwrap();

        let result = ConfigFile::load(&path);
        std::fs::remove_file(&path).ok();

        assert!(result.is_err());
    }
}

