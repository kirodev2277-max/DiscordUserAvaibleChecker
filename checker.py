#!/usr/bin/env python3
"""Discord username availability checker.

Checks whether a Discord unique username (the new "@handle" style name,
e.g. "coolname") is currently available or already taken, by calling
Discord's public unauthenticated username-attempt endpoint.
"""

from __future__ import annotations

import argparse
import os
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from typing import Callable, Iterable, List, Optional, Sequence

import requests


API_URL = "https://discord.com/api/v9/unique-username/username-attempt-unauthed"
USER_AGENT = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
    "AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/124.0.0.0 Safari/537.36"
)
REQUEST_TIMEOUT = 10
# Default inter-request delay. Kept at 0 so batch checks are fast by default;
# raise it with --delay if Discord starts rate-limiting you.
DEFAULT_DELAY = 0.0
DEFAULT_WORKERS = 8
# Cap how long we'll honor a Retry-After before giving up on the auto-retry.
MAX_RETRY_AFTER_SECONDS = 10.0
# Default file to append results to after a batch check.
DEFAULT_OUTPUT_FILE = "results.txt"
RESULTS_HEADER = "# Discord username check results"

# Status constants. Used both as internal values and (uppercased) in the
# saved results file.
STATUS_AVAILABLE = "available"
STATUS_TAKEN = "taken"
STATUS_INVALID = "invalid"
STATUS_RATE_LIMITED = "rate_limited"
STATUS_ERROR = "error"


class CheckResult:
    """Result of a single username check."""

    def __init__(
        self,
        username: str,
        status: str,
        reason: Optional[str] = None,
    ) -> None:
        self.username = username
        # status is one of: "available", "taken", "invalid", "rate_limited", "error"
        self.status = status
        self.reason = reason

    def format_line(self) -> str:
        if self.status == STATUS_AVAILABLE:
            return f"[+] {self.username}: AVAILABLE"
        if self.status == STATUS_TAKEN:
            return f"[-] {self.username}: taken"
        if self.status == STATUS_INVALID:
            extra = f" ({self.reason})" if self.reason else ""
            return f"[!] {self.username}: invalid{extra}"
        if self.status == STATUS_RATE_LIMITED:
            extra = f" ({self.reason})" if self.reason else ""
            return f"[?] {self.username}: rate limited{extra}"
        # error
        extra = f" ({self.reason})" if self.reason else ""
        return f"[?] {self.username}: error{extra}"

    def format_file_line(self) -> str:
        """Render this result as a line for the results file.

        Format: ``<username> - <STATUS>`` with optional ``: <reason>`` for
        statuses that carry extra detail (invalid / error / rate_limited).
        """
        # rate_limited reads better as "RATE_LIMITED" in the file.
        status_token = self.status.upper()
        if self.status in (STATUS_INVALID, STATUS_ERROR, STATUS_RATE_LIMITED) and self.reason:
            return f"{self.username} - {status_token}: {self.reason}"
        return f"{self.username} - {status_token}"


def _post_attempt(
    username: str, session: requests.Session
) -> requests.Response:
    return session.post(
        API_URL,
        json={"username": username},
        headers={
            "User-Agent": USER_AGENT,
            "Accept": "application/json",
            "Content-Type": "application/json",
        },
        timeout=REQUEST_TIMEOUT,
    )


def _retry_after_seconds(response: requests.Response) -> Optional[float]:
    """Extract a Retry-After value (seconds) from a 429 response, if any."""
    header = response.headers.get("Retry-After")
    if header:
        try:
            return float(header)
        except ValueError:
            pass
    try:
        body = response.json()
    except ValueError:
        return None
    if isinstance(body, dict):
        value = body.get("retry_after")
        if isinstance(value, (int, float)):
            return float(value)
    return None


