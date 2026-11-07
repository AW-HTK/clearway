# Clearway

Clearway queues heavy tasks (builds, test suites, renders, local AI) between several AI coding sessions on one
Windows + WSL machine, so that they do not exhaust the RAM together. `clearway.exe` (Windows: tray icon, small native
window) grants slots in arrival order with backfilling; sessions ask for them with the `clearway` client (WSL).
Messages are in English or French.

## Install (Windows 10/11, with or without WSL)

Download `clearway-setup.exe` from the [latest release](https://github.com/AW-HTK/clearway/releases/latest) and run
it. It looks for WSL distributions, Claude Code and Codex, shows what it will do and asks once; then it installs, for
the current user and without administrator rights:

- `clearway.exe` in `%LOCALAPPDATA%\Programs\clearway`, on the user `PATH`, started at logon by the `Clearway` task;
- the `clearway` client in `~/.local/bin` of each running WSL distribution;
- the Clearway plugin in each Claude Code and Codex it found (see Plugins below).

Run it again to update. A stopped WSL distribution is left alone: start it and run the setup again. Afterwards, restart
the Claude Code and Codex sessions; in Codex, trust the Clearway hook once with `/hooks`.

The release only carries `clearway-setup.exe` and its signed checksums (`SHA256SUMS`, `SHA256SUMS.sig`, used by the
update check); the setup embeds everything else. `windows/install.ps1` and `install.sh` are what it runs; they also
install from a local `dist/` folder (`CLEARWAY_SOURCE=<dist>`).

## Updates

Nothing updates by itself, neither the programs nor the plugin. The tray menu has **Check for updates** (or set
`update.check = "daily"` in `config.toml`); when a release is newer, Clearway offers it in a balloon and in the menu. A
click downloads `clearway-setup.exe`, checks that `SHA256SUMS` is signed by Clearway's Ed25519 key and that the setup
matches it, then opens the setup, which shows what it changes and asks first. `windows/install.ps1` checks that the
server answering after the switch is the new version, and otherwise puts the previous exe back.

## Build

`scripts/build-release.sh` builds every release file into `dist/` (needs cargo-zigbuild, the targets
`x86_64-pc-windows-gnu` and `x86_64-unknown-linux-musl`, and the signing seed in `CLEARWAY_RELEASE_SIGNING_KEY`;
`examples/sign_release.rs` holds the maintainer's commands). It refuses a release whose plugin changed without a new
plugin version. For development:

```sh
export PATH=$HOME/.cargo/bin:$(echo ~/.local/share/uv/tools/cargo-zigbuild/lib/python*/site-packages/ziglang):$PATH
cargo build --release -j 4 --bin clearway                                          # Linux client (and test server)
cargo zigbuild --release -j 4 --target x86_64-pc-windows-gnu --bin clearway        # clearway.exe
cargo test -j 4    # unit + integration (Linux build of the server on a Unix socket, simulated measures)
```

## Commands

- Server (Windows): `clearway.exe [serve] [--dir <path>] [--test | --test-icon]`. `--test`: nothing on screen.
- Relay: `clearway.exe relay [--pipe <name>]` (stdin/stdout ↔ named pipe; exit 69 if the server is absent, 74 if
  the pipe closes).
- Client (WSL): `clearway run [--side wsl|windows] [--ram 3G] [--cpu 4] [--duration 15m] [--max 9m] "description" --
  command…`, `clearway request`, `wait`, `extend`, `release`, `cancel`, `status`. Exit codes: 0, 1 ticket out of the
  queue, 2 error, 3 refused, 75 still waiting after `--max` or clearance taken back before the acknowledgement (rerun
  the same command to keep the place), 130 interrupted.
- Hook: `clearway hook` / `clearway.exe hook` (PreToolUse event on stdin, refusal on stdout).
- Client variables: `CLEARWAY_EXE`, `CLEARWAY_PIPE`, `CLEARWAY_DIR` (state folder seen from WSL, for the maintenance
  file), `CLEARWAY_LAUNCH_ARGS`, `CLEARWAY_SESSION`, `CLEARWAY_LANG`.

## Plugins (Claude Code and Codex)

This repository is a plugin marketplace for both. The plugin (`plugins/clearway/`) brings the `clearway` MCP server
(`clearway mcp`, so `clearway` or `clearway.exe` must be on the `PATH`), the `heavy-tasks` skill, a PreToolUse hook
(`clearway hook`) and, for Claude Code, the `clearance-waiter` agent. The hook refuses a heavy shell command (cargo
build or test, make, npm run build, pytest, docker build…) until the session holds an active ticket; it lets everything
through when Clearway does not answer within 2 s, and `CLEARWAY_HOOK=off` turns it off. Codex asks the user to trust
plugin hooks first (`/hooks`).

```sh
claude plugin marketplace add AW-HTK/clearway && claude plugin install clearway@clearway --scope user
codex plugin marketplace add AW-HTK/clearway && codex plugin add clearway@clearway
```

In Claude Code the tools are named `mcp__plugin_clearway_clearway__<tool>`. Codex reads the same
`.claude-plugin/marketplace.json` and the plugin's `.codex-plugin/plugin.json`.

## Files

- Exe: `%LOCALAPPDATA%\Programs\clearway\clearway.exe`; scheduled task `Clearway`; installed and updated by
  `windows/install.ps1` (or `clearway-setup.exe`, which runs it).
- State: `%LOCALAPPDATA%\clearway\` (`state.json`, `clearway.log`, `config.toml` written at first start).
- `config.toml`: `pipe`, `legacy_pipe`, `language` (`en`, `fr`, `auto`), reserves, margins, default needs, delays,
  backfill and swap thresholds; the file written at first start documents every key.

## Protocol

- Named pipe `\\.\pipe\clearway` (ACL: current user and SYSTEM, remote clients refused); a Unix socket
  `$XDG_RUNTIME_DIR/clearway-<pipe>.sock` outside Windows. One JSON request per line
  (`{"method","path","params","body"}`), one response per line (`{"code","body"}`).
- Routes: `POST /tickets`, `GET /tickets/{n}/wait` (`known`, `timeout_s`, `ack`), `POST /tickets/{n}/presence`,
  `/extend`, `/release`, `/ack`, `DELETE /tickets/{n}`, `GET /status`, `POST /freeze`, `/window`, `/quit`, `/link`,
  `/link/heartbeat`.
- Multiplexing: a request with an `"id"` runs on its own thread and its response carries the same `id`, in any order;
  without `id`, requests are served one after the other. The relay copies both ways at once (overlapped I/O on
  Windows); at the end of stdin it still delivers the responses to requests without `id`.
- Clearance acknowledgement: `clearway wait` and `clearway run` send `ack=1`, then `POST /tickets/{n}/ack` once the
  clearance has really been delivered; without it within `delay.recall`, the clearance is taken back.
- Links: `POST /link` (`identity`, `session`, `client`, `measures`) ties the connection to a session (MCP process);
  `POST /link/heartbeat` carries the WSL measures and the presence of its active tickets. When the connection closes
  (or after a server restart), its tickets wait `delay.resume` for a link with the same `identity` (active ones stay
  reserved without freezing the queue, waiting ones are skipped), then are removed.

## Compatibility with the pre-Clearway version

- The server also listens on the old pipe `orchestrateur` (`legacy_pipe = true`), accepts the old French dialect
  (`methode`, `chemin`, French routes and keys) and answers it in that dialect.
- Called as `charge` (link to `clearway`), the client keeps the old subcommands (`demande`, `attend`, `fin`, `annule`,
  `prolonge`, `statut`), the old `CHARGE_*` variables and French messages; called as `orchestrateur.exe`, the exe still
  accepts `relais --tube`.
- First start: `%LOCALAPPDATA%\orchestrateur\` is copied to `%LOCALAPPDATA%\clearway\` (old folder kept, with
  `MIGRATED-TO-CLEARWAY.txt`); `etat.json` becomes `state.json`, a French `config.toml` is rewritten in English (backup
  `config.toml.before-clearway`).

## Tests on real Windows (test instance, nothing on screen)

- `tests/windows-api.ps1`: the exe alone, PowerShell client on the pipe.
- `tests/windows-e2e.sh`: end to end from WSL with the real client and relay.
- `tests/window-capture.ps1 <out.png>`: capture of the off-screen window.
