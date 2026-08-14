# Development

## Local instance without hardware

On any Linux machine (or WSL under Windows, the environment this project
is developed in — WSL is only an environment detail, not a requirement: a
native Linux works identically). After `cargo build --workspace` (see
[installation.md](installation.md)), launch a local instance without any
Pi hardware:

    mkdir -p /tmp/rp
    cat > /tmp/rp/plugins.toml <<'PLUGINS'
    [[plugin]]
    name = "radio"
    kind = "source"
    exec = "target/debug/ritornello-plugin-radio"

    [[plugin]]
    name = "console"
    kind = "display"
    exec = "target/debug/ritornello-plugin-console"
    PLUGINS
    cat > /tmp/rp/stations.toml <<'STATIONS'
    [[stations]]
    name = "FIP"
    url = "http://icecast.radiofrance.fr/fip-midfi.mp3"
    preset = 1
    STATIONS
    RITORNELLO_PLUGINS=/tmp/rp/plugins.toml RITORNELLO_STATE=/tmp/rp/state.json \
    RITORNELLO_MPV_SOCKET=/tmp/rp/mpv.sock RITORNELLO_RUNTIME_DIR=/tmp/rp \
    RITORNELLO_HTTP=127.0.0.1:8080 \
    RITORNELLO_CONSOLE_TTY=/dev/stdout \
    RITORNELLO_RADIO_STATIONS=/tmp/rp/stations.toml RITORNELLO_RADIO_STATE=/tmp/rp/plugin-radio.json \
    RITORNELLO_LOCALES=deploy/locales \
    cargo run -p ritornello-core

`RITORNELLO_LOCALES` matters more than it looks: English is embedded in the
binary, every other language is read from disk at startup. Its default
(`/etc/ritornello/locales`) is a path `deploy.sh` installs and that a
development checkout does not have, so without the line above the language
dropdown offers **English only** — the French pack sits unread in
`deploy/locales/`. One setting is enough: plugins inherit the core's
environment and read the same variable.

The `generic-input` plugin can be added to the `plugins.toml` in
`/tmp/rp`:

    [[plugin]]
    name = "generic-input"
    kind = "input"
    exec = "target/debug/ritornello-plugin-generic-input"

with the following variables added to the environment line:

    RITORNELLO_INPUT_BINDINGS=/tmp/rp/input-bindings.toml RITORNELLO_INPUT_PRESETS=deploy/input-presets

The `metadata` plugins are added the same way (`kind = "metadata"`,
executables `ritornello-plugin-musicbrainz` and
`ritornello-plugin-ouifm-metas`).

## Tests

    cargo test --workspace                              # Rust suites
    cargo clippy --workspace --all-targets -- -D warnings
    npm test --workspaces                               # vitest (SPA, kit, plugin UIs)
    npm run typecheck                                   # vue-tsc
    npm run e2e -w app                                  # Playwright journeys

The project's testing style: pure functions tested against **real
captures** (mpv frames, radio-browser responses, OUI FM feeds), and
**discriminating** tests — several encode a regression that actually
happened, and say which one in a comment.

## E2e journeys (Playwright)

`npm run e2e -w app` needs a compiled core (`cargo build --workspace`)
and `mpv` on the machine running the journeys (real playback by the radio
plugin). Under Windows — the environment where npm/node/Playwright run in
this project —, the core binary is a Linux ELF compiled under WSL: the
harness (`web/app/e2e/serve.mjs`) therefore launches it through
`wsl.exe`, not directly, and the teardown (`web/app/e2e/teardown.mjs`)
must explicitly target the WSL-side process, a Windows `taskkill` only
killing the Windows process tree. Under native Linux, the same harness
launches the binary directly. The particulars (configuration vs runtime
directories, Unix sockets being impossible on the DrvFs mount) are
documented at the top of `serve.mjs`.

## Embedded data to regenerate

- **Theme presets** (42 tweakcn themes):
  `cd web/kit && node scripts/fetch-presets.mjs`.
- **OUI FM webradio table**:
  `node crates/ritornello-plugin-ouifm-metas/scripts/fetch-webradios.mjs`
  (re-reads the site's `apidata` variable; `--verifier` reports a drift
  without writing anything).

## Build guardrails

`web/app/scripts/verifier-dist.mjs` checks after every npm build that the
import map is correct and that the Vue runtime is unique; the equivalent
for plugin bundles is `verifier-dist-plugin.mjs`. The npm build must
**always** precede the cargo builds: the SPA and the plugins' `ui.js` are
embedded at compile time (`rust-embed`, `include_str!`). This is the
order `deploy/build.sh` applies. When cargo runs through WSL against a
Windows checkout, the `dist` fingerprint may not invalidate reliably —
`touch crates/ritornello-core/build.rs` after an npm rebuild to force
re-embedding the SPA.

## Process

The project is developed through specifications, implementation plans and
systematic reviews; these documents are archived (in French) in
[docs/superpowers/](superpowers/). The full review of 2026-07-27 (four
reviewers by area: protocol/SDK, core, plugins, web/deployment) produced
the `fix(core)`/`fix(sdk,i18n)`/`fix(plugins)`/`fix(web)`/`fix(deploy)`
series of fixes visible in the history. Debt identified and **accepted**
at this stage, in order of interest:

- no protocol version between core and plugins: a request unknown to an
  old binary is ignored on the plugin side and costs a 5 s timeout on the
  core side — acceptable as long as core and plugins are deployed
  together;
- the "two halves" bootstrap (source/input + admin) and the
  `build.rs`/placeholder pair are duplicated between radio and
  generic-input, as are the `env_or`/`log_half` helpers — to be hoisted
  into the SDK with the third UI-bearing plugin;
- `Enrichment` derives `Default`, which makes it possible to forget the
  identity echo (the enrichment is then simply discarded);
- the three copies of the `i18nKeysUsed` test and the admins' HTML tables
  would deserve a shared helper/component in the kit.
