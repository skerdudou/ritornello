# Development

## Local instance without hardware

On any Linux machine (or WSL under Windows, the environment this project
is developed in — WSL is only an environment detail, not a requirement: a
native Linux works identically). After `npm run build --workspaces` then
`cargo build --workspace` (see [installation.md](installation.md)), the
whole device runs from the checkout — core **and every plugin** — without
a Pi and without installing anything under `/etc`.

### 1. Configuration files, once

Every plugin keeps its data — settings, state, caches — in its own
directory under one root (`RITORNELLO_PLUGIN_DATA_ROOT`; see [Where a
plugin keeps its data](plugins.md#where-a-plugin-keeps-its-data)). The core
creates each plugin's directory itself on launch, so only the one below —
needed ahead of the first `cargo run`, to have a station to play — has to
exist beforehand:

    mkdir -p /tmp/rp/plugins/radio

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
    name = "nrj-metas"
    exec = "target/debug/ritornello-plugin-nrj-metas"

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
    cat > /tmp/rp/plugins/radio/stations.toml <<'STATIONS'
    [[stations]]
    name = "FIP"
    url = "http://icecast.radiofrance.fr/fip-midfi.mp3"
    preset = 1
    STATIONS

Nothing else has to exist. The `files` roots
(`/tmp/rp/plugins/files/media-roots.toml`), the remote-control bindings
(`/tmp/rp/plugins/generic-input/input-bindings.toml`) and the two optional
metadata override tables are all written by their own page (each into its
own plugin's directory, created by the core the first time that plugin
launches); a missing file is the normal case — at most a `WARN` naming the
page to use, never a failure to start. To start from a local folder without
going through the page:

    mkdir -p /tmp/rp/plugins/files
    cat > /tmp/rp/plugins/files/media-roots.toml <<'ROOTS'
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
    RITORNELLO_CONSOLE_TTY=/dev/stdout \
    RITORNELLO_PLUGIN_DATA_ROOT=/tmp/rp/plugins \
    RITORNELLO_INPUT_PRESETS=deploy/input-presets \
    cargo run -p ritornello-core

Then <http://127.0.0.1:8080>. A single variable now stands for every
plugin's own settings, state and caches: `RITORNELLO_PLUGIN_DATA_ROOT`
points a default that lives under `/var/lib/ritornello` at `/tmp/rp`, so
that a checkout writes nowhere it has no right to write — the core joins it
with each plugin's bare name and creates the result before launching that
plugin (`RITORNELLO_PLUGIN_DATA_DIR`, below). `RITORNELLO_INPUT_PRESETS` is
the one exception, pointing into the checkout rather than at `/tmp`: it
names data the repository ships and `deploy.sh` installs, not something a
plugin writes.

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
| `RITORNELLO_AUDIO_BUFFER`, `RITORNELLO_NETWORK_READAHEAD` | core (mpv tuning) | built-in durations |
| `RITORNELLO_CD_DEV` | core (mpv) **and** `cd` | `/dev/sr0` |
| `RITORNELLO_CONSOLE_TTY` | `console` | `/dev/tty1` |
| `RITORNELLO_PLUGIN_DATA_ROOT` | core | `/var/lib/ritornello/plugins` |
| `RITORNELLO_PLUGIN_DATA_DIR` | every plugin (set by the core, not by hand) | when unset — a plugin launched by hand, outside the core — the SDK's own `default_data_dir(name)`, `/var/lib/ritornello/plugins/<name>` **whatever `RITORNELLO_PLUGIN_DATA_ROOT` is set to**: a plugin binary reads this variable, never that one, so a checkout's root override never reaches a hand-run plugin |
| `RITORNELLO_RADIO_DIRECTORY` | `radio` | the radio-browser mirrors, tried in order |
| `RITORNELLO_FILES_PROC_MOUNTS` | `files` | `/proc/mounts` (overridden by its tests only) |
| `RITORNELLO_USER` | `files` (owner of the mounts) | `ritornello` |

### 3. What a machine without the hardware will not do

All the plugins start; three of them simply have nothing to talk to, and
say so rather than failing:

- **`cd`** finds no drive and stays on "no disc" (a real drive also needs
  the `cd-discid` binary to read a TOC);
- **`generic-input`** logs `bindings … unreadable … use the admin page`
  then `0 input device(s) opened` where there is no `/dev/input` — the
  usual case under WSL. Both are `WARN`, and its page still works, so
  bindings can be edited without a remote;
- **`files`** mounts nothing: mounting is done by a root helper (the `ritornello-files-mount` crate, not part of the plugin) through
  `ritornello-media-mount.service`, which a checkout does not have. Local
  roots work, SMB shares do not.

A local run shows **English only**, on purpose: English is embedded in the
binary, every other language arrives as an installed language pack, under
the directory `RITORNELLO_LANGUAGE_PACKS` names (default
`/etc/ritornello/language-packs`), and a checkout has none installed there.

## Language

Three audiences, three rules — the boundary is the audience, not the file:

- **Code, comments, test names and commit messages are English.** The
  identifiers, `///` doc comments and internal `//` comments all use the
  same vocabulary as the wire contract (`cover`, `preset`, `plugin`,
  `settings`, `state`); French appears only in test fixtures that replay
  real French data (station names, Radio France payloads).
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
  every one of them follows — a typed `ValidationError` with a `text()`
  returning an unresolved `Text` (a key and its parameters, resolved by the
  core against this plugin's announced catalog) and an English `Display` for
  logs. The same split applies to a save that fails on disk: the plugin
  admin backends turn the I/O failure into a catalogue phrase for the
  reader and log the raw detail, never the other way around.

## Layout of the core crate: `core/` and `status/`

`crates/ritornello-core/src/core/` is one struct, `Core<P>`, split by domain
— one file per domain, each holding a partial `impl<P: Player> Core<P>`:

| File | Owns |
|---|---|
| `mod.rs` | the struct, `new`, and `handle_source_update` — the entry point of a Source frame, which writes into every domain and stays here on purpose |
| `commands.rs` | remote and UI commands: play/standby machine, volume, tens offset, seek, held keys, startup |
| `deadlines.rs` | overlays and the deadlines the `main.rs` loop wakes on (`next_deadline`) |
| `playback.rs` | mpv events, retry with growing backoff, resume on wake |
| `track_metadata.rs` | identity, ICY titles, file tags, plugin enrichments, covers and their extraction |
| `position.rs` | the progression mpv reports and the anchor a plugin sets |
| `publish.rs` | player state and source catalogue pushed to displays, SPA and metadata plugins |
| `settings.rs` | audio output, locale, theme, and writing `state.json` |
| `sources.rs` | cycle order, switching, hot-plug and death of a plugin, applying a `SourceAction` |
| `test_support.rs` | fake player and sources, shared test rigs (`pub(super)`, test-only) |

`status/` follows the same shape for the HTTP surface: `mod.rs` keeps
`AppState`, the router, audio output, settings GET/PUT and the command route;
`settings_validation.rs` the setting ranges and `SettingsError`;
`logs.rs` the log buffer, `/api/logs` and the `/api/player` SSE stream;
`plugin_status.rs` `PluginStatus`, the plugin order, the enable switch and what a
disconnect or a re-announce changes; `locales.rs` the locale and i18n routes.
`mod.rs` re-exports what other files import, so `main.rs`, `admin.rs` and
`system.rs` never name a child module.

**The rule that made the split free:** a child module sees the private
fields of a struct its parent defines. So `publie_etat` still reads its
twenty-odd fields directly, `persist` still walks everything it writes, and
not one field became `pub`, not one accessor was added. Adding a domain means
adding a file with its own `impl<P: Player> Core<P>` — never widening a
field's visibility. A method the parent or a sibling calls is `pub(super)`;
that is the only visibility the split introduced.

## Tests

    cargo test --workspace                              # Rust suites
    cargo clippy --workspace --all-targets -- -D warnings
    npm test --workspaces                               # vitest (SPA, kit, plugin UIs)
    npm run typecheck                                   # vue-tsc
    npm run e2e -w app                                  # Playwright journeys

### The wire fingerprint

`crates/ritornello-proto/tests/wire_fingerprint.rs` serializes a sample of
every message that crosses the core/plugin wire and compares it with
`tests/wire-fingerprint.txt`. The fixture has one section per wire contract,
headed with that contract's version (`[display 1.0]`), plus an
`[announcement protocol=2]` section for the announcement and the bootstrap
number. When it fails, the wire changed or a version moved without the
fixture: decide whether an old plugin can still understand the new shape. If
not, it is a break, so bump that contract's **major**; if it is compatible
(an added optional field, an added variant nobody old receives), bump its
**minor**. Either way the version goes strictly up, in
`crates/ritornello-proto/src/contract.rs`, and the fixture is regenerated:

    UPDATE_WIRE_FINGERPRINT=1 cargo test -p ritornello-proto --test wire_fingerprint

Regeneration is not a way round the rule: a contract's section cannot change
unless its version went strictly up, and the test refuses otherwise, even
under `UPDATE_WIRE_FINGERPRINT=1`. A version never goes down: to revert a
bump, restore the fixture from git. A missing fixture is a panic. A
brand-new contract is introduced by hand-adding its `[<name> 0.0]` header to
the fixture, then bumping it. The announcement section regenerates freely for
additions; a *break* of the announcement moves `PROTOCOL_VERSION` (and
republishes everything). Read the diff of the fixture: it is the exact record
of what moved.

### Continuous integration

`.github/workflows/ci.yml` runs those five commands on every push and pull
request — the last three jobs below only on a tag:

- `web-build` ("Web UI (build)") — `npm ci`, build of the npm workspaces
  (the SPA, the kit, and one per plugin UI — a count deliberately not
  written here, it drifts every time a plugin gains a page); it
  publishes the `dist/` directories as an artifact, because they are
  git-ignored and the Rust jobs need them;
- `web-test` ("Web UI (typecheck, vitest)") — `npm ci`, `vue-tsc`, vitest,
  in parallel with `rust` and `e2e` rather than ahead of them: they read
  the built dist, never a test result;
- `rust` — downloads the dist, **refuses to go on if one is missing**
  (`build.rs` would otherwise embed a placeholder UI and only warn), then
  `cargo build`, `clippy -D warnings`, `cargo test`; `ffmpeg` is installed
  so the duration tests do not skip themselves;
- `e2e` — same dist, debug build of the core, `mpv` installed (the
  journeys really play), Playwright on chromium; the report is uploaded on
  failure;
- `installer` — on every event, once per workstation system (Linux musl
  x86_64 and aarch64, Windows, macOS Apple Silicon, and macOS Intel built
  only): clippy, the tests of `ritornello-install` alone, a release build,
  and its archive uploaded as an artifact; `installer-ok` ("Installer (all
  targets)") is the single check that stands for the five, meant to be
  required by branch protection;
- `publish-installer` — on an `installer-vX.Y.Z` tag only (the installer's own
  number, not the product's): refuses a tag that disagrees with
  `crates/ritornello-install/Cargo.toml`, then publishes the numbered release
  and moves the fixed release `installer` (see
  [Publishing the installer](installation.md#publishing-the-installer));
- `release` — on a `v*` tag only, once per architecture (`armv7`,
  `arm64`, `x86_64`), as soon as the web is built and while the tests
  still run: it refuses a tag that is not `v` + the product
  number, then `cross build --release --workspace` and
  `scripts/package-release.sh`, which produces that architecture's
  archives;
- `language-packs` — on a tag: the language pack archives, built once
  outside the per-architecture matrix since a pack has no architecture;
- `publish` — also on a tag, and the one job that waits for every test
  (`rust`, `e2e`, `web-test`), so archives built from a red tree are never
  drafted: keeps only the components whose own version
  moved since the last **finished** release, checks the notes, generates
  `catalogue.json` (the kind and description of every installable
  component, read by the "Add a plugin" dialog) and `inventory.json` (for
  `ritornello-install`), writes one `SHA256SUMS` for every asset including
  it — the installer archives are no longer part of a product release — and
  creates the release as
  a **draft**. A tag carrying a prerelease suffix (`v0.2.1-beta.1`) makes it
  a prerelease.

Everything runs on Ubuntu, since the SDK tests open Unix sockets, except
the `installer` job, which builds and tests that one crate on the systems a
person runs it from.
`scripts/ci-local.sh [web|rust|installer|e2e]` runs the same commands in the same
order from WSL — if one of the two changes, the other must follow. A known
flaky class (a test that assumes fast execution) is fixed at the source
when it shows up, never retried blindly.

The project's testing style: pure functions tested against **real
captures** (mpv frames, radio-browser responses, OUI FM feeds, Radio
France live answers), and
**discriminating** tests — several encode a regression that actually
happened, and say which one in a comment.

### Measurement benches

Two tests in `cover.rs` are `#[ignore]`d and driven by `COVER_CORPUS`, a directory of
real album covers:

    COVER_CORPUS=/path/to/covers cargo test --offline --release -p ritornello-core \
        -- --ignored --nocapture cover::tests

`the_weight_rule_of_a_thumbnail` measures what the encoder produces at several
(edge, quality) pairs. `where_the_passthrough_threshold_belongs` measures the other
population — images already smaller than the edge — and compares what they weigh against
what re-encoding them would produce.

They are not assertions, and they are not dead code: the configuration page shows the
user a predicted thumbnail weight, and `cover_passthrough_max_ko`'s default is derived
from these figures. A number put in front of the user whose measurement can no longer be
reproduced is an invention. Run them in `--release`: sizes are identical in debug, but
the image decoder is an order of magnitude slower.

The corpus is deliberately not shipped — it is someone's music library.

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
- **NRJ group station table** (365 stations across four brand sites):
  `node crates/ritornello-plugin-nrj-metas/scripts/fetch-stations.mjs`
  (re-reads each brand's own `/onair.json`; `--verifier` reports a drift
  without writing anything).
- **Screenshots** (`docs/captures/*.png`): with a core running (`node
  e2e/serve.mjs` from `web/app`), `node scripts/captures.mjs` from `web/app`;
  then stop the core with the e2e teardown. As with the e2e journeys
  above, run `npm run build --workspaces` then `cargo build --workspace`
  first — the core the script screenshots is the one just built.

  The script takes an optional list of shot names — `node
  scripts/captures.mjs system config-update` — and does all of them when
  given none. Worth knowing, because the two home shots depend on what the
  station happens to be playing at that moment: re-running everything to
  redo one of them can lose a good one, and there is no getting it back.

  The showcase pictures are richer than what `e2e/serve.mjs` alone gives
  (one station, no metadata plugin, hence a grey note and "Presets: 1").
  They were taken with a throwaway copy of that file — never committed,
  since the journeys need its minimal configuration — declaring
  `radiofrance-metas`, `musicbrainz` and `mpd` (on port 16600, so a real
  6600 is never touched) and six Radio France stations as presets. Radio
  France is what makes the shot speak: FIP broadcasts no ICY, so title,
  artist, year and cover all come from that plugin, and the provenance
  `(?)` then has something to show.

## Build guardrails

`web/app/scripts/check-dist.mjs` checks after every npm build that the
import map is correct and that the Vue runtime is unique; the equivalent
for plugin bundles is `check-plugin-dist.mjs`. The npm build must
**always** precede the cargo builds: the SPA and the plugins' `ui.js` are
embedded at compile time (`rust-embed`, `include_str!`). This is the
order `deploy/build.sh` applies. When cargo runs through WSL against a
Windows checkout, the `dist` fingerprint may not invalidate reliably —
`touch crates/ritornello-core/build.rs` after an npm rebuild to force
re-embedding the SPA.

## Cutting a release

The gesture itself lives in
[installation.md](installation.md#installing-from-a-release), with the
prerelease variant just below it — it is written there because that is
where the four numbers and the archive layout are explained, and splitting
them would give the same rule two homes. What a developer needs to know
before opening it:

- you bump versions **by hand**, once per component and per delivery, not
  once per commit: the release compares each component's declared version
  against the one it had at the release the tag is measured against (the
  last finished release for a finished tag, the previous published one for
  a prerelease), so fifteen commits to a plugin are one bump;
- you bump the product number too, and the tag must equal it exactly;
- a compatible change to a shared crate (`ritornello-proto`,
  `ritornello-i18n`, `ritornello-plugin-sdk`, `ritornello-updater`)
  republishes nothing: bump the plugins it must reach by hand. A change to
  the bootstrap `PROTOCOL_VERSION` or to the product's major republishes
  everything, and the script refuses a bootstrap move that left a core or
  plugin on its old number. A contract's major moved obliges the core and
  every plugin that speaks it to have moved (declared in
  `[package.metadata.ritornello]`), and the script names those that did not;
  a contract's minor requires nothing;
- the release lands as a draft, and a tag with a prerelease suffix lands
  as a prerelease. **A draft is invisible to every device** — GitHub lists
  drafts to a reader with push access alone, and the core polls with no
  token — so publishing it by hand is part of the gesture, not an
  afterthought. Skip it and every device reports "no release published
  yet", which is the truth from where it stands;
- a device whose "Offer prereleases" switch is off reads a repository
  holding only a beta as "only prereleases published" — its own sentence,
  naming the switch, and not the same one as "nothing published".

## Process

The project is developed through specifications, implementation plans and
systematic reviews; those working documents are kept outside the
repository. The full review of 2026-07-27 (four
reviewers by area: protocol/SDK, core, plugins, web/deployment) produced
the `fix(core)`/`fix(sdk,i18n)`/`fix(plugins)`/`fix(web)`/`fix(deploy)`
series of fixes visible in the history. Debt identified and **accepted**
at this stage, in order of interest:

- ~~no protocol version between core and plugins~~ — **settled since**:
  each wire contract carries its own `major.minor` version
  (`ritornello_proto::contract`), announced by the plugin, and the core
  refuses a plugin whose major differs and flags one whose minor is newer as
  limited. The refusal is proven by tests that fabricate a mismatched
  announcement, never by an actually incompatible binary: no contract major
  has moved in this project's history;
- the "two halves" bootstrap (source/input + admin) and the
  `build.rs`/placeholder pair are duplicated between radio and
  generic-input, as are the `env_or`/`log_half` helpers — to be hoisted
  into the SDK with the third UI-bearing plugin;
- `Enrichment` derives `Default`, which makes it possible to forget the
  identity echo (the enrichment is then simply discarded);
- the three copies of the `i18nKeysUsed` test and the admins' HTML tables
  would deserve a shared helper/component in the kit.
