# PyxeeTB

A fast, self-hosted Twitch chat bot, alerts and overlay engine, written in Rust and shipped as **one static
binary** for a small Linux server (built for an Oracle E2.Micro: 1 GB of memory, 1/8 of a CPU).

> **Status: Phase 0.** The program does one thing so far: `pyxeetb probe` measures the machine it runs on and its
> path to Twitch. The bot itself comes next, in phases. Nothing here replaces the Python PyxeeTB yet.

## Phase 0: measure the server first

Design decisions that depend on the server (which region is fast to Twitch, whether the CPU gets throttled once
its burst credit is spent) are made from numbers, not guesses.

1. Download the newest release's `pyxeetb-x.y.z-x86_64-unknown-linux-musl.tar.gz` on the server and unpack it.
2. Run `./pyxeetb probe` (about a minute). It writes `probe-report.txt`.
3. Paste the report back into the planning conversation.

```text
pyxeetb probe [--samples N] [--burst-seconds S] [--out FILE]
```

It never needs a Twitch account: the chat check logs in as an anonymous, read-only `justinfan` user.

## How it is built

Everything is built by GitHub Actions; nothing is ever compiled on the server.

- Every change runs `cargo fmt`, `clippy -D warnings`, the tests, and the exact static build that ships.
- **Actions -> Cut release -> Run workflow** bumps the version, tags, builds, signs the checksum list when a key is
  configured, writes release notes with AI (from the real diff) and publishes. See [docs/releasing.md](docs/releasing.md).

## Layout

| Path | What |
|---|---|
| `crates/core` | Dependency-free building blocks: statistics, `/proc` parsing, report layout |
| `crates/bot` | The `pyxeetb` program |
| `scripts/` | Version bump, release notes, checksum signing, and their tests |
| `.github/` | CI, release and dependency-update workflows |

## License
MIT, see [LICENSE](LICENSE).