def check_username(username: str, session: requests.Session) -> CheckResult:
    """Check a single username against Discord's API.

    Returns a CheckResult and never raises for normal failures. On HTTP 429
    the call will sleep up to MAX_RETRY_AFTER_SECONDS and retry once.
    """
    username = username.strip()
    if not username:
        return CheckResult(username, STATUS_INVALID, "empty username")

    try:
        response = _post_attempt(username, session)
    except requests.RequestException as exc:
        return CheckResult(username, STATUS_ERROR, f"network error: {exc}")

    if response.status_code == 429:
        wait = _retry_after_seconds(response)
        if wait is not None and wait <= MAX_RETRY_AFTER_SECONDS:
            time.sleep(max(wait, 0.0))
            try:
                response = _post_attempt(username, session)
            except requests.RequestException as exc:
                return CheckResult(username, STATUS_ERROR, f"network error: {exc}")
            if response.status_code == 429:
                wait2 = _retry_after_seconds(response)
                detail = f"retry after {wait2:.2f}s" if wait2 is not None else None
                return CheckResult(username, STATUS_RATE_LIMITED, detail)
        else:
            detail = (
                f"retry after {wait:.2f}s" if wait is not None else None
            )
            return CheckResult(username, STATUS_RATE_LIMITED, detail)

    # Try to parse JSON regardless of status code, since Discord returns
    # validation errors as JSON with non-2xx codes too.
    try:
        data = response.json()
    except ValueError:
        return CheckResult(
            username,
            STATUS_ERROR,
            f"HTTP {response.status_code}, non-JSON response",
        )

    if response.status_code == 200 and isinstance(data, dict) and "taken" in data:
        return CheckResult(
            username,
            STATUS_TAKEN if data["taken"] else STATUS_AVAILABLE,
        )

    # Validation / rate-limit / other error cases.
    reason = _extract_error_reason(data) or f"HTTP {response.status_code}"
    # Treat clear validation errors on the username field as "invalid",
    # everything else as a generic error.
    if response.status_code in (400, 422) and isinstance(data, dict):
        return CheckResult(username, STATUS_INVALID, reason)
    return CheckResult(username, STATUS_ERROR, reason)


def _extract_error_reason(data: object) -> Optional[str]:
    """Pull a human-readable reason out of a Discord error JSON payload."""
    if not isinstance(data, dict):
        return None

    # Common top-level message field.
    message = data.get("message")

    # Field-specific errors live under "errors": {"username": {"_errors": [...]}}.
    field_messages = []
    errors = data.get("errors")
    if isinstance(errors, dict):
        for field, value in errors.items():
            if isinstance(value, dict) and "_errors" in value:
                for entry in value["_errors"]:
                    if isinstance(entry, dict) and entry.get("message"):
                        field_messages.append(f"{field}: {entry['message']}")

    parts = []
    if field_messages:
        parts.extend(field_messages)
    elif message:
        parts.append(str(message))

    return "; ".join(parts) if parts else None


def save_results(results: Iterable[CheckResult], path: str) -> int:
    """Append a batch of results to ``path`` in the standard text format.

    If the file does not exist yet, it is created and a header comment line
    (``RESULTS_HEADER``) is written first. Subsequent runs against the same
    file just append more lines without repeating the header.

    Returns the number of result lines written.
    """
    results_list = list(results)
    if not results_list:
        return 0

    file_exists = os.path.exists(path)
    with open(path, "a", encoding="utf-8") as fh:
        if not file_exists:
            fh.write(RESULTS_HEADER + "\n")
        for result in results_list:
            fh.write(result.format_file_line() + "\n")
    return len(results_list)


def parse_usernames_text(text: str) -> List[str]:
    """Parse a blob of user-supplied text into a list of usernames.

    Accepts newline-separated, comma-separated, or whitespace-separated
    input and any mix of those. Empty fragments are dropped. Order is
    preserved.
    """
    if not text:
        return []
    # Replace commas with newlines, then split on any whitespace.
    pieces = text.replace(",", "\n").split()
    return [p.strip() for p in pieces if p.strip()]


def load_usernames_from_file(path: str) -> List[str]:
    """Read usernames from a text file (one per line or comma-separated)."""
    with open(path, "r", encoding="utf-8") as fh:
        return parse_usernames_text(fh.read())


