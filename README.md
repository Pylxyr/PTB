# PyxeeTB

A fast, self-hosted Twitch chat bot, alerts and overlay engine, written in Rust and shipped as **one static
binary** for a small Linux server (built for an Oracle E2.Micro: 1 GB of memory, 1/8 of a CPU).

> **Status: Phase 1a.** Three working commands: `probe` measures the server, `auth` logs in to Twitch, and
> `latency` measures real chat latency. The bot itself comes next, in phases. Nothing here replaces the Python
> PyxeeTB yet.

## Phase 0: measure the server

```text
pyxeetb probe [--samples N] [--burst-seconds S] [--jitter-seconds S] [--out FILE]
```

Times DNS, TCP, TLS and the first answer to Twitch's Helix, EventSub and IRC servers; runs a CPU test that
shows throttling and hypervisor steal; and an idle test that shows how late the machine wakes a sleeping
thread. It writes `probe-report.txt` and needs no Twitch account (the chat check logs in as an anonymous,
read-only `justinfan` user).

## Phase 1a: log in and measure real chat latency

You need a Twitch developer application (https://dev.twitch.tv/console). Any client type works: a public app
needs only its client id, a confidential app also needs its secret to refresh the login.

```sh
export PYXEETB_CLIENT_ID=your-client-id
export PYXEETB_CLIENT_SECRET=your-secret   # confidential apps only; never put it on the command line
./pyxeetb auth                             # prints a short code; approve it in a browser on any device
./pyxeetb latency --count 10               # posts 10 short test messages in your channel and times them
```

- `auth` uses the device code flow, so a headless server never needs a browser. The login is saved in
  `~/.config/pyxeetb/tokens.json` (override with `--state-dir` or `PYXEETB_STATE_DIR`), readable by you only.
  It is renewed automatically, and written so that a crash cannot lose it (Twitch replaces the refresh token on
  every renewal).
- `latency` sends each test message through Helix and waits for it to come back over **both** IRC and EventSub,
  timing everything on this machine's own clock. It reports the send round trip, how long each route takes to
  deliver, and which is first. Viewers see the test messages, so run it while offline or in a test channel;
  it pauses five seconds before sending so you can cancel with Ctrl+C. The report is saved as
  `latency-report.txt`.
- Twitch keeps one refresh token per user and app, so logging in with the same app that Python PyxeeTB uses may
  sign that bot out. Register a second app if both must run at once.

## How it is built

Everything is built by GitHub Actions; nothing is ever compiled on the server.

- Every change runs `cargo fmt`, the strict clippy command (`scripts/clippy.sh`), the tests, and the exact static
  build that ships.
- **Actions -> Cut release -> Run workflow** bumps the version, tags, builds, signs the checksum list when a key is
  configured, writes release notes with AI (from the real diff) and publishes. See [docs/releasing.md](docs/releasing.md).

## Layout

| Path | What |
|---|---|
| `crates/core` | Dependency-free building blocks: statistics, `/proc` parsing, report layout |
| `crates/twitch` | Twitch protocol logic with no I/O: OAuth device flow, Helix bodies, EventSub frames, IRC lines |
| `crates/bot` | The `pyxeetb` program: commands, HTTP, WebSocket and IRC connections, token storage |
| `scripts/` | Version bump, release notes, checksum signing, and their tests |
| `.github/` | CI, release and dependency-update workflows |

## License
MIT, see [LICENSE](LICENSE).

## Code quality bar

`scripts/clippy.sh` runs clippy with every group denied (`correctness`, `suspicious`, `complexity`, `perf`,
`style`, `pedantic`, `cargo`) and every rustc warning an error. No lint is switched off anywhere in the code, and
a test fails if one appears. The single accepted exception is a duplicate `getrandom` in the dependency tree
(the newest `ring` and the newest `tungstenite` each need a different one), recorded with its reason in
`clippy.toml`.
