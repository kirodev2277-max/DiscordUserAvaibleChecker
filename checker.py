#!/usr/bin/env python3
"""Discord username availability checker.

Checks whether a Discord unique username (the new "@handle" style name,
e.g. "coolname") is currently available or already taken, by calling
Discord's public unauthenticated username-attempt endpoint.
"""

from __future__ import annotations

import argparse
import sys
import time
from typing import Optional

import requests


API_URL = "https://discord.com/api/v9/unique-username/username-attempt-unauthed"
USER_AGENT = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
    "AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/124.0.0.0 Safari/537.36"
)
REQUEST_TIMEOUT = 10
DELAY_BETWEEN_REQUESTS = 1.0


class CheckResult:
    """Result of a single username check."""

    def __init__(
        self,
        username: str,
        status: str,
        reason: Optional[str] = None,
    ) -> None:
        self.username = username
        # status is one of: "available", "taken", "invalid", "error"
        self.status = status
        self.reason = reason

    def format_line(self) -> str:
        if self.status == "available":
            return f"[+] {self.username}: AVAILABLE"
        if self.status == "taken":
            return f"[-] {self.username}: taken"
        if self.status == "invalid":
            extra = f" ({self.reason})" if self.reason else ""
            return f"[!] {self.username}: invalid{extra}"
        # error
        extra = f" ({self.reason})" if self.reason else ""
        return f"[?] {self.username}: error{extra}"


def check_username(username: str, session: requests.Session) -> CheckResult:
    """Check a single username against Discord's API.

    Returns a CheckResult and never raises for normal failures.
    """
    username = username.strip()
    if not username:
        return CheckResult(username, "invalid", "empty username")

    try:
        response = session.post(
            API_URL,
            json={"username": username},
            headers={
                "User-Agent": USER_AGENT,
                "Accept": "application/json",
                "Content-Type": "application/json",
            },
            timeout=REQUEST_TIMEOUT,
        )
    except requests.RequestException as exc:
        return CheckResult(username, "error", f"network error: {exc}")

    # Try to parse JSON regardless of status code, since Discord returns
    # validation errors as JSON with non-2xx codes too.
    try:
        data = response.json()
    except ValueError:
        return CheckResult(
            username,
            "error",
            f"HTTP {response.status_code}, non-JSON response",
        )

    if response.status_code == 200 and isinstance(data, dict) and "taken" in data:
        return CheckResult(
            username,
            "taken" if data["taken"] else "available",
        )

    # Validation / rate-limit / other error cases.
    reason = _extract_error_reason(data) or f"HTTP {response.status_code}"
    # Treat clear validation errors on the username field as "invalid",
    # everything else as a generic error.
    if response.status_code in (400, 422) and isinstance(data, dict):
        return CheckResult(username, "invalid", reason)
    return CheckResult(username, "error", reason)


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


def check_many(usernames, delay: float = DELAY_BETWEEN_REQUESTS) -> int:
    """Check several usernames, printing each result. Returns an exit code."""
    session = requests.Session()
    exit_code = 0
    for index, name in enumerate(usernames):
        if index > 0 and delay > 0:
            time.sleep(delay)
        result = check_username(name, session)
        print(result.format_line())
        if result.status in ("error",):
            exit_code = 2
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
        help="Usernames to check. If omitted, runs in interactive mode.",
    )
    parser.add_argument(
        "--delay",
        type=float,
        default=DELAY_BETWEEN_REQUESTS,
        help=(
            "Seconds to wait between requests when checking multiple names "
            "(default: %(default)s)."
        ),
    )
    return parser.parse_args(argv)


def main(argv=None) -> int:
    args = parse_args(argv)
    if args.usernames:
        return check_many(args.usernames, delay=args.delay)
    return interactive_loop()


if __name__ == "__main__":
    sys.exit(main())
