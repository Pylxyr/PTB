"""Tests for scripts/bump_version.py and scripts/release_notes.py. Run: python -m unittest discover -s scripts/tests"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import bump_version as bv  # noqa: E402
import release_notes as rn  # noqa: E402

SCRIPTS = Path(__file__).resolve().parent.parent


class NextVersion(unittest.TestCase):
    def test_patch_minor_major(self):
        self.assertEqual(bv.next_version("1.2.3", "patch"), "1.2.4")
        self.assertEqual(bv.next_version("1.2.3", "minor"), "1.3.0")
        self.assertEqual(bv.next_version("1.2.3", "major"), "2.0.0")

    def test_patch_on_a_prerelease_releases_it(self):
        self.assertEqual(bv.next_version("0.5.1-rc1", "patch"), "0.5.1")
        self.assertEqual(bv.next_version("0.6.0-rc3", "patch"), "0.6.0")
        self.assertEqual(bv.next_version("1.2.3-beta.2", "patch"), "1.2.3")

    def test_minor_and_major_on_a_prerelease_drop_the_suffix(self):
        self.assertEqual(bv.next_version("1.2.0-rc1", "minor"), "1.3.0")
        self.assertEqual(bv.next_version("1.2.0-rc1", "major"), "2.0.0")

    def test_bad_input_is_refused(self):
        with self.assertRaises(ValueError):
            bv.next_version("1.2", "patch")
        with self.assertRaises(ValueError):
            bv.next_version("1.2.3", "huge")

    def test_semver_pattern(self):
        for ok in ("0.0.1", "1.2.3", "1.2.3-rc1", "1.2.3-beta.2"):
            self.assertTrue(bv.SEMVER.match(ok), ok)
        for bad in ("1.2", "v1.2.3", "1.2.3.4", "1.2.x", ""):
            self.assertFalse(bv.SEMVER.match(bad), bad)

    def test_the_repo_files_agree_with_each_other(self):
        versions = bv.read_versions()
        self.assertEqual(len(set(versions.values())), 1, versions)
        self.assertEqual(bv.current_version(), next(iter(versions.values())))

    def test_the_lockfile_entries_of_our_own_crates_are_found(self):
        labels = [label for label in bv.read_versions() if label.startswith("Cargo.lock ")]
        self.assertIn("Cargo.lock pyxeetb", labels)
        self.assertIn("Cargo.lock pyxeetb-core", labels)


class LockfileRegex(unittest.TestCase):
    LOCK = (
        'name = "pyxeetb-core"\nversion = "0.0.1"\n\nname = "serde"\nversion = "1.0.0"\n\n'
        'name = "pyxeetb"\nversion = "0.0.1"\n\nname = "pyxeetb-tools-lookalike"\nversion = "3.0.0"\n'
    )

    def test_only_our_own_crates_change(self):
        out = bv.lock_entry("pyxeetb").sub(lambda m: f"{m.group(1)}9.9.9{m.group(3)}", self.LOCK)
        self.assertIn('name = "pyxeetb-core"\nversion = "9.9.9"', out)
        self.assertIn('name = "pyxeetb"\nversion = "9.9.9"', out)
        self.assertIn('name = "serde"\nversion = "1.0.0"', out)

    def test_a_crate_that_merely_starts_with_the_slug_is_still_ours(self):
        # Every `<slug>-*` crate is ours by naming convention; a third-party crate must never be named like that.
        out = bv.lock_entry("pyxeetb").sub(lambda m: f"{m.group(1)}9.9.9{m.group(3)}", self.LOCK)
        self.assertIn('name = "pyxeetb-tools-lookalike"\nversion = "9.9.9"', out)

    def test_another_slug_does_not_match(self):
        self.assertIsNone(bv.lock_entry("other").search(self.LOCK))


class Classify(unittest.TestCase):
    def test_words_and_prefixes(self):
        self.assertEqual(rn.classify("Add probe command"), ("Added", "Add probe command"))
        self.assertEqual(rn.classify("Fix 403 on startup")[0], "Fixed")
        self.assertEqual(rn.classify("feat(core): percentiles")[0], "Added")
        self.assertEqual(rn.classify("Refactor report layout")[0], "Changed")

    def test_planning_labels_are_dropped(self):
        self.assertEqual(rn.classify("Phase 2: economy"), ("Changed", "Economy"))
        self.assertEqual(rn.classify("Phase 2 part 1: duel rules")[1], "Duel rules")

    def test_noise_and_generic_messages(self):
        self.assertIsNone(rn.classify("Release 1.2.0"))
        self.assertIsNone(rn.classify("Merge branch 'x'"))
        self.assertTrue(rn.is_generic("Update main.rs"))
        self.assertTrue(rn.is_generic("Add files via upload"))
        self.assertFalse(rn.is_generic("Fix queue reordering"))

    def test_sections_dedupe_and_skip_generic(self):
        commits = [("a1", "Fix crash"), ("b2", "Fix crash"), ("c3", "Update lib.rs"), ("d4", "Add themes")]
        sections = rn.build_sections(commits)
        self.assertEqual(sections["Fixed"], ["- Fix crash (a1)"])
        self.assertEqual(sections["Added"], ["- Add themes (d4)"])
        self.assertNotIn("Changed", sections)


class CodeAnalysis(unittest.TestCase):
    DIFF = (
        "+++ b/crates/core/src/volume.rs\n+pub struct Volume {\n+pub fn gain() -> f32 {\n-pub fn old_gain() -> f32 {\n"
        "+++ b/crates/bot/src/probe.rs\n+    pub(crate) async fn run() {\n+fn private_helper() {\n"
    )

    def test_detects_public_items(self):
        seen = rn.detect_logic(self.DIFF)
        self.assertEqual(seen["code"]["added"], {"Volume", "gain()", "run()"})
        self.assertEqual(seen["code"]["removed"], {"old_gain()"})

    def test_new_files_are_not_itemised(self):
        seen = rn.detect_logic(self.DIFF, frozenset({"crates/core/src/volume.rs"}))
        self.assertEqual(seen["code"]["added"], {"run()"})

    def test_an_edited_signature_counts_as_neither(self):
        seen = rn.detect_logic("+++ b/a.rs\n-pub fn go(a: i32) {\n+pub fn go(a: i64) {\n")
        self.assertEqual(seen["code"], {"added": set(), "removed": set()})

    def test_only_rust_files_are_read(self):
        seen = rn.detect_logic("+++ b/ui/src/x.ts\n+pub fn looks_like_rust() {\n")
        self.assertEqual(seen["code"]["added"], set())

    def test_areas_and_labels(self):
        self.assertEqual(rn.area_of("crates/core/src/stats.rs"), "Core library")
        self.assertEqual(rn.area_of("crates/bot/src/probe.rs"), "Bot service")
        self.assertEqual(rn.area_of("crates/twitch/src/irc.rs"), "Twitch protocol logic")
        self.assertEqual(rn.area_of("crates/bot/README.md"), "Documentation")
        self.assertEqual(rn.area_of("crates/bot/tests/cli.rs"), "Tests")
        self.assertEqual(rn.area_of(".github/workflows/ci.yml"), "Build and release")
        self.assertEqual(rn.area_of("somewhere/else.txt"), "Other")
        group = ["crates/bot/src/main.rs", "crates/core/src/main.rs", "ui/src/App.svelte"]
        self.assertNotEqual(rn._label(group[0], group), rn._label(group[1], group))
        self.assertEqual(rn._label(group[2], group), "App.svelte")

    def test_net_file_state(self):
        net, counts = rn.merge_file_states(
            [({"a": "A", "b": "M"}, {"a": (5, 0), "b": (1, 1)}), ({"a": "D", "b": "M", "c": "D"}, {"b": (2, 0)})]
        )
        self.assertEqual(net, {"b": "M", "c": "D"})
        self.assertEqual(counts["b"], (3, 1))

    def test_rendering_lists_areas_and_public_items(self):
        status = {"crates/core/src/stats.rs": "A", "Cargo.lock": "M"}
        counts = {"crates/core/src/stats.rs": (40, 0)}
        logic = {"code": {"added": {"summarize()"}, "removed": set()}}
        lines = rn.render_code_changes(status, counts, logic)
        self.assertEqual(lines[0], "- **Core library:** new `stats.rs` (+40 -0)")
        self.assertIn("- New public items: `summarize()`", lines)
        self.assertFalse(any("Cargo.lock" in line for line in lines), "lockfiles are never listed")


class AiPlumbing(unittest.TestCase):
    def test_provider_order(self):
        self.assertIsNone(rn.pick_provider({}))
        both = {"ANTHROPIC_API_KEY": "k", "AI_API_KEY": "x", "AI_BASE_URL": "https://h", "AI_MODEL": "m", "GITHUB_TOKEN": "t"}
        self.assertEqual(rn.pick_provider(both).name, "Claude")
        del both["ANTHROPIC_API_KEY"]
        self.assertEqual(rn.pick_provider(both).name, "AI")
        self.assertEqual(rn.pick_provider({"GITHUB_TOKEN": "t"}).name, "GitHub Models")

    def test_openai_compatible_needs_a_model(self):
        self.assertIsNone(rn.pick_provider({"AI_API_KEY": "x", "AI_BASE_URL": "https://h"}))

    def test_the_model_can_be_overridden(self):
        provider = rn.pick_provider({"ANTHROPIC_API_KEY": "k", "RELEASE_NOTES_MODEL": "my-model"})
        self.assertEqual(provider.model, "my-model")

    def test_prompt_stays_in_budget_and_drops_lockfiles(self):
        diff = "diff --git a/Cargo.lock b/Cargo.lock\n+x\n" + "".join(
            f"diff --git a/ui/src/F{i}.svelte b/ui/src/F{i}.svelte\n+" + "y" * 2000 + "\n" for i in range(30)
        )
        prompt = rn.build_prompt("v1.0.0", "v0.9.0", [("a", "Add x")], "stat", diff, 10_000)
        self.assertLess(len(prompt), 10_000)
        self.assertNotIn("Cargo.lock", prompt.split("## Diff")[1].split("[files changed")[0])
        self.assertIn("files changed but not shown", prompt)

    def test_output_token_limit_is_clamped(self):
        self.assertEqual(rn._max_output_tokens({}), 3000)
        self.assertEqual(rn._max_output_tokens({"AI_MAX_OUTPUT_TOKENS": "5"}), 256)
        self.assertEqual(rn._max_output_tokens({"AI_MAX_OUTPUT_TOKENS": "999999"}), 16000)
        self.assertEqual(rn._max_output_tokens({"AI_MAX_OUTPUT_TOKENS": "abc"}), 3000)

    def test_the_prompt_is_about_this_project_and_nothing_else(self):
        self.assertIn("PyxeeTB", rn._AI_SYSTEM)
        self.assertIn("Twitch", rn._AI_SYSTEM)
        for stale in ("Pyxee,", "yt-dlp", "Tauri", "music", "Svelte"):
            self.assertNotIn(stale, rn._AI_SYSTEM)
        self.assertNotIn("{", rn._AI_SYSTEM.replace("{}", ""), "every placeholder in the template was filled")


class TidyAiNotes(unittest.TestCase):
    def test_intro_fences_and_unknown_headings_are_dropped(self):
        text = "Here are the notes:\n```\n### Added\n- A thing\n\n### Misc\n- ignored\n### Fixed\n- A bug\n```\nBye"
        self.assertEqual(rn.tidy_ai_notes(text), "### Added\n- A thing\n\n### Fixed\n- A bug")

    def test_the_same_change_under_two_headings_keeps_the_fix(self):
        text = "### Added\n- Fixed clicks between tracks\n### Fixed\n- Fixed clicks between tracks\n"
        out = rn.tidy_ai_notes(text)
        self.assertEqual(out, "### Fixed\n- Fixed clicks between tracks")

    def test_sections_come_out_in_a_fixed_order_and_empty_ones_vanish(self):
        text = "### Removed\n- Old command\n### Added\n- New command\n"
        self.assertEqual(rn.tidy_ai_notes(text), "### Added\n- New command\n\n### Removed\n- Old command")

    def test_wrapped_bullets_are_joined(self):
        text = "### Added\n- A long bullet\n  that wraps\n"
        self.assertEqual(rn.tidy_ai_notes(text), "### Added\n- A long bullet that wraps")

    def test_nothing_usable_gives_none(self):
        self.assertIsNone(rn.tidy_ai_notes("no headings at all"))
        self.assertIsNone(rn.tidy_ai_notes("### Added\n"))


class Render(unittest.TestCase):
    def test_first_release(self):
        self.assertIn("First release.", rn.render("v0.1.0", None, "o/r", []))

    def test_ai_text_is_labelled(self):
        out = rn.render("v0.2.0", "v0.1.0", "o/r", [], ai_text="### Added\n- A thing")
        self.assertIn("Summarised automatically by AI", out)
        self.assertIn("compare/v0.1.0...v0.2.0", out)

    def test_empty_release(self):
        self.assertIn("Maintenance release", rn.render("v0.2.0", "v0.1.0", "", []))


def _git(cwd: str, *args: str) -> str:
    env = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}
    return subprocess.run(["git", *args], cwd=cwd, env=env, check=True, capture_output=True, encoding="utf-8").stdout


class EndToEnd(unittest.TestCase):
    """The script run for real, in a throwaway repository with two tags."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = self.tmp.name
        _git(self.repo, "init", "-q", "-b", "main")
        Path(self.repo, "a.txt").write_text("1\n", encoding="utf-8")
        _git(self.repo, "add", "-A")
        _git(self.repo, "commit", "-qm", "Initial commit")
        _git(self.repo, "tag", "v0.0.1")
        for name, subject in (("b.txt", "Add probe command"), ("c.txt", "Fix report layout"), ("d.rs", "Update d.rs")):
            Path(self.repo, name).write_text("pub fn thing() {}\n" if name.endswith(".rs") else "x\n", encoding="utf-8")
            _git(self.repo, "add", "-A")
            _git(self.repo, "commit", "-qm", subject)
        _git(self.repo, "tag", "v0.0.2")

    def run_script(self, *extra: str, env: dict[str, str] | None = None) -> str:
        clean = {k: v for k, v in os.environ.items() if k not in ("ANTHROPIC_API_KEY", "AI_API_KEY", "GITHUB_TOKEN")}
        done = subprocess.run(
            [sys.executable, str(SCRIPTS / "release_notes.py"), "--tag", "v0.0.2", "--repo", "o/r", *extra],
            cwd=self.repo, env={**clean, **(env or {})}, check=True, capture_output=True, encoding="utf-8",
        )
        return done.stdout

    def test_the_previous_release_is_found_and_commits_are_grouped(self):
        out = self.run_script()
        self.assertIn("## What's changed since v0.0.1", out)
        self.assertIn("### Added\n- Add probe command", out)
        self.assertIn("### Fixed\n- Fix report layout", out)
        self.assertIn("compare/v0.0.1...v0.0.2", out)

    def test_a_vague_commit_is_read_from_the_code(self):
        out = self.run_script()
        self.assertIn("(detected from the code)", out)
        self.assertIn("`d.rs`", out)

    def test_without_any_ai_key_the_ai_flag_falls_back_quietly(self):
        out = self.run_script("--ai")
        self.assertIn("### Added", out)
        self.assertNotIn("Summarised automatically by AI", out)

    def test_a_model_reply_is_cleaned_and_labelled(self):
        # Drive ai_summary in-process with a fake model, inside the throwaway repository.
        calls = {}

        def fake_ask(provider, prompt):
            calls["prompt"] = prompt
            return "Sure!\n### Added\n- A probe command measures the server.\n### Fixed\n- A probe command measures the server.\n"

        old_cwd = os.getcwd()
        old_ask, old_pick = rn.ask_model, rn.pick_provider
        rn.ask_model = fake_ask
        rn.pick_provider = lambda env=None: rn.Provider("Fake", "openai", "https://x", "k", "m", 20_000)
        try:
            os.chdir(self.repo)
            text = rn.ai_summary("v0.0.1", "v0.0.2", [("a", "Add probe command")])
        finally:
            os.chdir(old_cwd)
            rn.ask_model, rn.pick_provider = old_ask, old_pick
        self.assertEqual(text, "### Fixed\n- A probe command measures the server.")
        self.assertIn("## Diff", calls["prompt"])
        self.assertIn("b.txt", calls["prompt"])

    def test_a_malformed_model_reply_is_rejected(self):
        old_cwd = os.getcwd()
        old_ask, old_pick = rn.ask_model, rn.pick_provider
        rn.ask_model = lambda provider, prompt: "I cannot help with that."
        rn.pick_provider = lambda env=None: rn.Provider("Fake", "openai", "https://x", "k", "m", 20_000)
        try:
            os.chdir(self.repo)
            self.assertIsNone(rn.ai_summary("v0.0.1", "v0.0.2", []))
        finally:
            os.chdir(old_cwd)
            rn.ask_model, rn.pick_provider = old_ask, old_pick


if __name__ == "__main__":
    unittest.main()
