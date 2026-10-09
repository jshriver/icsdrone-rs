mod app;
mod board;
mod book;
mod config;
mod engine;
mod gui;
mod history;
mod ics;
mod pgn;
mod san;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use app::App;
use config::{Config, ConfigFile};
use gui::{GuiShared, LineKind};

/// How long to wait, after the window closes, for the bot to log off
/// and shut the engine down before exiting anyway.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// The ANSI color codes used by `Style12::to_ansi_board` (see
/// `board.rs`) work out of the box on Linux/macOS terminals and on
/// modern Windows terminals (Windows Terminal, PowerShell 7+), but
/// the classic Windows console (`cmd.exe`/`conhost.exe` on Windows 10
/// RTM-1511, and every default terminal before Windows 10) only
/// interprets them once "Virtual Terminal Processing" is turned on
/// for the process - it's supported by the OS since the Windows 10
/// Anniversary Update (1607) but off by default outside newer
/// terminal apps. This flips that switch so `ColorBoard: Yes` (the
/// default - see `config.rs`) is portable rather than printing raw
/// escape codes as garbage on an unprepared console. A no-op on
/// Linux/macOS, and harmless if it fails (e.g. stdout isn't a real
/// console, such as when piped to a file) - worst case then is the
/// same raw-escape-code garbage a user would get without this call,
/// so any failure is intentionally swallowed rather than treated as
/// fatal.
#[cfg(windows)]
fn enable_windows_ansi_support() {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        STD_OUTPUT_HANDLE,
    };

    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) != 0 {
            SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(windows))]
fn enable_windows_ansi_support() {}

/// With the GUI on, let go of the console window Windows gives every
/// console program, so double-clicking the .exe shows just the GUI
/// rather than an empty console beside it. (Started from an existing
/// terminal, this only detaches from it.) A no-op elsewhere.
#[cfg(windows)]
fn detach_windows_console() {
    unsafe {
        windows_sys::Win32::System::Console::FreeConsole();
    }
}

#[cfg(not(windows))]
fn detach_windows_console() {}

/// WAYLAND_DISPLAY is set, but the socket it names doesn't exist.
#[cfg(target_os = "linux")]
fn wayland_socket_missing() -> bool {
    let Some(name) = std::env::var_os("WAYLAND_DISPLAY").filter(|n| !n.is_empty()) else {
        return false;
    };
    let path = std::path::Path::new(&name);
    let socket = match std::env::var_os("XDG_RUNTIME_DIR") {
        _ if path.is_absolute() => path.to_path_buf(),
        Some(dir) => std::path::Path::new(&dir).join(path),
        None => return true,
    };
    !socket.exists()
}

#[cfg(not(target_os = "linux"))]
fn wayland_socket_missing() -> bool {
    false
}

fn main() -> Result<()> {
    let config = Config::parse();

    // Everything else (connection, engine, book, GUI) is configured in
    // the JSON config file. Read first, since it decides whether the
    // terminal is used at all; a broken file is still reported there.
    let config_file = ConfigFile::load(&config.config)?;
    let gui = config_file.resolve_gui();

    // With the GUI on, the window is the whole interface: nothing is
    // written to the terminal (so it can run as a standalone app),
    // unless RUST_LOG asks for logs explicitly.
    if gui {
        detach_windows_console();
        // Mesa's EGL (e.g. under WSLg) prints driver warnings straight
        // to stderr while the window's renderer probes for GPUs.
        if std::env::var_os("EGL_LOG_LEVEL").is_none() {
            // SAFETY: still single-threaded - the runtime isn't built yet.
            unsafe { std::env::set_var("EGL_LOG_LEVEL", "fatal") };
        }
        // WSL sometimes loses /run/user/<uid>, leaving WAYLAND_DISPLAY
        // naming a socket that isn't there; the window then fails with
        // "Could not find wayland compositor". Use X11 instead when
        // there is one.
        if wayland_socket_missing() && std::env::var_os("DISPLAY").is_some() {
            // SAFETY: still single-threaded - the runtime isn't built yet.
            unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
        }
    } else {
        enable_windows_ansi_support();
    }
    if !gui || std::env::var_os("RUST_LOG").is_some() {
        // Logs go to stderr so they don't interleave with the
        // interactive `>` prompt / its output on stdout. Level is
        // `info` unless RUST_LOG says otherwise.
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
            )
            .init();
    }
    if config.config.exists() {
        tracing::info!("Loaded config from {}", config.config.display());
    } else {
        tracing::warn!(
            "Config file {} not found - using defaults (guest login, default engine)",
            config.config.display()
        );
    }

    let runtime = tokio::runtime::Runtime::new()?;
    // Typed commands, from the terminal prompt and the GUI console.
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

    let result = if gui {
        // The window has to own the main thread (macOS requires it),
        // so the bot runs on the runtime's background threads.
        let shared = GuiShared::new();
        let title = format!("icsdrone-rs - {}", config_file.resolve_username());
        let bot = runtime.spawn(run_bot(
            config,
            config_file,
            Some(shared.clone()),
            cmd_tx.clone(),
            cmd_rx,
        ));
        let gui_result = gui::run(shared, cmd_tx.clone(), title);
        // The window couldn't open (or crashed), so it can't show this
        // itself: the terminal is the only place left to say why.
        if let Err(e) = &gui_result {
            eprintln!("icsdrone-rs: {e:#}");
        }

        // The window is closed - by the user, or because the bot quit.
        // Make sure the bot logs off and stops the engine either way.
        let _ = cmd_tx.send("quit".to_string());
        let bot_result =
            runtime.block_on(async { tokio::time::timeout(SHUTDOWN_TIMEOUT, bot).await });
        gui_result.and(match bot_result {
            Ok(joined) => joined.unwrap_or_else(|e| Err(e.into())),
            Err(_) => {
                tracing::warn!("bot did not shut down within {SHUTDOWN_TIMEOUT:?}; exiting anyway");
                Ok(())
            }
        })
    } else {
        runtime.block_on(run_bot(config, config_file, None, cmd_tx, cmd_rx))
    };

    // Don't wait on the terminal prompt's reader, which may be blocked
    // reading stdin.
    runtime.shutdown_background();

    // The window already showed any error; exit with a failure code
    // without printing it.
    if gui && result.is_err() {
        std::process::exit(1);
    }
    result
}

/// Connect, play until told to quit (or the connection fails), then
/// shut the engine down. With the GUI on, the window is told how it
/// ended: a normal quit closes it, an error stays on screen.
async fn run_bot(
    config: Config,
    config_file: ConfigFile,
    gui: Option<Arc<GuiShared>>,
    cmd_tx: mpsc::UnboundedSender<String>,
    cmd_rx: mpsc::UnboundedReceiver<String>,
) -> Result<()> {
    let result = async {
        let mut app = App::connect_and_login(&config, &config_file, gui.clone()).await?;
        let result = app.run(cmd_tx, cmd_rx).await;
        app.shutdown().await?;
        result
    }
    .await;

    // Logged here rather than left to main() so a failure during
    // startup (bad engine path, missing book, no connection) shows up
    // on the terminal right away, even while the window stays open.
    if let Err(e) = &result {
        tracing::error!("fatal error: {e:#}");
    }

    if let Some(gui) = &gui {
        gui.update(|s| match &result {
            Ok(()) => s.finished = true,
            Err(e) => {
                s.connected = false;
                s.error = Some(format!("{e:#}"));
                s.push_console(LineKind::Error, format!("Error: {e:#}"));
            }
        });
    }
    result
}
