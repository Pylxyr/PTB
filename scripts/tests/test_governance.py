"""Repository governance files, the workflows' safety rules and the secret flow into release.yml.

Text checks only (no YAML library needed), because workflows cannot be run locally: these tests are what catches
a careless edit before it reaches GitHub."""

import json
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = {p.name: p.read_text(encoding="utf-8") for p in sorted((ROOT / ".github/workflows").glob("*.yml"))}
RELEASE = WORKFLOWS["release.yml"]
CUT = WORKFLOWS["cut-release.yml"]
CI = WORKFLOWS["ci.yml"]
SECRETS = ["ANTHROPIC_API_KEY", "AI_API_KEY", "UPDATE_SIGNING_SEED"]


def block(text: str, header: str) -> str:
    """The indented block that follows `header` (a line ending in a colon)."""
    lines = text.splitlines()
    start = next(i for i, l in enumerate(lines) if l.strip() == header)
    indent = len(lines[start]) - len(lines[start].lstrip())
    out = []
    for l in lines[start + 1:]:
        if l.strip() and len(l) - len(l.lstrip()) <= indent:
            break
        out.append(l)
    return "\n".join(out)


def run_blocks(text: str) -> list[str]:
    """The bodies of every multi-line `run: |` step."""
    lines = text.splitlines()
    bodies = []
    for i, line in enumerate(lines):
        if re.match(r"^\s*(- )?run:\s*[|>][-+]?\s*$", line):
            indent = len(line) - len(line.lstrip())
            if line.lstrip().startswith("- "):
                indent += 2
            body = []
            for l in lines[i + 1:]:
                if l.strip() and len(l) - len(l.lstrip()) <= indent:
                    break
                body.append(l)
            bodies.append("\n".join(body))
    return bodies


class Security(unittest.TestCase):
    def test_security_md_uses_private_reporting_and_invents_no_address(self):
        text = (ROOT / "SECURITY.md").read_text(encoding="utf-8")
        self.assertIn("private vulnerability reporting", text.lower())
        self.assertIn("Report a vulnerability", text)
        self.assertNotRegex(text, r"[\w.+-]+@[\w-]+\.[\w.]+", "no email address: none was provided")


class WorkflowSafety(unittest.TestCase):
    def test_there_are_exactly_the_three_workflows_the_docs_describe(self):
        self.assertEqual(sorted(WORKFLOWS), ["ci.yml", "cut-release.yml", "release.yml"])

    def test_every_third_party_action_is_pinned_to_a_full_commit_sha(self):
        for name, text in WORKFLOWS.items():
            for use in re.findall(r"(?m)^\s*(?:- )?uses:\s*(\S+)", text):
                if use.startswith("./"):
                    continue
                self.assertRegex(use, r"^[\w.-]+/[\w./-]+@[0-9a-f]{40}$", f"{name}: '{use}' is not pinned to a commit SHA")

    def test_every_workflow_declares_its_permissions_at_the_top(self):
        for name, text in WORKFLOWS.items():
            self.assertRegex(text, r"(?m)^permissions:\n  contents: read\n", f"{name}: default permissions must be read-only")

    def test_no_pull_request_target_and_no_secrets_inherit(self):
        for name, text in WORKFLOWS.items():
            self.assertNotIn("pull_request_target", text, name)
            self.assertNotRegex(text, r"(?m)^\s*secrets:\s*inherit\b", name)

    def test_no_expression_is_pasted_into_a_shell_script(self):
        # `${{ ... }}` inside a run block is script injection waiting to happen; values go through env: instead.
        for name, text in WORKFLOWS.items():
            for body in run_blocks(text):
                self.assertNotIn("${{", body, f"{name}: an expression inside a run block:\n{body}")

    def test_no_expression_is_pasted_into_a_one_line_run_step_either(self):
        for name, text in WORKFLOWS.items():
            for line in re.findall(r"(?m)^\s*(?:- )?run:\s+(?![|>])(.+)$", text):
                self.assertNotIn("${{", line, f"{name}: {line}")

    def test_the_run_block_finder_actually_finds_blocks(self):
        self.assertGreater(len(run_blocks(RELEASE)), 5)
        self.assertGreater(len(run_blocks(CUT)), 2)
        self.assertGreaterEqual(len(run_blocks(CI)), 3)

    def test_ci_runs_everything_that_ships_and_has_one_required_check(self):
        for step in ("cargo fmt --all -- --check", "cargo clippy --workspace --all-targets --locked -- -D warnings",
                     "cargo test --workspace --locked", "--target x86_64-unknown-linux-musl", "check-binary.sh",
                     "unittest discover -s scripts/tests", "bump_version.py --check"):
            self.assertIn(step, CI, step)
        self.assertIn("ci-ok:", CI)
        self.assertIn("needs: [rust, scripts, deny]", CI, "ci-ok must wait for every job")

    def test_the_toolchain_comes_from_one_file(self):
        for name, text in WORKFLOWS.items():
            self.assertNotRegex(text, r"toolchain:\s*\d", f"{name}: the Rust version belongs in rust-toolchain.toml")
        toolchain = (ROOT / "rust-toolchain.toml").read_text(encoding="utf-8")
        self.assertIn('targets = ["x86_64-unknown-linux-musl"]', toolchain)

    def test_releases_are_cut_from_main_only(self):
        self.assertIn("github.ref != 'refs/heads/main'", CUT)
        self.assertIn("github.ref == 'refs/heads/main'", CUT)
        self.assertIn("git push --atomic origin HEAD:refs/heads/main", CUT)
        self.assertIn("is not on main", RELEASE)

    def test_publish_waits_for_checks_or_their_deliberate_skip_and_for_the_build(self):
        publish = block(RELEASE, "publish:")
        self.assertIn("needs.checks.result == 'success' || needs.checks.result == 'skipped'", publish)
        self.assertIn("needs.build.result == 'success'", publish)
        self.assertIn("!cancelled()", publish)

    def test_the_release_is_a_draft_until_it_is_complete(self):
        self.assertIn("--draft", RELEASE)
        self.assertIn("--draft=false", RELEASE)
        self.assertIn("--verify-tag", RELEASE)

    def test_the_build_checks_the_binary_before_anything_is_published(self):
        build = block(RELEASE, "build:")
        self.assertIn('check-binary.sh "target/$TARGET/release/pyxeetb" "$VERSION"', build)
        self.assertIn("The unpacked package runs", build)

    def test_the_same_toolchain_target_is_built_everywhere(self):
        self.assertIn("x86_64-unknown-linux-musl", RELEASE)
        self.assertIn("CC_x86_64_unknown_linux_musl: musl-gcc", RELEASE)
        self.assertIn("CC_x86_64_unknown_linux_musl: musl-gcc", CI)


