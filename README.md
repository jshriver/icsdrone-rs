# icsdrone-rs

A Rust rewrite inspired by icsdrone that bridges an Internet Chess Server to a chess engine — but talks
**UCI** to the engine instead of the original's **xboard/CECP** protocol.

## Usage

```
cargo run --release -- \
  --engine "/path/to/engine"
```

ICS connection details (host, port, username, password) come from
`config.json` now, not the command line — see "Config file" below.
Leave `Password` unset (in config.json, and in `FICSPASSWD`/`ICSPASSWD`)
to log in as a guest.

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
connection details, UCI engine options (applied via `setoption` right
after the engine identifies itself and before the first `isready`),
and an optional opening book:

```json
{
  "Host": "nightmare-chess.nl",
  "Port": 5000,
  "Username": "YourHandle",
  "Password": "yourpass",
  "Engine": "./yourengine",
  "Kibitz": "Yes",
  "engine_options": {
    "Hash": "1024",
    "Threads": "4",
    "SyzygyPath": "/path/to/syzygy"
  },
  "Book": "file.bin"
}
```

- `Host`/`Port` default to `nightmare-chess.nl`/`5000` if omitted.
- `Engine`: command line used to launch the UCI engine, same idea as
  `--engine` on the command line. `--engine`, if given, always takes
  precedence over `Engine` here; if neither is set, it falls back to
  `stockfish`.
- `Kibitz`: `"Yes"` or `"No"` (case/whitespace insensitive), whether to
  whisper search stats after each move - see "Kibitzing search stats"
  below. Defaults to `"Yes"` if omitted.
- `engine_options`: any option name the engine supports works here,
  not just Hash/Threads/SyzygyPath.
- `Book`: path to a Polyglot (`.bin`) opening book. When set, it's
  checked for a move before the engine is asked to search each turn;
  once the game falls out of book, every move for the rest of that
  game goes through the engine as usual.

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
| `config.rs` | CLI args + JSON config file (ICS connection, engine options, book) | `argparser.c` |
| `ics.rs` | Raw TCP connection to the ICS, telnet IAC stripping, line splitting | `net.c` (`OpenTCP`, `SendToIcs`, `ProcessRawInput`) |
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