def check_many(
    usernames: Iterable[str],
    delay: float = DEFAULT_DELAY,
    workers: int = DEFAULT_WORKERS,
    session: Optional[requests.Session] = None,
    on_result: Optional[Callable[[int, CheckResult], None]] = None,
) -> List[CheckResult]:
    """Check several usernames concurrently and return results in input order.

    A single shared ``requests.Session`` is reused across worker threads.
    If ``delay`` is greater than zero, each task sleeps for that many seconds
    before its request, which throttles the overall request rate when
    combined with a small worker count.

    ``on_result`` is invoked from worker threads as each result completes,
    receiving ``(index, CheckResult)``. The GUI uses this to stream progress.
    Results in the returned list are always ordered to match the input.
    """
    names: List[str] = [str(n) for n in usernames]
    if not names:
        return []

    owns_session = session is None
    if session is None:
        session = requests.Session()
    effective_workers = max(1, min(workers, len(names)))

    results: List[Optional[CheckResult]] = [None] * len(names)

    def _task(index_name):
        index, name = index_name
        if delay > 0 and index > 0:
            time.sleep(delay)
        result = check_username(name, session)
        results[index] = result
        if on_result is not None:
            try:
                on_result(index, result)
            except Exception:
                # Never let a callback failure break the batch.
                pass
        return result

    try:
        if effective_workers == 1:
            for pair in enumerate(names):
                _task(pair)
        else:
            with ThreadPoolExecutor(max_workers=effective_workers) as pool:
                # Drain to make sure any exceptions surface.
                for _ in pool.map(_task, list(enumerate(names))):
                    pass
    finally:
        if owns_session:
            session.close()

    # mypy/runtime: by this point every slot is filled.
    return [r for r in results if r is not None]


def run_batch(
    usernames: Sequence[str],
    delay: float = DEFAULT_DELAY,
    workers: int = DEFAULT_WORKERS,
    output_file: Optional[str] = DEFAULT_OUTPUT_FILE,
) -> int:
    """High-level CLI batch: check, print, and optionally save.

    Returns a process exit code (2 if any check errored).
    """
    if not usernames:
        return 0

    results = check_many(usernames, delay=delay, workers=workers)

    exit_code = 0
    for result in results:
        print(result.format_line())
        if result.status == STATUS_ERROR:
            exit_code = 2

    if output_file:
        try:
            written = save_results(results, output_file)
        except OSError as exc:
            print(
                f"warning: could not write to {output_file}: {exc}",
                file=sys.stderr,
            )
        else:
            if written:
                print(f"\nSaved {written} result(s) to {output_file}")

    return exit_code


def interactive_loop() -> int:
    """Prompt the user for usernames until they quit."""
    print("Discord username availability checker")
    print("Type a username to check, or 'quit' / 'exit' to stop.")
    session = requests.Session()
    try:
        while True:
            try:
                raw = input("> ")
            except EOFError:
                print()
                return 0
            name = raw.strip()
            if not name:
                continue
            if name.lower() in ("quit", "exit"):
                return 0
            result = check_username(name, session)
            print(result.format_line())
    except KeyboardInterrupt:
        print()
        return 0


def parse_args(argv=None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Check whether one or more Discord unique usernames are "
            "available or already taken."
        ),
    )
    parser.add_argument(
        "usernames",
        nargs="*",
        help="Usernames to check. If omitted (and --input is not used), runs in interactive mode.",
    )
    parser.add_argument(
        "--input",
        "-i",
        metavar="PATH",
        help=(
            "Read usernames from a text file (one per line or "
            "comma-separated). Combined with any usernames given on the "
            "command line."
        ),
    )
    parser.add_argument(
        "--delay",
        type=float,
        default=DEFAULT_DELAY,
        help=(
            "Seconds to wait before each request when checking multiple "
            "names. Use this if Discord starts rate-limiting you "
            "(default: %(default)s)."
        ),
    )
    parser.add_argument(
        "--workers",
        type=int,
        default=DEFAULT_WORKERS,
        help=(
            "Number of concurrent worker threads for batch checks "
            "(default: %(default)s). Use 1 to force sequential checks."
        ),
    )
    parser.add_argument(
        "--output",
        "-o",
        default=DEFAULT_OUTPUT_FILE,
        help=(
            "After a batch check, append all results to this file, "
            "creating it (with a header) if needed "
            "(default: %(default)s)."
        ),
    )
    parser.add_argument(
        "--no-save",
        dest="output",
        action="store_const",
        const=None,
        help="Don't write results to a file.",
    )
    return parser.parse_args(argv)


def main(argv=None) -> int:
    args = parse_args(argv)
    if args.workers < 1:
        print("error: --workers must be >= 1", file=sys.stderr)
        return 2

    usernames: List[str] = list(args.usernames)
    if args.input:
        try:
            usernames.extend(load_usernames_from_file(args.input))
        except OSError as exc:
            print(f"error: could not read --input file: {exc}", file=sys.stderr)
            return 2

    if usernames:
        return run_batch(
            usernames,
            delay=args.delay,
            workers=args.workers,
            output_file=args.output,
        )
    return interactive_loop()


if __name__ == "__main__":
    sys.exit(main())
