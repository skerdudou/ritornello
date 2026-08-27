# Development

## Local instance without hardware

On any Linux machine (or WSL under Windows, the environment this project
is developed in — WSL is only an environment detail, not a requirement: a
native Linux works identically). After `npm run build --workspaces` then
`cargo build --workspace` (see [installation.md](installation.md)), the
whole device runs from the checkout — core **and every plugin** — without
a Pi and without installing anything under `/etc`.

### 1. Configuration files, once

    mkdir -p /tmp/rp/playlists /tmp/rp/credentials

    # The plugin list. Only `name` and `exec` are ever needed: each binary
    # announces its own kinds (source, metadata, input, display) and whether
    # it serves an admin page, when it registers with the core. A third key,
    # `enabled = false`, appears when a plugin is switched off from the
    # configuration page; its absence means active, and the core rewrites
    # this file — comments included — when the toggle is used.
    cat > /tmp/rp/plugins.toml <<'PLUGINS'
    [[plugin]]
    name = "radio"
    exec = "target/debug/ritornello-plugin-radio"

    [[plugin]]
    name = "cd"
    exec = "target/debug/ritornello-plugin-cd"

    [[plugin]]
    name = "files"
    exec = "target/debug/ritornello-plugin-files"

    # DECLARATION ORDER MATTERS between `metadata` plugins: for a given
    # track, the first one declared here that answers wins. Same order as
    # deploy/plugins.example.toml, so development shows what the device shows.
    [[plugin]]
    name = "ouifm-metas"
    exec = "target/debug/ritornello-plugin-ouifm-metas"

    [[plugin]]
    name = "radiofrance-metas"
    exec = "target/debug/ritornello-plugin-radiofrance-metas"

    [[plugin]]
    name = "musicbrainz"
    exec = "target/debug/ritornello-plugin-musicbrainz"

    [[plugin]]
    name = "generic-input"
    exec = "target/debug/ritornello-plugin-generic-input"

    [[plugin]]
    name = "console"
    exec = "target/debug/ritornello-plugin-console"
    PLUGINS

    # A station, to have something to play. The radio page writes this file
    # afterwards; two lines are enough to start.
    cat > /tmp/rp/stations.toml <<'STATIONS'
    [[stations]]
    name = "FIP"
    url = "http://icecast.radiofrance.fr/fip-midfi.mp3"
    preset = 1
    STATIONS

Nothing else has to exist. The `files` roots (`/tmp/rp/media-roots.toml`),
the remote-control bindings (`/tmp/rp/input-bindings.toml`) and the two
optional metadata override tables are all written by their own page; a
missing file is the normal case — at most a `WARN` naming the page to use,
never a failure to start. To start from a local folder without going
through the page:

    cat > /tmp/rp/media-roots.toml <<'ROOTS'
    [[root]]
    name = "usb"
    kind = "local"
    path = "/home/me/Music"
    ROOTS

### 2. The launch line, every plugin included

Plugins inherit the core's environment: everything below is set once, on
the single `cargo run` line, whichever binary ends up reading it.

    RITORNELLO_PLUGINS=/tmp/rp/plugins.toml RITORNELLO_STATE=/tmp/rp/state.json \
    RITORNELLO_MPV_SOCKET=/tmp/rp/mpv.sock RITORNELLO_RUNTIME_DIR=/tmp/rp \
    RITORNELLO_HTTP=127.0.0.1:8080 \
    RITORNELLO_LOCALES=deploy/locales \
    RITORNELLO_CONSOLE_TTY=/dev/stdout \
    RITORNELLO_RADIO_STATIONS=/tmp/rp/stations.toml RITORNELLO_RADIO_STATE=/tmp/rp/plugin-radio.json \
    RITORNELLO_FILES_ROOTS=/tmp/rp/media-roots.toml \
    RITORNELLO_FILES_CREDENTIALS=/tmp/rp/credentials \
    RITORNELLO_FILES_STATE=/tmp/rp/plugin-files.json \
    RITORNELLO_FILES_MPV_PLAYLIST=/tmp/rp/plugin-files.m3u \
    RITORNELLO_FILES_PLAYLISTS=/tmp/rp/playlists \
    RITORNELLO_INPUT_BINDINGS=/tmp/rp/input-bindings.toml RITORNELLO_INPUT_PRESETS=deploy/input-presets \
    RITORNELLO_OUIFM_METAS=/tmp/rp/ouifm-metas.toml \
    RITORNELLO_RADIOFRANCE_METAS=/tmp/rp/radiofrance-metas.toml \
    cargo run -p ritornello-core

