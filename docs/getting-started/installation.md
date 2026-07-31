---
title: Installation
createTime: 2026/07/31 10:00:00
---

Three ways in: the install script, a release tarball unpacked by hand, or a build from source with [mise](https://mise.jdx.dev). Releases exist for macOS and Linux on both Apple Silicon and x86_64; Windows compiles and is tested in CI but ships no artifact.

## The install script

::: terminal Install the latest release

```shell
$ curl -sSL https://tgt.specht-labs.de/install.sh | sh
tgt installer
  platform:  aarch64-apple-darwin
  tree:      /Users/you/.local/share/tgt
  symlink:   /Users/you/.local/bin/tgt

  release:   v0.1.5

downloading tgt-0.1.5-aarch64-apple-darwin.tar.gz…
  checksum: ok (corruption check only — see the docs on signatures)
extracting…

installed tgt 0.1.5
  /Users/you/.local/bin/tgt -> /Users/you/.local/share/tgt/bin/tgt
```

:::

It detects your OS and architecture, refuses clearly on anything without a published build, and keeps your previous install until the new binary has proved it can start — a failed upgrade puts the working one back rather than leaving you with neither.

`TGT_INSTALL_ROOT` and `TGT_BIN_DIR` override where the tree and the symlink go.

::: warning What the script verifies, and what that is worth
It checks the tarball against `SHA256SUMS` when the release published one, and says so when it didn't — v0.1.4 shipped without it. Take that check for what it is: the sums file comes from the same host over the same connection as the tarball, so it catches a corrupted download and nothing more. Anyone able to serve you a modified tarball can serve you a matching sums file.

The signature that means more is the cosign bundle beside every release. The script doesn't verify it, because almost nobody has cosign installed and a check that usually can't run is not a check. If you want that guarantee, verify by hand before installing, or use `tgt update --require-signature` once you're on 0.1.5 or later.
:::

Piping a script into a shell is worth being uneasy about. [Read it first](https://tgt.specht-labs.de/install.sh) — it's the same file the repo ships at `scripts/install.sh`, copied into the site at build time so the two cannot drift.

## From a release tarball

Releases are on the [releases page](https://github.com/SpechtLabs/telegram-tui/releases) as `tgt-<version>-aarch64-apple-darwin.tar.gz`, with a `SHA256SUMS` file and a keyless cosign bundle (`.cosign.bundle`) alongside it.

::: terminal Install from a tarball

```shell
$ tar -xzf tgt-<version>-aarch64-apple-darwin.tar.gz
$ ls tgt
bin/  lib/

$ install -d ~/.local/bin ~/.local/lib
$ install -m 755 tgt/bin/tgt ~/.local/bin/tgt
$ cp tgt/lib/libtdjson*.dylib ~/.local/lib/

$ tgt --version
```

:::

The `bin/` + `lib/` split is load-bearing. The binary carries an `@executable_path/../lib` rpath and loads TDLib from a dylib next to it, so the two have to stay in that relative arrangement. Moving `bin/tgt` somewhere on its own gets you a dynamic-linker error at startup, not a useful one.

Both the versioned dylib (which is what the binary's load command actually names) and the unversioned alias ship in the tarball. Copy both.

## From source

Building needs mise and nothing else. No Homebrew, no system TDLib, no cmake and gperf and a C++ toolchain: TDLib arrives as a prebuilt library through `tdlib-rs`'s `download-tdlib` feature during the build.

::: terminal Build and install

```shell
$ git clone https://github.com/SpechtLabs/telegram-tui.git
$ cd telegram-tui
$ mise run install
installed tgt 0.x.y to /Users/you/.local/bin/tgt
```

:::

`mise run install` runs `mise run package` first (release build, `dist/` layout, tarball, and a relocation check), then installs the same tree the script does: `~/.local/share/tgt` with a symlink at `~/.local/bin/tgt`. Same overrides:

```shell
TGT_INSTALL_ROOT=/opt/tgt TGT_BIN_DIR=/opt/bin mise run install
```

Earlier versions scattered the binary and the dylib into `$TGT_PREFIX/{bin,lib}` (default `~/.local`), sharing those directories with everything else installed there. `mise run uninstall` cleans up both layouts, so an upgrade from one of those leaves nothing behind.

The first build downloads TDLib and takes a while. Later builds don't.

To run without installing, `mise run run` builds and starts the client from source. `mise run uninstall` removes the binary and its dylib from `$TGT_PREFIX`.

::: warning Linux and Windows are experimental
CI builds and tests the workspace on macOS, Linux and Windows, so the code compiles and the test suite passes on all three. Nobody has actually run the client on Linux or Windows. There is no release artifact for either, `mise run install` has only been exercised on macOS (it copies `*.dylib`), and you should expect to sort out the build yourself. Bug reports are genuinely wanted; assume nothing works until you've seen it work.
:::

## Where it puts things

`tgt` follows the XDG base directory spec through the [`etcetera`](https://docs.rs/etcetera) crate, so these paths honour `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_DATA_HOME` if you've set them.

| What | Path (default) |
| --- | --- |
| Config file | `~/.config/telegram-tui/config.toml` |
| Custom themes | `~/.config/telegram-tui/themes/<name>.toml` |
| Install id and telemetry salt | `~/.config/telegram-tui/install-id`, `~/.config/telegram-tui/telemetry-salt` |
| TDLib database | `~/.local/share/telegram-tui/td/` |
| Application log | `~/.local/state/telegram-tui/tgt.log.<date>` |
| TDLib's own log | `~/.local/state/telegram-tui/tdlib.log` |

The TDLib database is encrypted, and its key lives in the OS credential store (service name `telegram-tui`, entry `db-encryption-key`), never on disk. Which store that is depends on the platform: Keychain on macOS, Credential Manager on Windows, and on Linux a Secret Service provider such as gnome-keyring must be running.

On Unix the database directory is created mode `0700`, and the install id and telemetry salt files mode `0600`. Windows has no equivalent in the current code, so those inherit whatever ACLs the parent directory carries. That's a known gap rather than a decision.

## Next

You need your own Telegram API credentials before the client can connect to anything. [Getting an api_id and api_hash](api-credentials.md) covers that, and it takes about a minute.
