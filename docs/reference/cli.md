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
| `--no-telemetry` | Disable telemetry for this run, overriding config and environment. The config file is not modified. |
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

Prints exactly what a session would send: the current telemetry mode, the destination, every resource attribute with its live value, and every event attribute name with the set of values it may take. It never starts the TUI and never connects to anything.

`--no-telemetry` applies here too, so `tgt --no-telemetry telemetry show` reports mode `off`. Piping into `head` exits cleanly rather than dying on a broken pipe.

::: terminal Inspect what would be sent

```shell
$ tgt telemetry show

$ tgt --no-telemetry telemetry show

$ tgt telemetry show | head -20
```

:::

If you built from source without setting `TGT_INGEST_ENDPOINT`, the destination line says so: vendor mode is inert in a build with no endpoint baked in.

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
| `mise run install` | Install into `$TGT_PREFIX` (default `~/.local`) |
| `mise run uninstall` | Remove the binary and dylib from `$TGT_PREFIX` |
| `mise run check` | The merge gate: fmt, clippy, tests, crate boundaries |
| `mise tasks` | The full list |