Then <http://127.0.0.1:8080>. The `musicbrainz` plugin needs no variable at
all, and the two `*_METAS` lines are optional: those tables are embedded in
their binaries, the file only ever overrides an entry gone stale. Every
other line has the same job — pointing a default that lives under `/etc` or
`/var/lib` at `/tmp/rp`, so that a checkout writes nowhere it has no right
to write. Two of them are the exception, pointing into the checkout rather
than at `/tmp`: `RITORNELLO_INPUT_PRESETS` and `RITORNELLO_LOCALES` name
data the repository ships and `deploy.sh` installs.

Every variable, and who reads it — each default is a production path, which
is exactly why they have to be overridden in a checkout:

| Variable | Read by | Default |
|---|---|---|
| `RITORNELLO_PLUGINS` | core | `/etc/ritornello/plugins.toml` |
| `RITORNELLO_STATE` | core | `/var/lib/ritornello/state.json` |
| `RITORNELLO_HTTP` | core | `0.0.0.0:8080` |
| `RITORNELLO_MPV_SOCKET` | core | `/run/ritornello/mpv.sock` |
| `RITORNELLO_MPV_BIN` | core | `mpv` |
| `RITORNELLO_RUNTIME_DIR` | core, `files` | `/run/ritornello` |
| `RITORNELLO_LOCALES` | core and every plugin | `/etc/ritornello/locales` |
| `RITORNELLO_LOCALE` | plugins | set by the core when it launches them |
| `RITORNELLO_AUDIO_BUFFER`, `RITORNELLO_NETWORK_READAHEAD` | core (mpv tuning) | built-in durations |
| `RITORNELLO_CD_DEV` | core (mpv) **and** `cd` | `/dev/sr0` |
| `RITORNELLO_CONSOLE_TTY` | `console` | `/dev/tty1` |
| `RITORNELLO_RADIO_STATIONS`, `RITORNELLO_RADIO_STATE` | `radio` | `/etc/…/stations.toml`, `/var/lib/…/plugin-radio.json` |
| `RITORNELLO_RADIO_DIRECTORY` | `radio` | the radio-browser mirrors, tried in order |
| `RITORNELLO_FILES_ROOTS`, `_CREDENTIALS`, `_STATE`, `_MPV_PLAYLIST`, `_PLAYLISTS` | `files` | `/etc/ritornello/…`, `/var/lib/ritornello/…` |
| `RITORNELLO_FILES_PROC_MOUNTS` | `files` | `/proc/mounts` (overridden by its tests only) |
| `RITORNELLO_USER` | `files` (owner of the mounts) | `ritornello` |
| `RITORNELLO_OUIFM_METAS`, `RITORNELLO_RADIOFRANCE_METAS` | the two metadata plugins | `/etc/ritornello/…` — optional file, the tables are embedded |

### 3. What a machine without the hardware will not do

All the plugins start; three of them simply have nothing to talk to, and
say so rather than failing:

- **`cd`** finds no drive and stays on "no disc" (a real drive also needs
  the `cd-discid` binary to read a TOC);
- **`generic-input`** logs `bindings … unreadable … use the admin page`
  then `0 input device(s) opened` where there is no `/dev/input` — the
  usual case under WSL. Both are `WARN`, and its page still works, so
  bindings can be edited without a remote;
- **`files`** mounts nothing: mounting is done by a root helper through
  `ritornello-media-mount.service`, which a checkout does not have. Local
  roots work, SMB shares do not.

`RITORNELLO_LOCALES` matters more than it looks: English is embedded in the
binary, every other language is read from disk at startup. Its default
(`/etc/ritornello/locales`) is a path `deploy.sh` installs and that a
development checkout does not have, so without the line above the language
dropdown offers **English only** — the French pack sits unread in
`deploy/locales/`. One setting is enough: plugins inherit the core's
environment and read the same variable.

## Language

Three audiences, three rules — the boundary is the audience, not the file:

