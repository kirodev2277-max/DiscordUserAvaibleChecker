# DiscordUserAvaibleChecker

A small Python tool that checks whether a Discord **unique username** (the
new `@handle` style name, e.g. `coolname`) is available or already taken.

It works by calling Discord's public, unauthenticated username-attempt
endpoint, the same one the Discord client itself uses while you are picking
a new handle. No login or token is required.

Comes in two flavors:

- A fast concurrent **CLI** (`checker.py`)
- A simple **GUI** (`gui.py`) built with `tkinter`

> Note: this checks the new unique handles only, not legacy `Name#1234`
> discriminator usernames.

## Requirements

- Python 3.9+
- The [`requests`](https://pypi.org/project/requests/) library
- `tkinter` (ships with Python on most platforms; only needed for the GUI)

## Install

Clone the repo and install dependencies (a virtual environment is
recommended):

```bash
git clone https://github.com/kirodev2277-max/DiscordUserAvaibleChecker.git
cd DiscordUserAvaibleChecker

python3 -m venv .venv
source .venv/bin/activate         # on Windows: .venv\Scripts\activate

pip install -r requirements.txt
```

## CLI usage

### Check usernames

```bash
python checker.py alice bob charlie
```

Example output:

```
[-] alice: taken
[+] bob: AVAILABLE
[!] charlie: invalid (username: Username must be between 2 and 32 in length.)

Saved 3 result(s) to results.txt
```

Legend:

- `[+]` available
- `[-]` taken
- `[!]` invalid (Discord rejected the name, reason shown in parentheses)
- `[?]` error talking to Discord (network problem, rate limit, etc.)

### Check from a file

You can also feed a text file of usernames (one per line, or comma-separated,
or a mix). It can be combined with names on the command line:

```bash
python checker.py --input usernames.txt
python checker.py --input usernames.txt extra_name another_name
```

### Interactive mode

Run with no arguments and no `--input` to enter an interactive prompt:

```bash
python checker.py
```

```
Discord username availability checker
Type a username to check, or 'quit' / 'exit' to stop.
> bob
[+] bob: AVAILABLE
> alice
[-] alice: taken
> quit
```

You can also stop with `Ctrl-C`.

### Saved results file

After a batch check, results are appended to `results.txt` in the current
directory. The first time the file is created it gets a header line; later
runs simply append more lines, so the file accumulates everything you've
ever checked. Each line looks like:

```
# Discord username check results
alice - TAKEN
bob - AVAILABLE
charlie - INVALID: username: Username must be between 2 and 32 in length.
```

Use `--output PATH` to write somewhere else, or `--no-save` to skip the file.

### Options

```
python checker.py --help
```

- `--input PATH`, `-i PATH` - read usernames from a text file (one per
  line or comma-separated). Combined with any usernames given on the
  command line.
- `--workers N` - number of concurrent worker threads for batch checks
  (default: `8`). Use `1` to force sequential checks.
- `--delay SECONDS` - seconds to wait before each request when checking
  multiple names (default: `0`). Increase this if Discord starts
  rate-limiting you.
- `--output PATH`, `-o PATH` - file to append results to
  (default: `results.txt`).
- `--no-save` - don't write a results file.

The CLI handles HTTP 429 rate-limit responses by reading `Retry-After`
(from the header or JSON body), sleeping, and retrying once.

## GUI

Launch the desktop GUI with:

```bash
python gui.py
```

The GUI lets you:

- Paste or type usernames into a text box (one per line, or comma-separated,
  or both), or load them from a `.txt` file with **Load from file...**.
- Click **Check** to run a concurrent batch check in the background. The UI
  stays responsive: a progress bar and `Checking N/M...` label update as
  results stream in.
- See each result in a color-coded table (green = available, red = taken,
  orange = invalid, gray = error).
- Auto-save results to `results.txt` (or any path you pick) using the same
  appending text format as the CLI. A **Save results** button is also
  available for manual saves, and **Clear** wipes the input and results.

## Generating candidate usernames

If you want to feed a big batch of candidate handles into the checker
(for example, every possible 4-letter name), use the bundled
`generate.py` script. It writes one username per line to a `.txt` file
that the checker can then read with `--input`.

```bash
# Generate 1000 random 4-letter names
python generate.py --length 4 --count 1000 -o names.txt

# Generate every 4-letter combination (a-z) - 456,976 lines
python generate.py --mode all --length 4 -o all_4letter.txt

# Then feed it into the checker
python checker.py --input names.txt --workers 16
```

You can also load any of these `.txt` files into the GUI with the
**Load from file...** button.

Useful flags:

- `--mode {all,random}` - emit every combination, or sample random
  unique strings (default: `random`).
- `--length N` / `-n N` - username length (default: `4`).
- `--count N` / `-c N` - how many to write (random mode default: 1000;
  in `all` mode, omit to dump every combination).
- `--charset {letters,alnum,full}` - `letters` is `a-z`, `alnum` adds
  `0-9`, `full` also adds `.` and `_` while enforcing Discord's rule
  that names can't start or end with `.` or contain `..`.
- `--seed N` - RNG seed for reproducible random output.
- `--append` - append to the output file instead of overwriting it.

## How it works

The script POSTs to:

```
POST https://discord.com/api/v9/unique-username/username-attempt-unauthed
Content-Type: application/json
{"username": "<name>"}
```

A `200 OK` response with `{"taken": true|false}` tells us whether the handle
is already in use. Validation errors (too short, invalid characters, etc.)
come back as `400` / `422` with a JSON body, and the script surfaces the
error message from Discord.

For batch checks the CLI and GUI share the same `check_many` function from
`checker.py`, which uses a `ThreadPoolExecutor` over a single shared
`requests.Session` and returns results in input order.

## Disclaimer

This project is not affiliated with Discord. It uses a public endpoint
intended for the Discord client. Be polite: keep request volume low and do
not use this to bulk-scrape or mass-register usernames.
