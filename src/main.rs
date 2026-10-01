mod app;
mod board;
mod book;
mod config;
mod engine;
mod ics;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use app::App;
use config::Config;

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

#[tokio::main]
async fn main() -> Result<()> {
    enable_windows_ansi_support();

    let config = Config::parse();

    // Logs go to stderr so they don't interleave with the interactive
    // `>` prompt / its output on stdout. Level is `info` unless the
    // RUST_LOG environment variable says otherwise.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut app = App::connect_and_login(&config).await?;

    let result = app.run().await;
    if let Err(e) = &result {
        tracing::error!("fatal error: {e:#}");
    }
    app.shutdown().await?;
    result
}
