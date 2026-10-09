# icsdrone-rs

A Rust rewrite inspired by icsdrone that bridges an Internet Chess Server to a chess engine — but talks
**UCI** to the engine instead of the original's **xboard/CECP** protocol.

## Usage

```
cargo run --release
```

The release build uses thin LTO. For quicker builds while testing,
there's also an optimized profile without LTO:

```
cargo build --profile quick     # -> target/quick/icsdrone-rs
```

Everything - ICS connection, engine, display (terminal board or GUI),
timeseal, opening book, saving games - is configured in
`config.json`; see "Config file" below. Copy `config.example.json` to
`config.json` to get started. Leave `Password` unset (in config.json,
and in `FICSPASSWD`/`ICSPASSWD`) to log in as a guest.

To keep a record of every game the bot plays, set `SavePGN` to a file
name, e.g. `"SavePGN": "games.pgn"`: each game is appended to it as
PGN when it ends, ready to open in any chess program. Every move
carries the clock time left after it (`[%clk 0:04:58]`), and the
bot's own moves its engine's score/depth and the time it took
(`{+0.35/18 2.1s}`, or `{book}` for book moves). Leave it out or
set it to `""` to not save games.

### Command-line flags

Both optional:

| Flag | Default | Meaning |
|---|---|---|
| `--config <path>` | `config.json` | Path to the JSON config file (see "Config file" below). A missing file is fine (defaults/guest login apply); a malformed one is an error. |
| `--debug <file>` | (off) | Append a timestamped, raw transcript of everything sent to and received from the ICS to `<file>` — see "Debugging the ICS connection" below. |

`--help` lists the flags and `--version` prints the version.

Logs go to stderr at `info` level, so they don't interleave with the
interactive `>` prompt's output on stdout. For more or less detail,
set the `RUST_LOG` environment variable (`error`, `warn`, `info`,
`debug` or `trace`), e.g. `RUST_LOG=debug cargo run --release`.

### Debugging the ICS connection

`--debug <file>` records the session at the wire level, for chasing
server-side bugs or network trouble:

```
13:11:17.295 -> "d2f3\n"
13:11:19.418 <- "\r<12> --r---k- pr---ppn ... N/d2-f3 (0:00) Nf3 0 1\n"
13:11:28.314 <- "[G]\x00"
13:11:28.314 -> "<timeseal ping reply>\n"
```

- `->` is sent to the server, `<-` received from it; `--` marks
  events (connect, disconnect, timeseal on).
- Timestamps are UTC wall-clock time, to line up with server logs.
- Incoming data is logged exactly as received, before telnet codes are
  stripped or lines are split; control bytes are escaped (`\r`,
  `\n`, `\x00`, ...) so stray ones are visible.
- With `Timeseal` on, outgoing lines are logged as plain text, before
  encoding.
- The password is always masked as `********`, so the file is safe to
  share.
- The file is appended to, never overwritten.

### Interactive prompt

Once running, a `>` prompt reads commands from stdin:

- `quit` or `exit` (case-insensitive) logs off the ICS and shuts the
  program down cleanly — engine process included — instead of it
  needing to be killed.
- A bare line is sent to the ICS verbatim, same as typing it into any
  other ICS client (`tell someone hi`, `abort`, `kibitz nice game`).
- A line prefixed with `engine ` goes straight to the UCI engine's
  stdin instead, e.g. `engine setoption name Hash value 2048` or
  `engine go depth 20`.

Commands are handled even while the engine is thinking: ICS commands
are sent right away, and `quit` stops the search and logs off. Only
`engine ...` has to wait - it's refused with a message until the
current move is played (or, with pondering on, until the game ends).

Handy ICS commands during a game: `resign`, `draw` (offer a draw) and
`abort` (request an abort).

Logs go to stderr so they don't interleave with the prompt.

### GUI

With `"GUI": "Yes"` in config.json, a desktop window opens alongside
the terminal (Windows, Linux and macOS):

- **Board** with the merida pieces, the last move highlighted, and
  your side at the bottom.
- **Player bars** with names and clocks; the side to move's clock
  counts down live and turns red under 10 seconds.
- **Resign** button above the engine panel during a game. It asks
  for confirmation first, then sends FICS's `resign`.
- **Engine** panel: best move, score, depth, time, nodes, NPS and the
  principal variation, in SAN with move numbers (`39... e3 40. Rd3`).
  Book moves are shown as such.
- **Score** chart of the engine's evaluation after each of our moves,
  from the bot's point of view (above the line = the bot is ahead,
  matching the engine panel). The scale grows with the scores (±1,
  ±2, ±5, ±10, ±20, ...); mates sit on the edge. Hover a point for its
  move and exact score.
- **Moves** list for the current game, with the result once it ends.
  The bot's book moves (from `Book` or the engine's own book) are
  highlighted in light blue.