class SecretFlow(unittest.TestCase):
    def test_cut_release_no_longer_inherits_everything(self):
        self.assertNotRegex(CUT, r"(?m)^\s*secrets:\s*inherit\b")

    def test_release_declares_exactly_the_secrets_it_uses(self):
        declared = block(block(RELEASE, "workflow_call:"), "secrets:")
        names = re.findall(r"(?m)^\s{6}([A-Z0-9_]+):\s*$", declared)
        self.assertEqual(sorted(names), sorted(SECRETS))
        self.assertEqual(declared.count("required: false"), len(SECRETS), "optional: unset behaves as before")
        used = set(re.findall(r"secrets\.([A-Z0-9_]+)", RELEASE)) - {"GITHUB_TOKEN"}
        self.assertEqual(used, set(SECRETS), "release.yml uses a secret that is not declared, or declares one it never uses")

    def test_cut_release_passes_each_declared_secret_explicitly(self):
        passed = block(CUT, "secrets:")
        for name in SECRETS:
            self.assertRegex(passed, rf"(?m)^\s+{name}: \${{{{ secrets\.{name} }}}}\s*$")
        self.assertEqual(len(re.findall(r"(?m)^\s+[A-Z0-9_]+: \$\{\{ secrets\.", passed)), len(SECRETS))

    def test_tag_push_and_manual_runs_still_use_repository_secrets_directly(self):
        # Those triggers need no declaration: the secrets context comes from the repository.
        self.assertIn("push:", RELEASE)
        self.assertIn("workflow_dispatch:", RELEASE)
        for name in SECRETS:
            self.assertIn(f"secrets.{name}", RELEASE)

    def test_the_ai_key_names_match_the_pyxee_repository_so_the_same_keys_work(self):
        for name in ("ANTHROPIC_API_KEY", "AI_API_KEY"):
            self.assertIn(f"secrets.{name}", RELEASE)
        for name in ("AI_BASE_URL", "AI_MODEL", "RELEASE_NOTES_MODEL"):
            self.assertIn(f"vars.{name}", RELEASE)

    def test_models_read_is_granted_where_github_models_is_the_fallback(self):
        self.assertIn("models: read", block(RELEASE, "publish:"))
        self.assertIn("models: read", block(CUT, "release:"))

    def test_the_signing_step_refuses_a_half_configured_state(self):
        self.assertIn("Refusing to publish", RELEASE)
        self.assertIn("sign_sums.py verify release/SHA256SUMS.txt", RELEASE)


class Project(unittest.TestCase):
    def test_the_slug_names_the_binary_crate_and_the_release_files(self):
        project = json.loads((ROOT / "scripts/project.json").read_text(encoding="utf-8"))
        self.assertEqual(project["slug"], "pyxeetb")
        bot = (ROOT / "crates/bot/Cargo.toml").read_text(encoding="utf-8")
        self.assertIn(f'name = "{project["slug"]}"', bot)
        self.assertIn(f'pyxeetb-${{VERSION}}-${{TARGET}}', RELEASE)

    def test_the_docs_mention_every_workflow(self):
        docs = (ROOT / "docs/releasing.md").read_text(encoding="utf-8")
        for title in ("Cut release", "Release"):
            self.assertIn(title, docs)


if __name__ == "__main__":
    unittest.main()
