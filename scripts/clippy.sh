#!/usr/bin/env bash
# The project's one clippy command. CI runs exactly this, and so should you: every lint group is an error, rustc
# warnings are errors too (CARGO_BUILD_WARNINGS=deny), and nothing in the code may switch a lint off.
set -euo pipefail

CARGO_BUILD_WARNINGS=deny cargo clippy --workspace --all-targets --all-features --locked -- \
  -D clippy::correctness \
  -D clippy::suspicious \
  -D clippy::complexity \
  -D clippy::perf \
  -D clippy::style \
  -D clippy::pedantic \
  -D clippy::cargo
