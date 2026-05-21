# DiscordUserAvaibleChecker

A fast, native Discord **unique-username** (`@handle`) availability
checker, written in Rust. Ships two binaries from a single workspace:

- `dua` — concurrent async CLI with a `hunt` mode dedicated to
  3- and 4-letter handles.
- `dua-gui` — dark-themed [`egui`](https://github.com/emilk/egui) desktop
  GUI with a virtualized results table for tens of thousands of rows.

Both surfaces talk to Discord's public, unauthenticated
`unique-username/username-attempt-unauthed` endpoint — the same call the
Discord web client makes while you are picking a new handle. No login or
token is required.

> This checks the new unique handles only, not legacy `Name#1234`
> discriminator usernames.

## Requirements

- Rust **1.74+** (install via [rustup](https://rustup.rs/)).
- For the GUI on Linux, you need the usual X11/Wayland deps that any
  `egui` app needs (`libxcb`, `libgl1`, `libxkbcommon`, `libwayland-*`).
  Most desktop distros already have them.

## Install

Clone and build a release binary:

```bash
git clone https://github.com/kirodev2277-max/DiscordUserAvaibleChecker.git
cd DiscordUserAvaibleChecker

# CLI only (smallest build, no GUI dependencies)
cargo build --release --bin dua --no-default-features

# CLI + GUI
cargo build --release --bins
```

After a release build the binaries live in `target/release/`:

```bash
./target/release/dua --help
./target/release/dua-gui
```

Or run straight from source while you iterate:

```bash
cargo run --release --bin dua -- check alice bob charlie
cargo run --release --bin dua-gui
```

## CLI

```text
dua <COMMAND>

Commands:
  check     Check one or more handles
  generate  Generate a candidate handle list (3 or 4 letters)
  hunt      Generate + check in a single streaming pipeline
  watch     Poll a single handle on an interval until it frees up
```

### `dua check`

```bash
dua check alice bob charlie
dua check --input names.txt --workers 16 --delay-ms 25
```

Output:

```text
[+] alice: AVAILABLE
[-] bob: taken
[!] charlie: invalid (username: Username must be between 2 and 32 in length.)

summary: 1 available · 1 taken · 1 invalid · 0 rate-limited · 0 error
Appended 3 line(s) to results.txt
```

Legend:

- `[+]` available
- `[-]` taken
- `[!]` invalid (Discord rejected the name; reason in parens)
- `[?]` rate-limited or transport/error

By default results are appended to `results.txt` in the legacy
text-log format so any existing scripts you have keep working:

```text
# Discord username check results
alice - AVAILABLE
bob - TAKEN
charlie - INVALID: username: Username must be between 2 and 32 in length.
```

Use `--output PATH` to redirect, `--no-save` to skip the file, or
`--jsonl PATH` to *also* append a structured JSON Lines log.

### `dua generate`

Generation is **restricted to 3- or 4-letter handles** — the shapes
worth hunting for. Two modes:

```bash
# Every 3-letter combination over a-z (17,576 lines).
dua generate --length 3 --mode all -o all_3.txt

# 5,000 random 4-letter alnum candidates (no duplicates).
dua generate --length 4 --mode random --count 5000 --charset alnum -o names.txt

# Reproducible random output via --seed.
dua generate --length 4 --mode random --count 1000 --seed 42 -o seeded.txt
```

Flags:

- `-n, --length 3|4` — handle length (required; the only supported sizes).
- `--mode all|random` — exhaustive lexicographic enumeration, or
  uniform unique sampling.
- `--charset letters|alnum` — `a-z` (26) or `a-z` + `0-9` (36).
- `-c, --count N` — how many to emit. Defaults to 1,000 for random,
  the full space for `all`.
- `--seed N` — reproducible random output.
- `-o, --output PATH` — destination file (default `usernames.txt`).
- `--append` — append rather than overwrite.

Search-space sizes (so you know what you're getting into):

| Length | letters (a-z) | alnum (a-z + 0-9) |
|-------:|--------------:|------------------:|
| 3      | 17,576        | 46,656            |
| 4      | 456,976       | 1,679,616         |

### `dua hunt`

The headline command. One pass that:

1. Generates 3- or 4-letter candidates per your flags.
2. Streams them through the checker with N async workers.
3. Prints every `AVAILABLE` hit as it lands (other statuses are
   counted silently).
4. Appends a full text log to `results.txt`.
5. Optionally streams just the hits to a separate file.

```bash
# 500 random 4-letter handles, 16 workers, 25ms stagger, save hits.
dua hunt --length 4 --mode random --count 500 --workers 16 \
         --delay-ms 25 --hits-file hits.txt

# Exhaustive 3-letter sweep, stop after the first 25 hits.
dua hunt --length 3 --mode all --workers 24 --stop-after 25
```

Flags worth knowing:

- `--workers N` — concurrent in-flight requests (default 16).
- `--delay-ms N` — stagger between task starts (default 25). Bump it
  up if Discord starts rate-limiting you.
- `--stop-after N` — cancel the rest of the run after this many
  AVAILABLE hits (useful for exhaustive sweeps).
- `--hits-file PATH` — append only AVAILABLE handles, one per line.

### `dua watch`

Poll a single handle until it becomes available, then ring the
terminal bell and exit:

```bash
dua watch coolname --interval-secs 30 --max-attempts 0
```

## GUI

```bash
cargo run --release --bin dua-gui
```

Two tabs:

- **Check** — paste or load a list of handles, set workers/delay,
  click ▶ Check. Results stream into a colour-coded virtualized
  table; toggle the count chips to filter.
- **Hunt 3/4-letter** — pick length (3 or 4), mode (random / all),
  charset (letters / alnum), and optional count + seed + stop-after.
  Click 🎯 Hunt and watch AVAILABLE hits land live.

Auto-save appends to `results.txt` in the same legacy text format the
CLI uses. The Save now button performs an explicit append at any time.

To preseed input on launch from a file:

```bash
DUA_INPUT_FILE=names.txt dua-gui
```

## Configuration knobs that matter

- **Workers** — concurrent in-flight HTTP requests. 8–16 is a sane
  default; 24–32 is fine on a fast connection but will earn you 429s
  faster.
- **Delay** — milliseconds between task starts. Combined with worker
  count it caps your effective request rate.
- **Retry-After** — when Discord returns `429`, the CLI honours
  `Retry-After ≤ 10s` and auto-retries once; longer waits surface as
  `rate-limited` for you to handle.

## How it works

```text
POST https://discord.com/api/v9/unique-username/username-attempt-unauthed
Content-Type: application/json
{"username": "<name>"}
```

A `200 OK` with `{"taken": true|false}` decides
available vs. taken. Validation errors return `400`/`422` with a JSON
body that we surface as `INVALID`. `429` is treated as rate-limit
back-pressure.

## Project layout

```text
.
├── Cargo.toml
├── src/
│   ├── lib.rs          # public surface
│   ├── checker.rs      # async HTTP client + batch runner
│   ├── generator.rs    # 3/4-letter exhaustive + random generators
│   ├── persistence.rs  # text + JSONL logs, input parsing
│   ├── result.rs       # CheckResult / Status
│   └── bin/
│       ├── dua.rs      # CLI binary
│       └── dua_gui.rs  # GUI binary
└── README.md
```

## Disclaimer

Not affiliated with Discord. This uses a public endpoint intended for
the Discord client. Be polite: keep request volume modest and don't
use it to bulk-scrape or mass-register handles.
