#!/usr/bin/env python3
"""Release helpers for Dash OS.

The GitHub Actions workflows are thin wrappers around this, so the logic that
decides a version number and rewrites the CHANGELOG is plain Python that can be
run and tested on a laptop:

    release.py next-version --current 0.1.0 --intent patch
    release.py rewrite --changelog CHANGELOG.md --version 0.1.1 --date 2026-10-05
    release.py intent --message-file /tmp/msg
    release.py guard-bump --message-file /tmp/msg

Semantics, from AGENTS.md:

    patch   0.1.0 -> 0.1.1   a fix, no new content
    minor   0.1.0 -> 0.2.0   a new device-visible change; the minor slot is the
                             release slot
    major   0.9.3 -> 1.0.0   the pre-1.0 graduation. Past 1.0.0 it is a normal
                             major bump. 1.0.0 is a decision, not a date, but
                             when it is taken the mechanics are ordinary.

Exit codes are meaningful: 0 success, 2 bad input, 3 precondition not met.
"""

import argparse
import datetime
import re
import sys

PLACEHOLDER = "<!-- CHANGELOG-PLACEHOLDER -->"
UNRELEASED = "## [Unreleased]"
INTENTS = ("patch", "minor", "major")

VERSION_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")
INTENT_RE = re.compile(r"^Release-Intent:\s*(patch|minor|major)\s*$", re.MULTILINE)
SKIP_BUMP = "[skip-bump]"


def die(msg, code=2):
    print(f"release.py: {msg}", file=sys.stderr)
    sys.exit(code)


def parse_version(text):
    text = text.strip()
    m = VERSION_RE.match(text)
    if not m:
        die(f"not a MAJOR.MINOR.PATCH version: {text!r}")
    return int(m.group(1)), int(m.group(2)), int(m.group(3))


def next_version(current, intent):
    """Return the version that `intent` produces from `current`."""
    if intent not in INTENTS:
        die(f"unknown intent {intent!r}; want one of {', '.join(INTENTS)}")
    major, minor, patch = parse_version(current)

    if intent == "patch":
        return f"{major}.{minor}.{patch + 1}"
    if intent == "minor":
        return f"{major}.{minor + 1}.0"
    # major: while pre-1.0, "major" means graduating to 1.0.0 -- not 1.0.1.
    if major == 0:
        return "1.0.0"
    return f"{major + 1}.0.0"


def cmd_next_version(args):
    print(next_version(args.current, args.intent))


def split_sections(text):
    """Split the changelog into (preamble, [(heading, body), ...]).

    A section runs from its `## [ ... ]` heading to the next one. Anything
    before the first heading is the preamble.
    """
    lines = text.splitlines(keepends=True)
    sections = []
    preamble = []
    current = None
    for line in lines:
        if line.startswith("## ["):
            if current is not None:
                sections.append(current)
            current = [line, []]
        elif current is None:
            preamble.append(line)
        else:
            current[1].append(line)
    if current is not None:
        sections.append(current)
    return "".join(preamble), sections


def rewrite_changelog(path, version, date):
    with open(path, encoding="utf-8") as fh:
        text = fh.read()

    preamble, sections = split_sections(text)
    if not sections or sections[0][0].strip() != UNRELEASED:
        die(
            f"{path} must begin its version list with '{UNRELEASED}'; "
            f"found {sections[0][0].strip() if sections else 'no sections'!r}"
        )

    _, body = sections[0]

    # The placeholder is a template marker, not content. A release that shipped
    # the placeholder would put "describe your change here" in the tag notes.
    kept = [ln for ln in body if ln.strip() != PLACEHOLDER]
    # Collapse the blank lines the placeholder left behind, but keep the body
    # otherwise byte-for-byte: this is someone's prose.
    while kept and not kept[0].strip():
        kept.pop(0)
    while kept and not kept[-1].strip():
        kept.pop()
    if not kept:
        die(
            f"the Unreleased section of {path} has no content beyond the "
            f"placeholder; there is nothing to release"
        )

    out = []
    out.append(preamble)
    out.append(f"{UNRELEASED}\n\n{PLACEHOLDER}\n\n")
    out.append(f"## [{version}] — {date}\n\n")
    out.extend(kept)
    out.append("\n")

    # Re-attach any sections that followed Unreleased (there should not be any,
    # but silently dropping a maintainer's notes would be worse than failing).
    for heading, rest in sections[1:]:
        out.append(heading)
        out.extend(rest)

    with open(path, "w", encoding="utf-8") as fh:
        fh.write("".join(out))


def cmd_rewrite(args):
    date = args.date or datetime.date.today().isoformat()
    rewrite_changelog(args.changelog, args.version, date)
    print(f"{args.changelog}: released {args.version} ({date})")


def read_message(args):
    if args.message_file == "-":
        return sys.stdin.read()
    with open(args.message_file, encoding="utf-8") as fh:
        return fh.read()


def cmd_intent(args):
    m = INTENT_RE.search(read_message(args))
    if not m:
        die("no 'Release-Intent: patch|minor|major' line in the commit message")
    print(m.group(1))


def cmd_guard_bump(args):
    """Exit 0 if this commit is a bump commit, i.e. must not trigger another.

    The bump commit pushes to main, which re-triggers this workflow. Without
    this guard the bump would bump again, forever.
    """
    sys.exit(0 if SKIP_BUMP in read_message(args) else 1)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("next-version", help="print the version an intent produces")
    p.add_argument("--current", required=True)
    p.add_argument("--intent", required=True, choices=INTENTS)
    p.set_defaults(fn=cmd_next_version)

    p = sub.add_parser("rewrite", help="release the Unreleased section in place")
    p.add_argument("--changelog", required=True)
    p.add_argument("--version", required=True)
    p.add_argument("--date", default=None, help="YYYY-MM-DD; default today")
    p.set_defaults(fn=cmd_rewrite)

    p = sub.add_parser("intent", help="print the release intent of a commit message")
    p.add_argument("--message-file", required=True, help="path, or - for stdin")
    p.set_defaults(fn=cmd_intent)

    p = sub.add_parser(
        "guard-bump", help="exit 0 if the message marks a bump commit (recursion guard)"
    )
    p.add_argument("--message-file", required=True, help="path, or - for stdin")
    p.set_defaults(fn=cmd_guard_bump)

    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
