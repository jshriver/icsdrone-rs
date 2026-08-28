use anyhow::{Context, Result};
use clap::Parser;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Default ICS hostname, used when `Host` is absent from config.json.
const DEFAULT_ICS_HOST: &str = "nightmare-chess.nl";
/// Default ICS port, used when `Port` is absent from config.json.
const DEFAULT_ICS_PORT: u16 = 5000;

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
    /// Command line used to launch the UCI engine, e.g. "stockfish" or
    /// "/path/to/engine --some-flag". Overrides `Engine` in config.json
    /// if both are set; falls back to `Engine` from config.json, then
    /// to "stockfish", if omitted.
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
}

/// Whether to announce search stats after each move as
/// "depth=<d> score=<pawns> time=<s> node=<n> nps=<n> pv=<moves>"
/// via ICS "whisper" (visible only to observers of the game).
/// Controlled entirely by `Kibitz` in config.json (Yes/No) - there is
/// deliberately no way to broadcast to the whole channel/room, only
/// on/off. Whisper on by default.
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
///   "Engine": "stockfish",
///   "Kibitz": "Yes",
///   "engine_options": {
///     "Hash": "1024",
///     "Threads": "4",
///     "SyzygyPath": "/path/to/syzygy"
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
/// "stockfish" - see `resolve_engine`.
///
/// `Kibitz` turns search-stat announcements (via ICS "whisper", never
/// the public "kibitz" channel) on or off. Accepts "Yes"/"No" in any
/// case or spacing ("yes", "YES", " yEs " all mean on); defaults to
/// "Yes" (whisper on) if absent - see `resolve_kibitz`.
///
/// Keys/values in `engine_options` are sent to the engine as-is, so
/// any option the engine supports can be set this way, not just
/// Hash/Threads/SyzygyPath.
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

    #[serde(default)]
    pub engine_options: BTreeMap<String, String>,

    #[serde(default, rename = "Book")]
    pub book: Option<PathBuf>,
}

impl ConfigFile {
    /// Load from `path`. A missing file is not an error - it just means
    /// every field falls back to its default - but a file that exists
    /// and fails to parse is.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents)
                .with_context(|| format!("failed to parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ConfigFile::default()),
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
    /// config.json), else `Engine` from config.json, else "stockfish".
    pub fn resolve_engine(&self, cli_engine: Option<&str>) -> String {
        cli_engine
            .map(str::to_string)
            .or_else(|| self.engine.clone())
            .unwrap_or_else(|| "stockfish".to_string())
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
    fn resolve_engine_defaults_to_stockfish() {
        let cfg = ConfigFile::default();
        assert_eq!(cfg.resolve_engine(None), "stockfish");
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
    fn malformed_config_file_is_an_error() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("icsdrone-test-bad-{}.json", std::process::id()));
        std::fs::write(&path, "not json").unwrap();

        let result = ConfigFile::load(&path);
        std::fs::remove_file(&path).ok();

        assert!(result.is_err());
    }
}

