# Changelog

## [0.2.1](https://github.com/SpechtLabs/telegram-tui/compare/v0.2.0...v0.2.1) (2026-08-02)


### Features

* **ui:** keyboard navigation that stops fighting the viewport ([#10](https://github.com/SpechtLabs/telegram-tui/issues/10)) ([731667f](https://github.com/SpechtLabs/telegram-tui/commit/731667fe7741aba5bcc13109ae0b5ab78fc2d22a))


### Bug Fixes

* **docs:** replace the hand-rolled cast player with the real one ([#77](https://github.com/SpechtLabs/telegram-tui/issues/77)) ([d55eda6](https://github.com/SpechtLabs/telegram-tui/commit/d55eda65ca181888c63480e893fbae16530d1171))

## [0.2.0](https://github.com/SpechtLabs/telegram-tui/compare/v0.1.7...v0.2.0) (2026-08-01)


### ⚠ BREAKING CHANGES

* the `[telemetry] mode` config key has been removed. A configuration file that still sets it will refuse to start, naming the file and the replacement. Replace `mode = "off"` with `enabled = false`, and `mode = "vendor"` or `mode = "custom"` with `enabled = true`. Crash reporting and a user's own OTLP collector are controlled separately by `crash_reports` and `endpoint`.

### Features

* **app:** add tgt --demo, an offline in-memory demo backend for recordings ([9462afa](https://github.com/SpechtLabs/telegram-tui/commit/9462afae27e9774487b3e866095d73a407e5c3ee))
* **app:** show the composer's Sending -&gt; Sent transition in the recording ([8af78ee](https://github.com/SpechtLabs/telegram-tui/commit/8af78eed883e9f62209c6390856872f6c50d97ed))
* **app:** switch tgt --demo to a scripted FakeTd fixture, add the recording ([2cef314](https://github.com/SpechtLabs/telegram-tui/commit/2cef314deda366e694b2e195489e98f8399753cb))
* remove the retired [telemetry] mode config key ([f49c0fa](https://github.com/SpechtLabs/telegram-tui/commit/f49c0fa7c9e17791a44fe6a642a6edc416d12682))


### Bug Fixes

* **docs:** make the deploy build actually run mise run docs-build ([c843336](https://github.com/SpechtLabs/telegram-tui/commit/c8433366904c6b49967090601801b6849996c0c1))
* **install:** replace the wrong libc++ warning with the real glibc floor ([d8a4646](https://github.com/SpechtLabs/telegram-tui/commit/d8a4646d8e0bb94f0017e14a02fa7cc92225a4a1))
* **release:** pass the release PR JSON as data, not as shell syntax ([88f28ae](https://github.com/SpechtLabs/telegram-tui/commit/88f28ae829ef6be9b5016dcaca545f079e7da1c9))

## [0.1.7](https://github.com/SpechtLabs/telegram-tui/compare/v0.1.6...v0.1.7) (2026-08-01)


### Bug Fixes

* **release:** stop a repair from downgrading Homebrew, guard against stale runs ([af28cb2](https://github.com/SpechtLabs/telegram-tui/commit/af28cb2e923e6c9aa5cd059703d6385d3ff6120c))

## [0.1.6](https://github.com/SpechtLabs/telegram-tui/compare/v0.1.5...v0.1.6) (2026-07-31)


### Features

* forget the signed-out account when the TDLib client restarts ([deb3292](https://github.com/SpechtLabs/telegram-tui/commit/deb3292f18426172b4c5f37bcb90abfcef2f1332))
* **update:** add --force, and refuse to walk a version backwards ([ccb5a6b](https://github.com/SpechtLabs/telegram-tui/commit/ccb5a6b4588db016ce37166711934a31f4ab6f2a))


### Bug Fixes

* **core:** surface failed logout and failed external-open as toasts ([5c24aff](https://github.com/SpechtLabs/telegram-tui/commit/5c24aff838128f51692f91769a3d66fa27767058))
* **release:** a repair completes a release instead of rebuilding it ([9542c26](https://github.com/SpechtLabs/telegram-tui/commit/9542c269580e4c207f1f044f1a3741e8a659e021))
* **update:** keep advice readable, and stop an update stealing the PATH link ([58cc556](https://github.com/SpechtLabs/telegram-tui/commit/58cc5569bde0d908892efa27f5d70415bad76572))
* **update:** resolve the real tree when tgt is run through a symlink ([9362054](https://github.com/SpechtLabs/telegram-tui/commit/9362054719a4eb15fe893c8b111bee6c5becdf7f))

## [0.1.5](https://github.com/SpechtLabs/telegram-tui/compare/v0.1.4...v0.1.5) (2026-07-31)


### Features

* curl | sh installer, and one install layout everywhere ([cd39d6b](https://github.com/SpechtLabs/telegram-tui/commit/cd39d6b33c09f66987e5288019e5c0ecd54d6e45))
* move the upload progress bar, and offer to cancel ([1d1250a](https://github.com/SpechtLabs/telegram-tui/commit/1d1250ae43ff5a2f48aa41ea59a2718dd5b38180))
* show real Telegram folder names in the sidebar tab strip ([80f4ca0](https://github.com/SpechtLabs/telegram-tui/commit/80f4ca0fe04306c92ad953afabfdc394c52f7bdd))
* sub-row hit targets for spoiler reveal and reply-quote jump ([33e3cb4](https://github.com/SpechtLabs/telegram-tui/commit/33e3cb42cecffed488cca3351f5073ef455ad376))


### Bug Fixes

* **core:** close the chat when the conversation stops being visible ([6a8761c](https://github.com/SpechtLabs/telegram-tui/commit/6a8761c1d67e25a1522f6707cfc4d442b92e46e7))
* **install:** never replace a directory that is not demonstrably a tgt tree ([6087f60](https://github.com/SpechtLabs/telegram-tui/commit/6087f60b1a0f6f2513e37f5bebc4aa162539256b))
* **release:** install the tarball Homebrew actually staged ([64a69c6](https://github.com/SpechtLabs/telegram-tui/commit/64a69c634960285a38b2934c78f90bc353fe905e))

## [0.1.4](https://github.com/SpechtLabs/telegram-tui/compare/v0.1.3...v0.1.4) (2026-07-31)


### Features

* **app:** abort with an actionable error when the config can't be written ([3bd8e1a](https://github.com/SpechtLabs/telegram-tui/commit/3bd8e1afa40d12deb26d841c80b93bd44196cf5f))
* **app:** replace a TDLib client that has closed, pre-authorization ([9587db8](https://github.com/SpechtLabs/telegram-tui/commit/9587db8c4116212db858c4ccbeade77517b27687))
* **app:** Sentry crash reporting by default, OTLP opt-in ([a388953](https://github.com/SpechtLabs/telegram-tui/commit/a3889530f01dbf0083e4c7eef325af8a476939b7))
* **auth:** default to QR sign-in with a phone-number escape hatch ([082ec9c](https://github.com/SpechtLabs/telegram-tui/commit/082ec9c9c4e8c34b55d2465e7bdfa920e318bab1))
* compile on Linux and Windows ([d63e913](https://github.com/SpechtLabs/telegram-tui/commit/d63e913dc0aad75ca1c9deb12096d17179c208ab))


### Bug Fixes

* **app:** a build with no Sentry DSN says so on the consent screen ([a9fad66](https://github.com/SpechtLabs/telegram-tui/commit/a9fad660911a384c8c34583a1608c42e40a27f6e))
* **app:** enable bracketed paste so multi-line pastes stay one event ([78a8268](https://github.com/SpechtLabs/telegram-tui/commit/78a82685beb48f721b4e16ed6fde05f179d7768e))
* **core:** mark messages read so the unread badge clears and syncs ([58e21b3](https://github.com/SpechtLabs/telegram-tui/commit/58e21b31ff8f4b5e0b5db986b154683ba7f50bf5))
* **core:** scroll the chat list viewport instead of moving the selection ([564e864](https://github.com/SpechtLabs/telegram-tui/commit/564e864051c218002a322567f7241cd9a9f9fe0a))
* **core:** surface failed message actions instead of dropping their completions ([6ae9948](https://github.com/SpechtLabs/telegram-tui/commit/6ae9948eb7abccb5f921a2d7f79579a03527d9bd))
* **core:** tell TDLib when a chat is no longer open ([b3c8434](https://github.com/SpechtLabs/telegram-tui/commit/b3c8434c92b02bd0bd32e10ac84d11f6cd9b0a84))
* **ui:** distinguish "still loading" from "genuinely empty" in the sidebar ([d004bf5](https://github.com/SpechtLabs/telegram-tui/commit/d004bf551ae1a504bdc90924531cde4703197943))
* **ui:** give the sidebar a vertical rhythm so folder tabs read as navigation ([272e780](https://github.com/SpechtLabs/telegram-tui/commit/272e780ee6a177cbcaca795a60e5ce0ecd3a4345))
* **ui:** make the chat list window follow scroll_offset instead of selection ([926bd4a](https://github.com/SpechtLabs/telegram-tui/commit/926bd4a0b4ef15e7c57b973b4f23014dd7a89b90))

## [0.1.3](https://github.com/SpechtLabs/telegram-tui/compare/v0.1.2...v0.1.3) (2026-07-31)


### Features

* auto-download photos for inline display ([088a945](https://github.com/SpechtLabs/telegram-tui/commit/088a94540a765b47d736dc796f65a9c1f77d457a))
* render inline images in supported terminals ([5cf85e5](https://github.com/SpechtLabs/telegram-tui/commit/5cf85e58582633eb1ab3ab80e795acfdc136b851))
* **ui:** built-in theme catalogue with runtime switching ([64e2030](https://github.com/SpechtLabs/telegram-tui/commit/64e2030afb6af7e08ec2d5382a71b46be84a69e5))
* **ui:** message rendering — inline receipts, single-line attachments, tertiary timestamps ([0819281](https://github.com/SpechtLabs/telegram-tui/commit/0819281b6718e80d6da42861bec0880fda6a5cd5))
* **ui:** modern chrome — borderless panes, padding, visual hierarchy ([c4dc36d](https://github.com/SpechtLabs/telegram-tui/commit/c4dc36d701d3cfab9c7f98ec9859ede6f41fe846))


### Bug Fixes

* **app:** seed file state from message payloads so downloads survive restart ([9acc9c6](https://github.com/SpechtLabs/telegram-tui/commit/9acc9c60180c8e9043efa68b26ed54c5e6c0915f))
* **app:** stop the draw gate from swallowing frames ([7a65b44](https://github.com/SpechtLabs/telegram-tui/commit/7a65b441a5cf08e2d676df0073d6023c9b093753))
* **core:** fill the viewport when a chat opens cold ([e8c7eeb](https://github.com/SpechtLabs/telegram-tui/commit/e8c7eebf6d357035079f77328b05fda514bff440))
* **ui:** erase inline images instead of only forgetting them ([2f92913](https://github.com/SpechtLabs/telegram-tui/commit/2f92913c30e2d5a8ee6c902a62bd5149b9e1dd7f))

## [0.1.2](https://github.com/SpechtLabs/telegram-tui/compare/v0.1.1...v0.1.2) (2026-07-31)


### Bug Fixes

* **ci:** guarantee rustfmt and clippy regardless of the mise cache ([b168bed](https://github.com/SpechtLabs/telegram-tui/commit/b168bed0cc13a65b77328bab8598481836d45c3e))
* **ci:** install rustfmt and clippy with the mise rust toolchain ([f1b1291](https://github.com/SpechtLabs/telegram-tui/commit/f1b1291876afac7a1a9f75204f7ef74c224614d0))
* **ci:** stop the release-please annotation leaking into the tarball name ([9651bd0](https://github.com/SpechtLabs/telegram-tui/commit/9651bd024933818a8f821c7a86b33f0aaac2ad22))

## [0.1.1](https://github.com/SpechtLabs/telegram-tui/compare/v0.1.0...v0.1.1) (2026-07-31)


### Features

* **app:** main loop, dispatcher, panic hook, file logging, CLI ([ead48dc](https://github.com/SpechtLabs/telegram-tui/commit/ead48dc16185024b33230bc8159c61f36a6c5d20))
* **app:** OTLP exporter with public-marker filter and bounded shutdown ([acfb3ac](https://github.com/SpechtLabs/telegram-tui/commit/acfb3ac21e275fc3a5f9bfb80e7f501bb4b4f5ed))
* **app:** tdlib linking and macOS rpath mechanism ([97d6527](https://github.com/SpechtLabs/telegram-tui/commit/97d65271ec1aee13b14793fa9b60906ebe465db1))
* **app:** TdlibRuntime with update pre-digestion and error mapping ([4ccd7af](https://github.com/SpechtLabs/telegram-tui/commit/4ccd7af1d6ab7d4fe2e4415aec3e490692be699c))
* **app:** telemetry show/reset-id and mode precedence ([43ad832](https://github.com/SpechtLabs/telegram-tui/commit/43ad83294ee26cae839bae223b07aa89ccb02b7d))
* **app:** TOML config with env overrides, Keychain-backed db key ([5bab6d5](https://github.com/SpechtLabs/telegram-tui/commit/5bab6d5137bd128971e4b9b2f8f90e8becd18023))
* **ci:** release-please pipeline, renovate, and mise-driven tasks ([92d7c5b](https://github.com/SpechtLabs/telegram-tui/commit/92d7c5b8bae0e5f7a96a79d9237f63434e56e373))
* **core:** Action/Effect enums, App root, focus stack, sub-state structs ([562c715](https://github.com/SpechtLabs/telegram-tui/commit/562c71556968efcff50308e13ad625aaeed40a86))
* **core:** auth state projection handlers ([2cb4bb3](https://github.com/SpechtLabs/telegram-tui/commit/2cb4bb3b3f976812837308a089b7e7454543ca59))
* **core:** chat list state mirroring TDLib order ([16f4013](https://github.com/SpechtLabs/telegram-tui/commit/16f401364df82c60a83dd0dd0b91bf92141bfcef))
* **core:** command palette with nucleo fuzzy matching ([2a2d2bd](https://github.com/SpechtLabs/telegram-tui/commit/2a2d2bdc6f9da5621ecd743ee7a204f9da115ab4))
* **core:** composer state with pending-send restore ([29008ea](https://github.com/SpechtLabs/telegram-tui/commit/29008eaa59fba98c04e04aefb35ed3f86529d45e))
* **core:** conversation window with anchor-stable paging and eviction ([fe6dd97](https://github.com/SpechtLabs/telegram-tui/commit/fe6dd97bdab877337d53d8a8a5bb8a93ffc55bc4))
* **core:** delete-confirmation modal handlers ([fc65927](https://github.com/SpechtLabs/telegram-tui/commit/fc65927487fc529cd4d604cc375f70ab2afed119))
* **core:** domain model types and TdError ([3b8f089](https://github.com/SpechtLabs/telegram-tui/commit/3b8f08918cdf4369fc41342eaf77bac3ee088b85))
* **core:** FakeTd JSONL fixture replay runtime ([a095906](https://github.com/SpechtLabs/telegram-tui/commit/a095906eaa22de3304315bfeaf8f9bea456bf26f))
* **core:** full key routing table with focus stack and telemetry emission ([85c5bfe](https://github.com/SpechtLabs/telegram-tui/commit/85c5bfe0ab16e5ab66bd7fde6eda69c4c433f81d))
* **core:** history paging state machine with empty-response retry ([557bcc9](https://github.com/SpechtLabs/telegram-tui/commit/557bcc985cfa61dae148541ffd83f43c6c8a0fd7))
* **core:** in-chat message search with hit stepping ([2bd1ee1](https://github.com/SpechtLabs/telegram-tui/commit/2bd1ee11b496387feb960fd665103a49a86081e6))
* **core:** local-first history on chat open with remote reconcile ([2bb4bd3](https://github.com/SpechtLabs/telegram-tui/commit/2bb4bd391577b4bdcd9c5cdd229ff047386470a7))
* **core:** M7 routing — palette, search, toasts, archive escape ([85211f0](https://github.com/SpechtLabs/telegram-tui/commit/85211f0c3666d4e3f35f29fdc9aa8f2e27aa59aa))
* **core:** media state, download priority tiers, M6 routing ([a0782ff](https://github.com/SpechtLabs/telegram-tui/commit/a0782ffe353b4347fb6ee9efce26e03ac21e0a33))
* **core:** presence, typing expiry, reaction updates with M5 routing ([eb1f487](https://github.com/SpechtLabs/telegram-tui/commit/eb1f4872488ff13c68cad8dc2198f77e8e35d881))
* **core:** selection mode, capability chips, GetMessageProperties contract ([1e07575](https://github.com/SpechtLabs/telegram-tui/commit/1e0757549c5d7fd81f4317079098457f6cdd9fd5))
* **core:** semantic mouse actions — click and scroll routing ([894e523](https://github.com/SpechtLabs/telegram-tui/commit/894e5236eeed69e50f7a8733b6d0bf15c7458d56))
* **core:** TDLib boundary types and TdRuntime trait ([317d65c](https://github.com/SpechtLabs/telegram-tui/commit/317d65c2f282a42b4cce834e099d77c4f2b9eb85))
* **core:** telemetry allowlist schema, emit! macro, id hashing ([9772f49](https://github.com/SpechtLabs/telegram-tui/commit/9772f49b3f84804e5ef3042db063bc703253fb73))
* **core:** wire '?' help overlay routing (gap found by T54) ([3270ec5](https://github.com/SpechtLabs/telegram-tui/commit/3270ec5c3c7f81f46ef9c145a2064e5443084f76))
* distributable packaging with rpath relocation proof ([475b84b](https://github.com/SpechtLabs/telegram-tui/commit/475b84bc7f11f4f192c0b57d26467f122a8b860c))
* file sending via /send, path-paste offer, MIME kind mapping ([3abdf67](https://github.com/SpechtLabs/telegram-tui/commit/3abdf67537b0cc0934c8396820f2a0d0e9be0450))
* inline image rendering with terminal graphics probe ([abb0d53](https://github.com/SpechtLabs/telegram-tui/commit/abb0d53035f05b81f7e4ac0105a270ef8491fa89))
* mouse support — hit-map translation, capture lifecycle, config toggle ([04ec707](https://github.com/SpechtLabs/telegram-tui/commit/04ec7070f61bd602c92bc6708a68fb82ba48f787))
* palette and search integration with overlay wiring ([4a6d3ec](https://github.com/SpechtLabs/telegram-tui/commit/4a6d3ec9772f8f18deeabd39d3c0f35a3a0168ef))
* scaffold workspace, toolchain pins, module tree, boundary check, CI ([86e8999](https://github.com/SpechtLabs/telegram-tui/commit/86e8999c5bfc01f9725629dcd00eb4828b36d469))
* sidebar organization — pinned, archive, folders ([fc26666](https://github.com/SpechtLabs/telegram-tui/commit/fc266667b298f5f8bb68f5300d990fa0651404aa))
* telemetry consent screen gating first run ([6a37250](https://github.com/SpechtLabs/telegram-tui/commit/6a3725072febeca6e94cb67baaf08e1a567a70d3))
* toast queue and PII-free terminal alerts ([c344489](https://github.com/SpechtLabs/telegram-tui/commit/c3444893e568557aa3dbfd5f3e82f10a13040eb9))
* **ui:** auth wizard views with QR half-block rendering ([8ae1f04](https://github.com/SpechtLabs/telegram-tui/commit/8ae1f04c44842275b76328d59223df138620757c))
* **ui:** centered command palette view with match highlighting ([2462521](https://github.com/SpechtLabs/telegram-tui/commit/246252142322656fb6a49b0ce036ec6b2e06f59c))
* **ui:** chat list sidebar with badges and selection ([f4e2ef6](https://github.com/SpechtLabs/telegram-tui/commit/f4e2ef6b2ea65574734962c3f4c535854333a134))
* **ui:** chip row, modal, context hint bar ([cedcbcd](https://github.com/SpechtLabs/telegram-tui/commit/cedcbcd3cb322052437434acc92a61438e684712))
* **ui:** composer input box with reply and edit banners ([2bbc9bb](https://github.com/SpechtLabs/telegram-tui/commit/2bbc9bb4fd3b6f8dcc84bb70159b77b15533328a))
* **ui:** conversation viewport with cached bottom-up layout ([b2c9cfc](https://github.com/SpechtLabs/telegram-tui/commit/b2c9cfcb123d11a76e7e0d0d2a14c7bd024e9b8a))
* **ui:** file cards with progress and open affordances ([b3fdf75](https://github.com/SpechtLabs/telegram-tui/commit/b3fdf75f551bd35d373a192d1f5b69b41557bc2b))
* **ui:** grapheme- and width-aware span wrapping ([c083d50](https://github.com/SpechtLabs/telegram-tui/commit/c083d50c92f6caa005d6d6872ecd33cc85de7689))
* **ui:** help overlay with per-context keymap ([54089ab](https://github.com/SpechtLabs/telegram-tui/commit/54089ab2732902dee6fb268c2cdfa7df20c3d3f5))
* **ui:** layout cache with line-count-bounded LRU; thread cache through view ([0c73ad4](https://github.com/SpechtLabs/telegram-tui/commit/0c73ad47e6a44ed9c73e2fff875b1e12621526c7))
* **ui:** message layout engine with accent rails and entity styling ([1c5dd2c](https://github.com/SpechtLabs/telegram-tui/commit/1c5dd2cb680f0f577c91c9179b1c5f83610d0f66))
* **ui:** reactions, receipts, typing and presence rendering ([7229689](https://github.com/SpechtLabs/telegram-tui/commit/7229689bd266276cd2d276f232c304d979577d2a))
* **ui:** responsive single-pane stack below breakpoint ([5ea96be](https://github.com/SpechtLabs/telegram-tui/commit/5ea96be6ad5e473b53c6a9d7c74d8729c16e95e1))
* **ui:** search hit highlighting and hit-count indicator ([06e4d59](https://github.com/SpechtLabs/telegram-tui/commit/06e4d592cef52d37016a8d09301ed4844bdf1d79))
* **ui:** theme file loading with 256-color degradation ([5139c70](https://github.com/SpechtLabs/telegram-tui/commit/5139c704c5bc4dc8889a477e44cf459d1b0674dc))
* **ui:** theme, input mapping, root two-pane shell, hint bar, header ([b4e096f](https://github.com/SpechtLabs/telegram-tui/commit/b4e096f7eba720b476e71391ab6b7ad3fd24140d))
* **ui:** UTF-16 code-unit span to byte range conversion ([60e59e0](https://github.com/SpechtLabs/telegram-tui/commit/60e59e0160fd58f0531bf835b9977c02c5cf04c8))
* wire auth flow end to end with FakeTd integration tests ([160cb1d](https://github.com/SpechtLabs/telegram-tui/commit/160cb1d5a6681c89b4da16dc07c59ba336871d50))
* wire interaction flows end to end with send/delete integration tests ([2421dc2](https://github.com/SpechtLabs/telegram-tui/commit/2421dc2126fdb07d2cb125208b16361d4f225e72))
* wire media flows end to end with download and send-file integration tests ([888c64e](https://github.com/SpechtLabs/telegram-tui/commit/888c64efbdac261455242f4e966aca821f16e110))
* wire read-only client end to end with paging retry integration test ([8468084](https://github.com/SpechtLabs/telegram-tui/commit/84680842d4863400f7c59969037b6700b1af3fa3))


### Bug Fixes

* **app:** allow dead_code on Core::app, live only via test #[path] include ([f17a5f2](https://github.com/SpechtLabs/telegram-tui/commit/f17a5f29d39ed9ac266684d529908be6e9e5459a))
* **app:** telemetry show exits cleanly when its pipe closes early ([0ec36d1](https://github.com/SpechtLabs/telegram-tui/commit/0ec36d17d116febab383f894d88af09d68491c87))
* **ui:** credentials wizard panel height — API id input was collapsed to its title line ([7276630](https://github.com/SpechtLabs/telegram-tui/commit/72766305a47c88679b91b35231ed19cd2167c8fa))
