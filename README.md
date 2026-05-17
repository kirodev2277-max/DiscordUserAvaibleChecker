# DiscordUserAvaibleChecker

A tiny Python CLI that checks whether a Discord **unique username** (the new
`@handle` style name, e.g. `coolname`) is available or already taken.

It works by calling Discord's public, unauthenticated username-attempt
endpoint, the same one the Discord client itself uses while you are picking a
new handle. No login or token is required.

> Note: this checks the new unique handles only, not legacy `Name#1234`
> discriminator usernames.

## Requirements

- Python 3.9+
- The [`requests`](https://pypi.org/project/requests/) library

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

## Usage

### Check one or more usernames from the command line

```bash
python checker.py alice bob charlie
```

Example output:

```
[-] alice: taken
[+] bob: AVAILABLE
[!] charlie: invalid (username: Username must be between 2 and 32 in length.)
```

Legend:

- `[+]` available
- `[-]` taken
- `[!]` invalid (Discord rejected the name, reason shown in parentheses)
- `[?]` error talking to Discord (network problem, rate limit, etc.)

### Interactive mode

Run with no arguments to enter an interactive prompt:

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

### Options

```
python checker.py --help
```

- `--delay SECONDS` — seconds to wait between requests when checking
  multiple names (default: `1.0`). Keep some delay to avoid getting
  rate-limited by Discord.

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

## Disclaimer

This project is not affiliated with Discord. It uses a public endpoint
intended for the Discord client. Be polite: keep request volume low and do
not use this to bulk-scrape or mass-register usernames.