- **Console** showing server output, our commands, kibitz and bot
  events, each line timestamped to the millisecond, with an input
  line that takes the same commands as the `>` prompt (Up/Down
  recalls earlier ones). Bare `fics%` prompt lines are left out.
- **Status bar**: login state (or the server's login error), progress
  while a large opening book loads or while reconnecting, timeseal,
  and the current game number.
- **F** toggles fullscreen (when you're not typing in the console).

The window is the whole interface, so it runs as a standalone app:
nothing is printed to the terminal and the `>` prompt is off (use the
window's console instead), `ColorBoard` is ignored, and on Windows the
console window is released so double-clicking the .exe shows just the
GUI. Set `RUST_LOG` (e.g. `RUST_LOG=info`) to get logs on stderr
anyway when debugging. A broken `config.json` is still reported in the
terminal, since it's read before the window opens. Closing the window
logs off and shuts the engine down; typing `quit` in the console
closes the window.

`config.json`, `Book`, `Engine` and `SavePGN` paths are relative to
the working directory, so when launching it from a desktop shortcut,
set the shortcut's "Start in" folder to the one holding `config.json`.

The window needs a graphical desktop: on Windows 11 under WSL it opens
through WSLg, and on a headless server leave `GUI` at `"No"`. WSL
sometimes loses the Wayland connection (`/run/user/<uid>` goes
missing); the window then falls back to X11 automatically. If the
window can't open at all, the reason is printed in the terminal.

The window follows the system's light/dark setting.

### Config file

`--config` (default `config.json`) points at a JSON file with the ICS
connection details, display and protocol settings, UCI engine options
(applied via `setoption` right after the engine identifies itself and
before the first `isready`), and an optional opening book. Every key
is optional - see `config.example.json`:

```json
{
  "Host": "nightmare-chess.nl",
  "Port": 5000,
  "Username": "YourHandle",
  "Password": "yourpass",
  "Engine": "./yourengine",
  "Kibitz": "Yes",
  "ColorBoard": "No",
  "GUI": "No",
  "Timeseal": "Yes",
  "SavePGN": "games.pgn",
  "Book": "file.bin",
  "engine_options": {
    "Hash": "1024",
    "Threads": "4",
    "SyzygyPath": "/path/to/syzygy",
    "Ponder": "false",
    "OwnBook": "false",
    "NNUE": "true"
  }
}
```

| Key | Values | Default |
|---|---|---|
| `Host` | hostname | `nightmare-chess.nl` |
| `Port` | number | `5000` |
| `Username` | ICS handle | `$FICSHANDLE`, then `$ICSHANDLE`, then `guest` |
| `Password` | ICS password | `$FICSPASSWD`, then `$ICSPASSWD`, then none (guest) |
| `Engine` | engine command line | `engine` |
| `Kibitz` | `"Yes"` / `"No"` | `"Yes"` |
| `ColorBoard` | `"Yes"` / `"No"` | `"No"` (plain ASCII board) |
| `GUI` | `"Yes"` / `"No"` | `"No"` (terminal only) |
| `Timeseal` | `"Yes"` / `"No"` | `"No"` |
| `SavePGN` | path to a `.pgn` file | none (games aren't saved) |
| `engine_options` | `{ "UCI option": "value", ... }` | `Ponder` `false`, `OwnBook` `false`, `NNUE` `true` |
| `Book` | path to a Polyglot `.bin` | none |

Yes/No values are case- and whitespace-insensitive. Keys are
case-sensitive, and unknown keys (such as the removed `DisplayBoard`)
are silently ignored.

- `Host`/`Port` default to `nightmare-chess.nl`/`5000` if omitted.
- `Username`: ICS handle to log in as. Falls back to `$FICSHANDLE`,
  then `$ICSHANDLE`, then `"guest"` if omitted.
- `Password`: ICS password for `Username`. Falls back to
  `$FICSPASSWD`, then `$ICSPASSWD`. Leave it unset everywhere (here
  and both env vars) to log in as a guest.
- `Engine`: command line used to launch the UCI engine, e.g.
  `"./yourengine"` or `"/path/to/engine --some-flag"`. Falls back to
  the literal command `engine` (i.e. an executable named `engine` on
  `$PATH`) if omitted.
- `Kibitz`: `"Yes"` or `"No"` (case/whitespace insensitive), whether to
  whisper search stats after each move - see "Kibitzing search stats"
  below. Defaults to `"Yes"` if omitted.
- `ColorBoard`: `"Yes"` or `"No"` (case/whitespace insensitive). The
  console board is always printed after every move; this only picks
  its style. With `"Yes"`, the previous move's from/to squares are
  highlighted with ANSI colors and the screen is cleared before each
  redraw, so the latest position replaces the previous one instead of
  scrolling. With `"No"` (the default when omitted, or for any value
  other than `"Yes"`), the board is plain ASCII with no escape codes
  at all - safe for any terminal, or when redirecting output to a
  file/log.
  The board is drawn like FICS's `style 1`, from the bot's side
  (Black at the bottom when it plays Black), with black pieces marked
  `*` and the game's details down the right:
  ```
  Game 39 (Erebus vs. SomeHandle)

         ---------------------------------
      8  | *R| *N| *B| *Q| *K| *B| *N| *R|     Move # : 1 (Black)
         |---+---+---+---+---+---+---+---|
      7  | *P| *P| *P| *P| *P| *P| *P| *P|
         |---+---+---+---+---+---+---+---|
      6  |   |   |   |   |   |   |   |   |     White Moves : 'e4'  (0:02)
         |---+---+---+---+---+---+---+---|
      5  |   |   |   |   |   |   |   |   |
         |---+---+---+---+---+---+---+---|
      4  |   |   |   |   | P |   |   |   |     Black Clock : 5:00
         |---+---+---+---+---+---+---+---|
      3  |   |   |   |   |   |   |   |   |     White Clock : 4:58
         |---+---+---+---+---+---+---+---|
      2  | P | P | P | P |   | P | P | P |     Black Strength : 39
         |---+---+---+---+---+---+---+---|
      1  | R | N | B | Q | K | B | N | R |     White Strength : 39
         ---------------------------------
           a   b   c   d   e   f   g   h

  Kibitz: depth=17 score=1.87 time=8.96 node=17234760 nps=1923522 pv=...
  ```
  `Kibitz` is the most recent search-stats line from our own last
  move (see "Kibitzing search stats" below) and is only shown once
  we've made at least one move in the game - it's carried over from
  the previous board redraw and cleared at the start of each new
  game.
- `GUI`: `"Yes"` or `"No"` (case/whitespace insensitive). `"Yes"`
  opens the desktop window - see "GUI" above. Only an explicit
  `"Yes"` turns it on.
- `Timeseal`: `"Yes"` or `"No"` (case/whitespace insensitive).
  When `"Yes"`, everything sent to the server is timeseal v1 encoded,
  so the server charges our clock for thinking time only - not for
  time our moves spend stuck on a slow or lossy network. The server's
  `[G]` keepalive pings are answered automatically. Defaults to
  `"No"` if omitted, since a server without timeseal support can't
  read the encoded lines; nightmare-chess.nl:5000 supports it.
- `SavePGN`: path (absolute, or relative to the working directory) of
  a PGN file. Every game we play is appended to it when it ends, with
  the usual tags (Event, Site, Date, players, result, ratings when the
  server gives them, time control) and the server's reason as a final
  comment, e.g. `{GuestJTMH resigns} 0-1`. The file is created if
  needed and never overwritten. Games that end before any move (e.g.
  aborted) are skipped, and a game still in progress when we quit
  isn't saved. Leave it out, or set it to `""`, to not save games.
- `engine_options`: any option name the engine supports works here,
  not just Hash/Threads/SyzygyPath - sent to the engine as-is via
  `setoption name <k> value <v>`. `Ponder`, `OwnBook`, and `NNUE` are
  UCI "check" (boolean) options like any other, just with built-in
  defaults (`Ponder`/`OwnBook` default to `"false"`, `NNUE` defaults
  to `"true"`) applied when that key isn't listed here at all, so you
  don't have to spell them out unless you want a non-default value.
  With `"Ponder": "true"` the bot also ponders: after each move it
  has the engine think on the opponent's time (`go ponder`) about the
  reply it expects, sends `ponderhit` if the opponent plays it, and
  `stop`s and searches afresh if they don't.
  With `"OwnBook": "true"` the engine plays from its own opening book.
  UCI has no message for "this was a book move", so the bot infers it:
  a `bestmove` with no search behind it, or an `info string` saying
  "book move". Those moves are shown as book moves in the GUI, written
  as `{book}` in the PGN, and not kibitzed.
- `Book`: path (absolute, or relative to the working directory the
  process was started from) to a Polyglot (`.bin`) opening book. When
  set, it's checked for a move before the engine is asked to search
  each turn; once the position falls out of book, every move for the
  rest of that game goes through the engine as usual. Omitting `Book`
  entirely, or leaving it present but blank/whitespace-only (e.g. a
  template config that left the field empty rather than deleting it),
  are both treated the same way: no book.

A missing config file is fine (every field falls back to its
default/guest login); a malformed one is an error.

### Kibitzing search stats

`Kibitz` in config.json (`"Yes"` or `"No"`, case/whitespace
insensitive - `"yes"`, `"YES"`, `" yEs "` all count) turns on
whispering the finished search behind each move, right before the move
itself, via ICS "whisper" (visible only to observers of the game -
there's deliberately no way to broadcast to the whole channel/room):

```
whisper depth=17 score=1.87 time=8.96 node=17234760 nps=1923522 pv=e2e4 e7e5 g1f3 b8c6
```

Field names/units match what the engine itself prints in its `info`
line (score in pawns, time in seconds, mate scores as `M3`/`-M3`), just
using the deepest completed iteration rather than every depth along the
way. Book moves aren't kibitzed, since there's no search to report. On
by default (`Kibitz` defaults to `"Yes"` if omitted from config.json);
set it to `"No"` to turn it off.

### Playing a game

- **Full move history.** UCI engines don't remember earlier moves, so
  each turn the engine gets the game's starting position plus every
  move since (`position fen <start> moves e2e4 e7e5 ...`), not just
  the current board. That's how it sees, and avoids, threefold
  repetition. Takebacks and games joined midway are handled.
- **Illegal move safety net.** If FICS ever rejects one of the bot's
  moves, the bot searches again from the current board instead of
  letting its clock run out. It does this once per move.

### Flaky connections

- **TCP keepalive.** After 30 seconds without traffic, the operating
  system probes the connection every 10 seconds. That keeps routers
  from dropping a quiet connection, and a dead one is noticed in
  about a minute rather than hours. FICS doesn't see the probes.
- **Automatic reconnect.** If the connection drops, the bot doesn't
  exit: it stops the engine, then reconnects and logs back in,
  retrying after 5s, 10s, 30s and then every 60s. The console and
  status bar show each attempt, and `quit` still works while it
  waits.
- **Resuming games.** The server adjourns a game when a player drops.
  After reconnecting, the bot sends `match <opponent>` to the player
  it was playing, which resumes the adjourned game where it left off,
  clocks included. With `SavePGN`, the moves before the drop aren't
  saved; the recording restarts from the resumed position.

## Architecture

| Module | Responsibility | Replaces (original) |
|---|---|---|
| `config.rs` | CLI args + JSON config file (ICS connection, display/timeseal settings, engine options, book) | `argparser.c` |
| `ics.rs` | TCP connection to the ICS: TCP keepalive, optional timeseal v1 encoding and ping replies, telnet IAC stripping, line splitting, `--debug` transcript | `net.c` (`OpenTCP`, `SendToIcs`, `ProcessRawInput`) |
| `board.rs` | Parses `style 12` board lines into a `Style12` struct, converts to FEN and the last move to UCI, and draws the FICS style 1 terminal board | `board.c` (`ParseBoard`, `BoardToFen`) |
| `history.rs` | The game's moves in UCI form since its first board, for `position fen ... moves ...` | `SendMovesToComputer` bookkeeping |
| `engine.rs` | Spawns the engine subprocess, does the UCI handshake (`uci`/`uciok`, `isready`/`readyok`), sends `position`/`go`/`go ponder`/`ponderhit`, parses `bestmove` and spots book moves | `computer.c` (`StartComputer`, `SendMoveToComputer`, `ProcessComputerLine`) — protocol swapped from xboard/CECP to UCI |
| `app.rs` | Ties it together: login and reconnect, main event loop reacting to `<12>` lines and typed commands, pondering | `main.c` (login block) + the `ProcessIcsLine`/`ProcessComputerLine` dispatch |
| `gui.rs` | The optional desktop window (egui/eframe): reads state the bot publishes, sends typed commands back | (new) |
| `san.rs` | UCI -> SAN conversion for the engine panel (via shakmaty) | (new) |
| `pgn.rs` | Records our games' moves and appends finished games to the `SavePGN` file | (new) |
| `main.rs` | Startup: config, logging, runtime; with `GUI` on, the window owns the main thread and the bot runs in the background | `main.c` |


### Aborting a search mid-move

While the engine is thinking, incoming ICS lines are still read and
echoed (`app.rs`'s `search_or_abort`) so the game-over case can be
caught: if the game we're playing ends before the engine answers -
we flagged, the opponent resigned, the game was aborted, etc. - the
search is stopped via `UciEngine::stop` instead of sending a move into
a game that's already over. `stop()` sends UCI's `stop` and drains the
`bestmove` line the spec requires the engine to send in response, so
the next search doesn't mistake it for its own result.


## Acknowledgements

* Armando Hernandez Marroquin for the merida chess pieces (GPLv2+), taken from the Lichess repository - see `assets/pieces/merida/README.md`.

* Joost Buijs for running nightmare-chess.nl ICS server and his monthly (C) tournaments.
* Marcel van Kervinck fork of icsdrone https://github.com/kervinck/icsdrone
* Henrik Gram original author of icsdrone https://sourceforge.net/projects/icsdrone/
* Bob Hyatt for inspiring me to get into computer chess.