---
title: API credentials
createTime: 2026/07/31 10:00:00
---

Telegram requires every third-party client to identify itself with an `api_id` and `api_hash` issued to a developer account. `tgt` ships with none compiled in, so the first thing it asks for is yours.

That's a deliberate choice, not an oversight. A shared, hard-coded credential pair in an open-source binary gets scraped, abused, and eventually banned, taking every user of that build down with it. Yours is yours.

## Getting a pair

1. Open [my.telegram.org](https://my.telegram.org) and log in with the phone number on your Telegram account. Telegram sends the confirmation code to the app, not by SMS, so have Telegram open somewhere.
2. Click **API development tools**.
3. Fill in the short form. The app title and short name are yours to pick and nothing checks them against anything; "tgt" works fine. Platform: Desktop. The URL field can stay empty.
4. Submit, and the page shows you an **App api_id** (a number) and an **App api_hash** (a 32-character hex string).

Keep that page open, or copy both somewhere, because the hash is only shown on that page.

::: warning Treat the hash like a password
The `api_hash` identifies your developer account to Telegram. Don't commit it, don't paste it into an issue, and don't put it in a dotfiles repo that isn't private.
:::

## Giving them to tgt

Three routes, and they're checked in this order (later wins):

**The first-run wizard.** With no credentials configured, `tgt` opens a "Connect to Telegram" panel before anything else happens: two fields, API id and API hash. <kbd>Tab</kbd> or <kbd>Enter</kbd> moves from the id to the hash, <kbd>Enter</kbd> on the hash submits. If the id isn't a number or the hash is empty, the panel says so and stays put rather than failing later against Telegram's servers. On success it writes both into your config file and moves straight on to the sign-in screen.

**The config file.** `~/.config/telegram-tui/config.toml`:

```toml
[credentials]
api_id = 1234567
api_hash = "0123456789abcdef0123456789abcdef"
```

The generated default config doesn't include this section at all (it's only rendered once at least one of the two is set), so you'll be adding it by hand if you go this way.

**Environment variables.** `TELEGRAM_API_ID` and `TELEGRAM_API_HASH` override whatever's in the config file, for that run only, and are never written back:

```shell
TELEGRAM_API_ID=1234567 TELEGRAM_API_HASH=0123... tgt
```

A `TELEGRAM_API_ID` that doesn't parse as a 32-bit integer is ignored with a warning in the log, and the config file's value is kept.

## Next

With credentials in place, `tgt` moves to the sign-in screen. [First login](login.md) covers phone codes, QR, and the 2FA password prompt.
