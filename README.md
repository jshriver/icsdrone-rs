# icsdrone-rs

A Rust rewrite inspired by icsdrone that bridges an Internet Chess Server to a chess engine — but talks
**UCI** to the engine instead of the original's **xboard/CECP** protocol.

## Usage

```
cargo run --release
```

Everything - ICS connection, engine, display, timeseal, opening book -
is configured in `config.json`; see "Config file" below. Copy
`config.example.json` to `config.json` to get started. Leave
`Password` unset (in config.json, and in `FICSPASSWD`/`ICSPASSWD`) to
log in as a guest.

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

Logs go to stderr so they don't interleave with the prompt.

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
  "Timeseal": "Yes",
  "engine_options": {
    "Hash": "1024",
    "Threads": "4",
    "SyzygyPath": "/path/to/syzygy",
    "Ponder": "false",
    "OwnBook": "false",
    "NNUE": "true"
  },
  "Book": "file.bin"
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
| `Timeseal` | `"Yes"` / `"No"` | `"No"` |
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
  Just above the board, a short header is printed:
  ```
  White: Erebus  Black: SomeHandle
  Move: P/e2-e4
  Clock: White 4:52  Black 4:58
  Kibitz: depth=17 score=1.87 time=8.96 node=17234760 nps=1923522 pv=...
  ```
  The `White`/`Black` names, `Move` and `Clock` (each side's
  remaining time as of that move) come from the current game's
  style12 line;
  `Kibitz` is the most recent search-stats line from our own last
  move (see "Kibitzing search stats" below) and is only shown once
  we've made at least one move in the game - it's carried over from
  the previous board redraw and cleared at the start of each new
  game.
- `Timeseal`: `"Yes"` or `"No"` (case/whitespace insensitive).
  When `"Yes"`, everything sent to the server is timeseal v1 encoded,
  so the server charges our clock for thinking time only - not for
  time our moves spend stuck on a slow or lossy network. The server's
  `[G]` keepalive pings are answered automatically. Defaults to
  `"No"` if omitted, since a server without timeseal support can't
  read the encoded lines; nightmare-chess.nl:5000 supports it.
- `engine_options`: any option name the engine supports works here,
  not just Hash/Threads/SyzygyPath - sent to the engine as-is via
  `setoption name <k> value <v>`. `Ponder`, `OwnBook`, and `NNUE` are
  UCI "check" (boolean) options like any other, just with built-in
  defaults (`Ponder`/`OwnBook` default to `"false"`, `NNUE` defaults
  to `"true"`) applied when that key isn't listed here at all, so you
  don't have to spell them out unless you want a non-default value.
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
way. On by default (`Kibitz` defaults to `"Yes"` if omitted from
config.json); set it to `"No"` to turn it off.

## Architecture

| Module | Responsibility | Replaces (original) |
|---|---|---|
| `config.rs` | CLI args + JSON config file (ICS connection, display/timeseal settings, engine options, book) | `argparser.c` |
| `ics.rs` | TCP connection to the ICS: optional timeseal v1 encoding and ping replies, telnet IAC stripping, line splitting, `--debug` transcript | `net.c` (`OpenTCP`, `SendToIcs`, `ProcessRawInput`) |
| `board.rs` | Parses `style 12` board lines into a `Style12` struct and converts to FEN | `board.c` (`ParseBoard`, `BoardToFen`) |
| `engine.rs` | Spawns the engine subprocess, does the UCI handshake (`uci`/`uciok`, `isready`/`readyok`), sends `position`/`go`, parses `bestmove` | `computer.c` (`StartComputer`, `SendMoveToComputer`, `ProcessComputerLine`) — protocol swapped from xboard/CECP to UCI |
| `app.rs` | Ties it together: login sequence, main event loop reacting to `<12>` lines | `main.c` (login block) + the `ProcessIcsLine`/`ProcessComputerLine` dispatch |


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

* Joost Buijs for running nightmare-chess.nl ICS server and his monthly (C) tournaments.
* Marcel van Kervinck fork of icsdrone https://github.com/kervinck/icsdrone
* Henrik Gram original author of icsdrone https://sourceforge.net/projects/icsdrone/
* Bob Hyatt for inspiring me to get into computer chess.