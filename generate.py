#!/usr/bin/env python3
"""Generate candidate Discord unique-username strings into a text file.

The output is a plain UTF-8 text file with one username per line, ready to
be fed into ``checker.py --input <path>``.

Two modes:

- ``all``: emit every combination of the chosen charset at the chosen
  length, in lexicographic order. For length 4 with letters-only that's
  26**4 = 456,976 lines.
- ``random``: sample uniformly at random from the charset and dedupe until
  ``--count`` unique strings have been written.

Three charsets:

- ``letters``: ``a-z`` (26)
- ``alnum``:   ``a-z`` + ``0-9`` (36)
- ``full``:    ``a-z`` + ``0-9`` + ``.`` + ``_`` (38), but with Discord's
  punctuation rules enforced: cannot start or end with ``.`` and cannot
  contain consecutive ``..``.

Stdlib only: argparse, itertools, random, string.
"""

from __future__ import annotations

import argparse
import itertools
import random
import string
import sys
from typing import Iterable, Iterator, Optional, TextIO


CHARSETS = {
    "letters": string.ascii_lowercase,
    "alnum": string.ascii_lowercase + string.digits,
    "full": string.ascii_lowercase + string.digits + "._",
}

DEFAULT_LENGTH = 4
DEFAULT_RANDOM_COUNT = 1000
DEFAULT_CHARSET = "letters"
DEFAULT_OUTPUT = "usernames.txt"


def is_valid_username(name: str) -> bool:
    """Return True if ``name`` satisfies Discord's punctuation rules.

    Only the ``.``-related rules matter here; character-set membership is
    already guaranteed by the generator. We keep this lenient for the
    letter and alnum charsets (which can never trip these rules) and only
    really filter for the ``full`` charset.
    """
    if not name:
        return False
    if name.startswith(".") or name.endswith("."):
        return False
    if ".." in name:
        return False
    return True


def iter_all(charset: str, length: int) -> Iterator[str]:
    """Lexicographic iterator over every length-``length`` string."""
    for tup in itertools.product(charset, repeat=length):
        yield "".join(tup)


def iter_random(
    charset: str,
    length: int,
    count: int,
    rng: random.Random,
) -> Iterator[str]:
    """Yield ``count`` unique random strings of length ``length``.

    Uniqueness is enforced with an in-memory set, so this is best for
    counts that are comfortably small relative to ``len(charset) ** length``.
    """
    seen: set[str] = set()
    chars = charset
    # Guard against asking for more uniques than the space contains.
    space_size = len(chars) ** length
    target = min(count, space_size)
    while len(seen) < target:
        candidate = "".join(rng.choices(chars, k=length))
        if candidate in seen:
            continue
        seen.add(candidate)
        yield candidate


def generate(
    length: int,
    count: Optional[int],
    mode: str,
    charset_name: str,
    rng: Optional[random.Random] = None,
) -> Iterator[str]:
    """Yield candidate usernames according to the given parameters.

    Applies Discord's punctuation rules (only relevant for the ``full``
    charset). For ``mode == "all"`` and a non-None ``count``, the iterator
    stops after ``count`` valid strings have been yielded.
    """
    if length < 1:
        raise ValueError("length must be >= 1")
    if mode not in ("all", "random"):
        raise ValueError(f"unknown mode: {mode!r}")
    if charset_name not in CHARSETS:
        raise ValueError(f"unknown charset: {charset_name!r}")

    charset = CHARSETS[charset_name]
    needs_filter = charset_name == "full"

    if mode == "all":
        source: Iterable[str] = iter_all(charset, length)
        emitted = 0
        for name in source:
            if needs_filter and not is_valid_username(name):
                continue
            yield name
            emitted += 1
            if count is not None and emitted >= count:
                return
        return

    # random mode
    if count is None:
        count = DEFAULT_RANDOM_COUNT
    rng = rng if rng is not None else random.Random()

    if not needs_filter:
        yield from iter_random(charset, length, count, rng)
        return

    # For the "full" charset we sample, then drop invalid names. We keep
    # going until we've produced ``count`` valid uniques, but to avoid an
    # infinite loop in pathological inputs (e.g. length=1 with a charset
    # that has no valid 1-char names) we cap retries.
    seen: set[str] = set()
    space_size = len(charset) ** length
    target = min(count, space_size)
    # Allow generous slack to account for filtered-out candidates.
    max_attempts = max(target * 20, 1000)
    attempts = 0
    while len(seen) < target and attempts < max_attempts:
        attempts += 1
        candidate = "".join(rng.choices(charset, k=length))
        if candidate in seen:
            continue
        if not is_valid_username(candidate):
            continue
        seen.add(candidate)
        yield candidate


