# Releasing

## One click

1. On GitHub: **Actions -> Cut release -> Run workflow**. Leave the branch on `main`.
2. Pick **patch**, **minor** or **major** (or type an exact version such as `0.2.0` or `0.2.0-rc1`; a `-` makes it a
   pre-release). On a release candidate, **patch** releases it (`0.5.1-rc1` becomes `0.5.1`); type an exact version
   such as `0.6.0-rc4` for the next candidate.

That is all. The workflow then:

1. runs the full CI on `main` (`cargo fmt`, clippy, the tests, the static build, the release scripts' own tests).
   If anything fails, nothing is bumped or tagged;
2. works out the next version, writes it into every file that carries it, commits `Release X.Y.Z` and pushes the
   commit and the tag `vX.Y.Z` in one atomic push (if `main` moved in the meantime the push is refused and nothing
   is released);
3. builds the static `x86_64-unknown-linux-musl` binary, checks that it needs no shared library and reports the
   version it was built as, packs it into a reproducible `.tar.gz`, unpacks that and runs it again;
4. writes `SHA256SUMS.txt` (and signs it when a key is configured, see below), adds a build-provenance
   attestation when the repository allows it, writes the release notes, creates the release as a draft, uploads
   the files and publishes it.

The very first release (no `v*.*.*` tag yet) publishes the version already in the files as it is (`0.0.1`).

## Release notes

`scripts/release_notes.py` writes the "What's changed" section. It tries, in order:

| Layer | When it is used |
|---|---|
| **AI summary** of the real diff, in plain user-facing language | a key is configured (below), otherwise GitHub Models with the built-in token |
| Commit messages grouped into Added / Fixed / Changed / Removed | no AI service answered |
| What the code shows for vague messages ("Update main.rs"): files per area, new or removed public items | commits that say nothing |

The secrets and variables are the same names as in the Pyxee repository, so the same keys work. Secrets belong to a
repository: add them again here (**Settings -> Secrets and variables -> Actions**), or set them once on the
account or organization and share them with this repository.

| Name | Kind | Meaning |
|---|---|---|
| `ANTHROPIC_API_KEY` | secret | use Claude (default model `claude-sonnet-5-5`) |
| `AI_API_KEY` + `AI_BASE_URL` + `AI_MODEL` | secret + 2 variables | any OpenAI-compatible service |
| `RELEASE_NOTES_MODEL` | variable | override the model for whichever service is used |

Who the notes are written for, and what the project is called, lives in `scripts/project.json`.

Preview the notes for any two tags on your own machine:
`python scripts/release_notes.py --tag v0.2.0 --prev v0.1.0 --repo OWNER/NAME` (add `--ai` with a key in the environment).

## Dry run

**Actions -> Release -> Run workflow**, enter a version. It builds and tests the package and uploads it as a workflow
artifact, and publishes nothing. The version must already be in the files (`python scripts/bump_version.py 0.2.0`
first, on a branch).

## If a release fails

The run summary says what to do. Short version: a flaky step (network, runner) -> **Re-run failed jobs**, which is
safe to repeat. A real problem -> fix it on `main`, `git push --delete origin vX.Y.Z`, and cut the release again.

## Repository settings it relies on

- **Settings -> Actions -> General -> Workflow permissions**: *Read and write*, or the bot cannot push the version
  commit and tag.
- If `main` is protected, allow `github-actions[bot]` to push to it (or use a rules bypass for it). Make the
  **`ci-ok`** check the required one: it fails if any CI job failed.
- Dependabot opens weekly update PRs for GitHub Actions and Cargo (`.github/dependabot.yml`); action versions are
  pinned to commit SHAs, so they only change through those PRs.

## Versions by hand

`python scripts/bump_version.py 0.2.0` sets every file (`Cargo.toml`, `Cargo.lock`, and the web UI's package files
once they exist); `--check v0.2.0` verifies them; `--next minor` prints the next version. CI runs the check on every
push so the files cannot drift apart.

## Signing the checksum list

`SHA256SUMS.txt` proves a download is intact. To prove it was made by this project, the release also carries
`SHA256SUMS.txt.sig`, an Ed25519 signature (128 hex characters) of that exact file.

One-time setup:

1. `python scripts/sign_sums.py keygen` prints a seed and a public key (needs OpenSSL 3, which every current Linux
   and GitHub runner has). Keep the seed secret.
2. Store the seed as the repository **secret** `UPDATE_SIGNING_SEED` and the public key as the repository
   **variable** `UPDATE_PUBLIC_KEY`.
3. The next release signs its list, and checks the signature against the public key before publishing. A release
   that sets the variable but not the secret fails instead of publishing something the server could not trust.

Verify by hand: `python scripts/sign_sums.py verify SHA256SUMS.txt <public key>`.

Rules: once the server only installs signed releases, every release must be signed, so keep the seed safe. Losing it
means installing a new key on the server by hand once.
