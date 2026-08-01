---
title: CLI Reference
createTime: 2026/07/31 10:00:00
---

The binary is `tgt`. Run with no arguments it starts the TUI; the subcommands are for inspecting telemetry without launching anything.

## `tgt`

**Synopsis**

```text
tgt [--no-telemetry]
tgt [-h|--help] [-V|--version]
```

**Description**

Starts the terminal client. On first run it shows the telemetry disclosure, then the API credentials wizard if none are configured, then the sign-in screen.

**Flags**

| Flag | Description |
| --- | --- |
| `--no-telemetry` | Disable telemetry for this run, overriding config and environment. It's the master switch, so it covers crash reports and OTLP export together. The config file is not modified. |
| `-h`, `--help` | Print help and exit |
| `-V`, `--version` | Print the version and exit. Terminal modes are never touched, so this is safe to call from a script. |

::: terminal Start the client

```shell
$ tgt

$ tgt --version
tgt 0.x.y

$ tgt --no-telemetry
```

:::

There is no `--config`, no `--theme`, no `--verbose` and no `--list-themes`. Configuration is the file and the environment; see [Configuration](configuration.md).

## `tgt telemetry show`

**Synopsis**

```text
tgt telemetry show
```

**Description**

Prints the state of both egresses without starting the TUI or connecting to anything: a `telemetry:` line for the master switch, a `crash reports:` line with a paragraph describing what a report is made of, and an `OTLP export:` line naming the collector you configured or saying there isn't one. Then the resource attributes with their live values, and every event attribute name with the set of values it may take.

For OTLP that listing really is exactly what a session would send, since every key on it is a schema constant. For crash reports it can't be, and the output says so: a report is assembled out of the failure's own message and stack at the moment something breaks, so its contents aren't enumerable in advance.

`--no-telemetry` applies here too, so `tgt --no-telemetry telemetry show` reports `off` on all three lines. Piping into `head` exits cleanly rather than dying on a broken pipe.

::: terminal Inspect the telemetry settings

```shell
$ tgt telemetry show

$ tgt --no-telemetry telemetry show

$ tgt telemetry show | head -20
```

:::

If you built from source without setting `TGT_SENTRY_DSN`, the crash-reports line admits it: reporting reads as on, but the build has no DSN baked in, so `sentry::init` is never called and nothing is sent.

## `tgt telemetry reset-id`

**Synopsis**

```text
tgt telemetry reset-id
```

**Description**

Regenerates the pseudonymous install id and the HMAC salt used to hash chat ids, both written mode `0600` on Unix. Prints the old and new install id. The salt is never printed.

Regenerating the salt changes every `chat.hash` your install would produce, so past and future events can't be joined. Regenerating the id does the same for the install.

::: terminal Reset the install identity

```shell
$ tgt telemetry reset-id
install id: 3f2a...c1 -> 9b74...0e
```

:::

## Related mise tasks

Building from a checkout, these are the tasks rather than raw cargo invocations. CI calls the same ones.

| Task | What it does |
| --- | --- |
| `mise run run` | Build and start the client from source |
| `mise run build` | Release build |
| `mise run package` | Build `dist/` with the relocatable binary, its dylib, and a tarball |
| `mise run install` | Install the private tree into `$TGT_INSTALL_ROOT` (default `$XDG_DATA_HOME/tgt`), symlinked from `$TGT_BIN_DIR` (default `~/.local/bin`) |
| `mise run uninstall` | Remove that tree and symlink, and clean up the legacy `$TGT_PREFIX/{bin,lib}` layout if one is found |
| `mise run check` | The merge gate: fmt, clippy, tests, crate boundaries |
| `mise tasks` | The full list |

## `tgt update`

Replaces this install with the latest published release. Like the telemetry subcommands it never starts the TUI.

```shell
tgt update
tgt update --require-signature
tgt update --force
```

`--force` installs the latest release even when it is the version already running. It is the repair for a tree that is intact enough to start but broken otherwise (a partial extraction, a missing library), which otherwise has no fix short of reinstalling by hand. It changes only the decision to proceed: the download, both verification steps, the swap, the probe and the rollback are the ones the ordinary path runs, so it is also how that sequence gets exercised without waiting for a newer release to exist.

If the latest published release is *older* than the version you are running, an ordinary `tgt update` refuses and says so rather than silently going backwards. `--force` installs it anyway and names it as a downgrade while it does.

It refuses rather than guessing. A Homebrew install (see [Installation](../getting-started/installation.md#homebrew)) is left to `brew upgrade tgt`, because brew tracks its files in a manifest an in-place overwrite would desynchronise. Anything that isn't a private `bin/` + `lib/` tree it can identify (a legacy shared-prefix install, or a `cargo` target directory) is refused with the reinstall command, since replacing a directory it cannot identify would mean renaming and deleting whatever is there.

The swap itself is the installer's, shipped inside the tarball and run from the newly extracted copy: stage, rename, run the new `bin/tgt --version` while the old tree still exists, and put the old one back if it can't start. There is one implementation of that procedure and both the installer and the updater use it.

It updates the tree the running binary lives in, and only that. If `~/.local/bin/tgt` is this tree's own symlink it is refreshed; if it points at some other install, it is left alone and the update says so. Installing takes that name over, since that's what you asked for, but updating one install must not silently change which `tgt` your PATH finds.

::: warning What "verified" means here
The output states exactly which checks ran. A `SHA256SUMS` match is corruption detection only. The sums file comes from the same host over the same connection as the tarball, so anyone able to serve you a modified tarball can serve you a matching digest.

The cosign signature is the check that means something, and only with the signing identity pinned: given just a bundle, cosign confirms *somebody* signed the blob, not who. It runs when `cosign` is on your PATH; `--require-signature` makes its absence an error instead of a note. There is no unpinned fallback reported as verified, and a release that published neither check is reported as unverified rather than quietly installed.
:::

`tgt update` needs `sh` and `tar`, and `cosign` only for `--require-signature`.