def write_stream(
    fh: TextIO,
    items: Iterable[str],
) -> int:
    """Write ``items`` to ``fh`` one per line. Returns the line count."""
    written = 0
    for item in items:
        fh.write(item)
        fh.write("\n")
        written += 1
    return written


def parse_args(argv: Optional[list] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Generate candidate Discord unique-username strings into a "
            "text file (one per line) for use with checker.py."
        ),
    )
    parser.add_argument(
        "--length",
        "-n",
        type=int,
        default=DEFAULT_LENGTH,
        help="Username length (default: %(default)s).",
    )
    parser.add_argument(
        "--count",
        "-c",
        type=int,
        default=None,
        help=(
            "How many usernames to generate. In 'random' mode defaults to "
            f"{DEFAULT_RANDOM_COUNT}. In 'all' mode, omit to emit every "
            "combination."
        ),
    )
    parser.add_argument(
        "--mode",
        choices=("all", "random"),
        default="random",
        help="Generation strategy (default: %(default)s).",
    )
    parser.add_argument(
        "--charset",
        choices=tuple(CHARSETS.keys()),
        default=DEFAULT_CHARSET,
        help=(
            "Character set to draw from: "
            "letters (a-z), alnum (a-z + 0-9), or "
            "full (a-z + 0-9 + . _, with Discord's . rules enforced). "
            "Default: %(default)s."
        ),
    )
    parser.add_argument(
        "--output",
        "-o",
        default=DEFAULT_OUTPUT,
        help="File to write to (default: %(default)s).",
    )
    parser.add_argument(
        "--seed",
        type=int,
        default=None,
        help="RNG seed for reproducible 'random' output.",
    )
    parser.add_argument(
        "--append",
        action="store_true",
        help="Append to the output file instead of overwriting it.",
    )
    return parser.parse_args(argv)


def main(argv: Optional[list] = None) -> int:
    args = parse_args(argv)

    if args.length < 1:
        print("error: --length must be >= 1", file=sys.stderr)
        return 2
    if args.count is not None and args.count < 0:
        print("error: --count must be >= 0", file=sys.stderr)
        return 2

    charset = CHARSETS[args.charset]
    space_size = len(charset) ** args.length

    if args.mode == "all" and args.count is not None and args.count < space_size:
        print(
            f"warning: --count {args.count} is less than the full space "
            f"size {space_size}; only the first {args.count} combination(s) "
            "will be written.",
            file=sys.stderr,
        )
    if args.mode == "random" and args.count is not None and args.count > space_size:
        print(
            f"warning: --count {args.count} is larger than the unique "
            f"space size {space_size}; only {space_size} unique value(s) "
            "can be produced.",
            file=sys.stderr,
        )

    rng = random.Random(args.seed) if args.seed is not None else random.Random()

    items = generate(
        length=args.length,
        count=args.count,
        mode=args.mode,
        charset_name=args.charset,
        rng=rng,
    )

    open_mode = "a" if args.append else "w"
    try:
        with open(args.output, open_mode, encoding="utf-8") as fh:
            written = write_stream(fh, items)
    except OSError as exc:
        print(f"error: could not write to {args.output}: {exc}", file=sys.stderr)
        return 2

    verb = "Appended" if args.append else "Wrote"
    print(f"{verb} {written} usernames to {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
