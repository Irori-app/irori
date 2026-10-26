#!/usr/bin/env python3
"""Check a pull request's branch, title and base against Ember's conventions.

The rules are documented in AGENTS.md ("Branches, commits and pull requests"):

- Branch: ``<type>/<short-kebab-description>``, for example ``fix/epic-login``.
- Title: a Conventional Commit header, ``<type>(<scope>): <summary>`` or
  ``<type>: <summary>``, with ``!`` before the colon for a breaking change.
- The branch's type and the title's type are the same.
- The base is ``main``, or another convention-named branch for stacked work.

Bot branches (Dependabot, Renovate) are exempt from the branch rule.

Usage, with the values GitHub provides:

    PR_TITLE=... PR_HEAD=... PR_BASE=... python3 .github/scripts/pr_rules.py

or ``pr_rules.py --title ... --head ... --base ...`` to try a name locally.
Exits 1 and prints every problem when a rule is broken.
"""

from __future__ import annotations

import argparse
import os
import re
import sys

# What each type is for; AGENTS.md has the same table.
TYPES = {
    "feat": "a new user-facing capability",
    "fix": "a bug fix",
    "perf": "a performance or memory improvement with no behaviour change",
    "refactor": "restructuring with no behaviour change",
    "docs": "documentation only (README, docs/, AGENTS.md, doc comments)",
    "test": "tests only",
    "i18n": "translations and locale catalogs only",
    "build": "Cargo manifests, dependencies, packaging/",
    "ci": ".github/ workflows and scripts",
    "chore": "maintenance that fits no other type",
    "revert": "reverting an earlier change",
}

MAIN = "main"
TITLE_MAX = 72
BOT_PREFIXES = ("dependabot/", "renovate/")

_TYPE = "|".join(TYPES)
BRANCH_RE = re.compile(rf"^(?P<type>{_TYPE})/(?P<name>[a-z0-9]+(?:[-.][a-z0-9]+)*)$")
TITLE_RE = re.compile(
    rf"^(?P<type>{_TYPE})(?:\((?P<scope>[a-z0-9]+(?:[-/][a-z0-9]+)*)\))?(?P<breaking>!)?: (?P<summary>.+)$"
)


def branch_type(branch: str) -> str | None:
    match = BRANCH_RE.match(branch)
    return match["type"] if match else None


def check(title: str, head: str, base: str) -> list[str]:
    """Return every rule the pull request breaks (empty when it follows them)."""
    problems: list[str] = []
    types = ", ".join(TYPES)

    bot = head.startswith(BOT_PREFIXES)
    head_type = branch_type(head)
    if not bot and head_type is None:
        problems.append(
            f"Branch '{head}' must be named <type>/<short-kebab-description>, "
            f"for example 'fix/epic-login'. Types: {types}."
        )

    title_match = TITLE_RE.match(title)
    if title_match is None:
        problems.append(
            f"Title '{title}' must be '<type>(<scope>): <summary>' or '<type>: <summary>', "
            f"for example 'fix(epic): keep the session after a token refresh'. Types: {types}."
        )
    else:
        summary = title_match["summary"]
        if summary != summary.strip() or not summary.strip():
            problems.append("The title's summary must not start or end with spaces.")
        if summary.endswith("."):
            problems.append("The title's summary must not end with a period.")
        if summary[:1].isupper() and not summary.split(" ", 1)[0].isupper():
            problems.append(
                "The title's summary must start in lower case ('fix(epic): keep ...'); "
                "only an acronym in capitals (GOG, UI) may lead."
            )
        if head_type and title_match["type"] != head_type:
            problems.append(
                f"The title's type '{title_match['type']}' must match the branch's type "
                f"'{head_type}' ('{head}')."
            )

    if len(title) > TITLE_MAX:
        problems.append(f"The title is {len(title)} characters; keep it to {TITLE_MAX}.")

    if base != MAIN and branch_type(base) is None:
        problems.append(
            f"The base '{base}' must be '{MAIN}', or a convention-named branch when "
            "stacking pull requests. Work merged into any other branch never reaches main."
        )
    return problems


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--title", default=os.environ.get("PR_TITLE"))
    parser.add_argument("--head", default=os.environ.get("PR_HEAD"))
    parser.add_argument("--base", default=os.environ.get("PR_BASE", MAIN))
    args = parser.parse_args(argv)
    if args.title is None or args.head is None:
        parser.error("give --title and --head, or set PR_TITLE and PR_HEAD")

    problems = check(args.title, args.head, args.base)
    for problem in problems:
        # GitHub shows ::error lines as annotations on the run.
        prefix = "::error::" if os.environ.get("GITHUB_ACTIONS") else "error: "
        print(f"{prefix}{problem}")
    if not problems:
        print(f"OK: '{args.title}' from '{args.head}' into '{args.base}'")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
