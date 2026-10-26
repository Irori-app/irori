"""Tests for pr_rules.py: python3 -m unittest discover -s .github/scripts"""

import unittest

from pr_rules import check


class PullRequestRules(unittest.TestCase):
    def ok(self, title, head, base="main"):
        self.assertEqual(check(title, head, base), [], (title, head, base))

    def broken(self, title, head, base="main", count=1):
        problems = check(title, head, base)
        self.assertEqual(len(problems), count, problems)

    def test_conventional_names_pass(self):
        self.ok("fix(sign-in): open the window without the helper", "fix/in-app-sign-in")
        self.ok("feat: add a login command", "feat/cli-login")
        self.ok("feat(ui)!: replace the sidebar", "feat/new-sidebar")
        self.ok("docs: add AGENTS.md", "docs/agents-md")
        self.ok("i18n(manage): translate the login dialog", "i18n/login-dialog")
        self.ok("build: update wry to 0.54", "build/wry-0.54")
        self.ok("fix(store-epic): keep the session", "fix/epic-session")
        self.ok("fix: drop GOG duplicates", "fix/gog-duplicates")

    def test_branch_needs_a_type_and_kebab_name(self):
        self.broken("fix: x", "claude/translucency")
        self.broken("fix: x", "fix-login")
        self.broken("fix: x", "fix/Login_Page")
        self.broken("fix: x", "feature/login", count=1)

    def test_title_needs_a_conventional_header(self):
        self.broken("Fix the login", "fix/login")
        self.broken("fix:no space", "fix/login")
        self.broken("fix(Epic): keep it", "fix/login")
        self.broken("fixed: keep it", "fix/login")

    def test_summary_style(self):
        self.broken("fix: keep the session.", "fix/login")
        self.broken("fix: Keep the session", "fix/login")
        self.broken("fix: " + "x" * 80, "fix/login")

    def test_types_must_match(self):
        self.broken("feat: add login", "fix/login")

    def test_base_is_main_or_a_stacked_branch(self):
        self.ok("fix: keep it", "fix/login", base="feat/cli-login")
        self.broken("fix: keep it", "fix/login", base="codex/macos-windows-support")

    def test_bots_are_exempt_from_the_branch_rule(self):
        self.ok("build(deps): bump serde from 1.0.1 to 1.0.2", "dependabot/cargo/serde-1.0.2")


if __name__ == "__main__":
    unittest.main()