- **Code comments, commit messages, and the specs and plans under
  `docs/superpowers/` are French.** Public `///` doc comments follow the
  file they live in: files documenting an API surface (`state.rs`,
  `core.rs`) are English throughout and keep internal `//` comments French.
- **Logs are English**, at every level, including the `anyhow!` and
  `.context(…)` strings they interpolate. They are read next to
  `journalctl` and rustc — and they are visible in the UI: the System
  tab's "Recent errors" card serves them verbatim (`GET /api/logs` returns
  the buffer's last 500 WARN/ERROR lines, of which the card shows 8 and a
  dialog the rest), so a French log line would show up untranslated in an
  English interface.
- **Everything a user reads goes through the i18n catalogues**, never a
  hard-coded string: the display, the SPA, and the `error` field of a `422`
  (the kit turns it straight into a toast). English lives in the binary,
  other languages in `deploy/locales/` — see [interface.md](interface.md).
  Validation stays pure and catalogue-free (`validate_settings`,
  `validate_audio_device`, `theme::validate`, `system::parse_action` all
  return a typed error); the HTTP route is what resolves it against the
  core's current catalogue. The radio plugin's `config.rs` shows the pattern
  every one of them follows — a typed `ValidationError` with a
  `message(&Catalog)` and an English `Display` for logs. The same split
  applies to a save that fails on disk: the plugin admin backends turn the
  I/O failure into a catalogue phrase for the reader and log the raw detail,
  never the other way around.

## Tests

    cargo test --workspace                              # Rust suites
    cargo clippy --workspace --all-targets -- -D warnings
    npm test --workspaces                               # vitest (SPA, kit, plugin UIs)
    npm run typecheck                                   # vue-tsc
    npm run e2e -w app                                  # Playwright journeys

### Continuous integration

`.github/workflows/ci.yml` runs those five commands on every push and pull
request, on Ubuntu, in four jobs:

- `web` — `npm ci`, build of the six workspaces, `vue-tsc`, vitest; it
  publishes the `dist/` directories as an artifact, because they are
  git-ignored and the Rust jobs need them;
- `rust` — downloads the dist, **refuses to go on if one is missing**
  (`build.rs` would otherwise embed a placeholder UI and only warn), then
  `cargo build`, `clippy -D warnings`, `cargo test`; `ffmpeg` is installed
  so the duration tests do not skip themselves;
- `e2e` — same dist, debug build of the core, `mpv` installed (the
  journeys really play), Playwright on chromium; the report is uploaded on
  failure;
- `release` — on a `v*` tag only: `cross build --release` for
  `armv7-unknown-linux-gnueabihf`, and the binaries `deploy.sh` expects as
  an artifact.

Ubuntu and not Windows because the SDK tests open Unix sockets.
`scripts/ci-local.sh [web|rust|e2e]` runs the same commands in the same
order from WSL — if one of the two changes, the other must follow. A known
flaky class (a test that assumes fast execution; the plan
`docs/superpowers/plans/2026-08-26-ci-github-actions.md` lists the known
cases) is fixed at the source when it shows up, never retried blindly.

The project's testing style: pure functions tested against **real
captures** (mpv frames, radio-browser responses, OUI FM feeds, Radio
France live answers), and
**discriminating** tests — several encode a regression that actually
happened, and say which one in a comment.

## E2e journeys (Playwright)

`npm run e2e -w app` needs a compiled core, built in the order
`npm run build --workspaces` **then** `cargo build --workspace`: the core
embeds the SPA's `dist/` at compile time (see "Build guardrails" below),
so a stale `dist` produces a core that serves a stale UI to the e2e
journeys even though the source changed. It also needs `mpv` on the
machine running the journeys (real playback by the radio plugin). Under
Windows — the environment where npm/node/Playwright run in
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
- **Radio France station table**:
  `node crates/ritornello-plugin-radiofrance-metas/scripts/fetch-stations.mjs`
  (re-reads the Open API documentation and the site's webradio cards, and
  re-checks every mount not covered by the documentation; `--verifier`
  reports a drift without writing anything).
- **Screenshots** (`docs/captures/*.png`): with a core running (`node
  e2e/serve.mjs` from `web/app`), `node scripts/captures.mjs` from `web/app`;
  then stop the core with the e2e teardown. As with the e2e journeys
  above, run `npm run build --workspaces` then `cargo build --workspace`
  first — the core the script screenshots is the one just built.

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
