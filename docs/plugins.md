# The plugins

Plugin architecture: the core (`ritornello-core`) orchestrates plugins —
separate processes communicating over Unix sockets (line-delimited JSON
protocol) — of four kinds: **source** (content to play: radio, CD),
**input** (remote control), **display** (screens) and **metadata**
(now-playing metadata). Each kind has a stable interface; adding a new
plugin (e.g. a Bluetooth source, an OLED display) does not touch the core.

`ritornello-core` loads `/etc/ritornello/plugins.toml` at startup (see
`deploy/plugins.example.toml`): each entry declares a plugin (`source`,
`display`, `input` or `metadata`) and the path to its executable — nothing
else. An **admin page** is a property of the binary, not of the
deployment: the core offers `--admin-socket` to every plugin, and the one
that has a page declares it by **binding that socket** at startup. The
page is then served by the core under the same origin, with a link shown
on the home page (`http://<host>:8080/`) — no configuration line to know
about, hence nothing to forget.

A plugin's death is tolerated: it is marked unavailable on the config
page, the others keep working. None of these plugins is Pi-specific:
`ritornello-plugin-radio` and `ritornello-plugin-cd` are pure portable
Rust, `ritornello-plugin-generic-input` and `ritornello-plugin-console`
only depend on generic Linux hardware (respectively a USB infrared
receiver recognized by `evdev`, and a `/dev/ttyN` console) — not on GPIO
or any Pi-specific bus.

## `ritornello-plugin-radio` — internet radio

Its station management page is served by the core, under the single
origin, at `http://<host>:8080/plugins/radio/` (the plugin binds no
network port — its page is detected through the admin socket it binds).
It lets you enter a station by hand (name + stream URL) **and** add one
from the online community directory
[Radio Browser](https://api.radio-browser.info): type a name, pick a
country, "Search", then "Add" on a result. It is **the plugin** that
queries the directory — the page loads no external resource — and nothing
is written until "Save" has been clicked.

Presets are numbered **automatically by position** (1 to 99): adding
appends to the end of the list, deleting renumbers the following ones;
beyond 99, adding is refused. The station count is declared to the core
as `preset_count`, which is what lets the web grid show only the numbers
that exist and reach past nine through its `+10` window (see
[interface.md](interface.md)) — the remote's bare digits alone only
reach 1-9. The order is changed **by dragging a row** (or with the ▲▼
arrows, which remain the keyboard-and-touch-accessible path): moving a
station therefore changes its remote digit.

The search **country** is picked from a keyboard-filterable list,
populated by the directory itself (241 countries at the last count, with
each one's station count). Names are rendered by the browser from the ISO
code — no country table to translate in the language packs. The list is
only requested when the picker opens, never on page load, and the choice
is **remembered by the plugin** (in `plugin-radio.json`, next to the
current preset): it follows the device, not the browser.

Directory unreachable ⇒ error message on the page, current playback and
already-configured stations are untouched, and manual entry remains the
fallback. The directory is queried across **several servers tried in
order** (`de1`, `de2`, `at1`, `nl1`, `fi1` of `api.radio-browser.info`)
until one answers: `all.api.radio-browser.info` is a rotating record, and
the mirror fleet drifts over time — a vanished host fails fast, the next
one is tried, and every failure is logged. The whole thing fits in a
**4 s budget** (at most 2 s per server): the admin page goes through the
core's admin protocol, which abandons any request after 5 s, so a search
that drags on is stopped on its own with an error message rather than
ending in a timeout.

Selecting an **empty** preset shows "empty preset" for a few seconds,
then the display returns to the station that is playing: nothing was
started, so nothing stopped, and the message must not durably describe a
state that does not exist.

Variables: `RITORNELLO_RADIO_STATIONS`, `RITORNELLO_RADIO_STATE`,
`RITORNELLO_RADIO_DIRECTORY` (**pins** a directory server: it becomes the
only one tried, to impose your own mirror without recompiling; when
unset, the built-in list applies).

## `ritornello-plugin-cd` — the CD player

Disc detection by ioctl (`RITORNELLO_CD_DEV`, default `/dev/sr0`), TOC
read through `cd-discid`, next/previous tracks, ejection (`eject`
package). Album recognition does **not** live here: it is the business of
the MusicBrainz `metadata` plugin (see below) — a multi-second network
call has no place in the process that answers track commands.

It declares its track count as `preset_count` (`Some(0)` with no disc
loaded), the same field the radio plugin uses — this drives the same
preset grid described in [interface.md](interface.md), tracks standing
in for stations.

## `ritornello-plugin-console` — the display

Display plugin for the HDMI console (`RITORNELLO_CONSOLE_TTY` variable,
default `/dev/tty1`). Three lines composed by the core; control
characters coming from content (ICY titles…) are filtered before being
written to the tty. A future display (an SSD1306 OLED over SPI/I2C, for
example) would be a new plugin of the same kind, with no fallback rule to
reimplement.

The core's config page (`http://<host>:8080/config`, `/status` redirects
there) also offers an **audio output** picker, based on the ALSA devices
the system knows about (`aplay -L`) — a Bluetooth speaker already paired
through `bluetoothctl` will show up there automatically once exposed by
`bluez-alsa`.

## `ritornello-plugin-generic-input` — inputs

It opens **all** readable evdev devices (non-exclusively: the keyboard
keeps working normally) and translates keys into commands according to
`/etc/ritornello/input-bindings.toml`. Its page
`http://<host>:8080/plugins/generic-input/` lists the detected devices,
lets you learn one key per action, load a bundled preset (`mce`,
`keyboard`) and save; it also lets you import a preset from an uploaded
`.toml` file and export the selected device's current bindings to such a
file. Variables: `RITORNELLO_INPUT_BINDINGS`, `RITORNELLO_INPUT_PRESETS`,
`RITORNELLO_LOCALE`.

Each line an input plugin writes on its socket is an `InputMessage`: a
`Command` plus an optional `"held": true` flag set when a key **repeats
while held down** rather than being freshly pressed — for this plugin,
kernel autorepeat (evdev `value == 2`) on an already-known key. Absent (or
`false`) means a fresh press. `held` is **additive and backward
compatible**: a plugin that writes a bare `Command` line (no `held` field)
keeps working unchanged, parses as `held: false`, and `false` is never
serialized, so pre-existing messages stay byte-identical on the wire.

This plugin only marks **volume** repeats as `held` — a held Stop or Next
would otherwise machine-gun the command, since the kernel repeats much
faster than a sane step rate — and drops repeats of every other command
rather than forwarding them. The core, for its part, **paces** held volume
commands itself (an initial delay, then one step per interval; see the
volume-hold card in [interface.md](interface.md)) and ignores `held` on
any command other than volume, so a plugin that did send it elsewhere
would have no effect.

Plugins built with the Rust SDK return an `InputMessage` from
`next_command`; `From<Command>` covers the non-held case, so a plugin that
never sends held repeats can keep returning a bare `Command` — the wire
format stays backward compatible either way.

**Updating an existing installation** (old hard-coded-keyboard
`ritornello-plugin-mce`): in `/etc/ritornello/plugins.toml`, replace the
plugin's entry with `name = "generic-input"`, `exec =
"/usr/local/lib/ritornello/plugins/ritornello-plugin-generic-input"`.
`deploy/deploy.sh` automatically removes the old `ritornello-plugin-mce`
binary on the target so it does not keep running after an update.

## Now-playing metadata (the `metadata` kind)

A `metadata` plugin enriches what the active Source is playing **without
the Source knowing**. The core tells it what is playing, it answers with
what it knows about it.

Two layers stack up, and the second one wins:

1. **What the stream announces itself.** The core watches mpv's
   `metadata` property and reads the ICY header (`icy-title`), displayed
   **raw**, without splitting on `" - "`: the convention exists but is not
   guaranteed — OUI FM's webradios actually emit `Title - ARTIST`, in the
   reverse of the usual order. This layer works without any plugin, and
   without the Source having to declare anything.
2. **What a `metadata` plugin has learned**, if it matches what is
   playing.

**A plugin takes precedence over ICY under all circumstances**, as long
as the station does not change: what it said stays displayed even if the
stream announces a new title in the meantime. These streams' ICY is of
lesser quality — reversed order (`Title - ARTIST`), sometimes just the
station name as filler — and letting it take over on every track made the
display change shape twice per track.

Accepted trade-off: on a track change, the previous title stays displayed
until the plugin sends its frame — short in practice, both coming from
the station's same automation, but lasting if the plugin stops
responding. Changing station, on the other hand, wipes the slate clean:
the identity changes, and ICY takes over until the plugin's first answer.

With no `metadata` plugin declared, there is therefore no enrichment —
this is by design, not a regression. **Playback is never affected** by a
`metadata` plugin, and its failure is silent on screen. A plugin whose
process dies is marked unavailable on the config page; however, a plugin
that starts but never serves its socket stays shown there as connected
(same behavior as the `input` kind, whose connection is not awaited at
startup).

**Declaration order matters**, and this is the only kind for which it
does: between two plugins answering for the same track, the first one
declared in `plugins.toml` wins, and a plugin declared lower down never
overwrites it. The chosen criterion is predictability for whoever is
debugging: "first to arrive" would depend on network latency, so the same
installation would display different things from one boot to the next.

**Updating an existing installation.** `deploy/deploy.sh` installs the
new binaries but never overwrites an existing
`/etc/ritornello/plugins.toml` (it only provisions the default one when
the file is absent): without
manually adding the two `kind = "metadata"` entries (see
`deploy/plugins.example.toml`), a device already in service **loses the
CD track titles**, which the cd plugin used to provide itself before this
version. The rest of the display is unchanged.

### The two bundled plugins

- `ritornello-plugin-musicbrainz` recognizes a disc through MusicBrainz.
  This is the code that used to live in `ritornello-plugin-cd`, where a
  multi-second network call shared the process that had to answer track
  commands. No variable to set.
- `ritornello-plugin-ouifm-metas` reads the metadata feed of OUI FM's
  webradios. **Nothing to configure**: the table of 21 streams is
  embedded in the binary (`src/webradios.toml`), taken from the site's
  source of truth — the `apidata` JavaScript variable of its player page,
  where each stream carries its stream identifier and its metadata
  identifier. `scripts/fetch-webradios.mjs` regenerates it from that same
  source (with `--verifier`, it reports a drift without writing
  anything).

  Recognition is based on a **fragment of the URL**, not the whole URL:
  the one OUI FM serves carries a signed token and a format parameter
  that vary, while the stream identifier is stable. **Both URL forms** of
  a given webradio are recognized: the `streams.lesindesradios.fr` one
  and the historical Icecast mount (`ouifm3.ice.infomaniak.ch/ouifm3.mp3`)
  — the latter is the form met in practice, long published, hence
  referenced by directories and copied around by users.

  The optional `/etc/ritornello/ouifm-metas.toml` file
  (`RITORNELLO_OUIFM_METAS` variable, example in `deploy/`) is there for
  the day OUI FM changes something: its entries are consulted **before**
  the embedded table, which allows fixing a mapping gone stale or adding
  one, without recompiling.

### Where it shows up

On the displays, the core composes: `line3` carries `artist — title`
(falling back to either one alone — partial information beats none), and
`line2` receives the album **only if the Source declared its own `line2`
as replaceable**, that is, wrote it for lack of anything better. The cd
plugin uses this: it writes "audio CD", the album takes its place when a
plugin reports it, and the label comes back as soon as it is no longer
known. The criterion is that explicit declaration, not the line being
empty: otherwise a Source would be asking for the album by staying
silent, and one that wants an empty line would have no way to say so. The
core never destroys information only the Source has, and the Display
protocol stays unchanged: a future display has no fallback rule to
reimplement.

In the web UI, the home page carries a **Player** card, above the remote:
active source, volume, and two badges for mute and standby. The track
**joins it** when known — with a badge indicating its **origin** (`icy`,
or the name of the winning plugin), the first question one asks in front
of a wrong title. None of this is polled: the card updates over a pushed
stream, so the volume follows the infrared remote and the other tabs.

**Automatic track advance.** When a CD moves to the next track on its
own, mpv informs the core, which relays it to the Source
(`SourceReq::PlayerTrack`): the Source is what realigns itself and sends
back view and identity, since the core cannot modify an identity it has
made a principle of never interpreting. Display and metadata therefore
follow the advance without any key being pressed. The **end of the disc**
follows the same principle in reverse: the core signals the stop to the
Source, which realigns its state — without which the last track would
stay displayed indefinitely.

### Writing a `metadata` plugin

Implement the SDK's `MetadataPlugin` (`now_playing` / `next_enrichment`)
and call `run_metadata_plugin`. Two points of contract:

- the **identity** of what is playing is an **opaque** JSON produced by
  the Source, which the core only compares and relays. The radio plugin
  puts `{"kind":"stream","url":…}` there, the cd plugin
  `{"kind":"disc","toc":…,"track":…}`. A plugin that does not recognize
  the shape it receives simply stays silent;
- every enrichment must **echo back the identity** it concerns. This is
  the staleness guard: the core discards one that no longer matches what
  is playing, which prevents a slow answer from overwriting the next
  track. An enrichment whose text fields are all empty counts as a
  non-answer, and therefore lets a lower-priority plugin win.

`next_enrichment` must be **cancellable without loss**: its future is
dropped as soon as a `NowPlaying` arrives, so any durable state (open
HTTP connection, cache, queue) must live in the plugin, not in the
future's local variables. (The same requirement holds for the Sources'
`poll_notification`, and for the same reason — see the SDK docs.)

## A plugin's UI

A plugin that binds the `--admin-socket` the core offers it can ship its
own interface, without a single line of the core changing (the SDK does
everything: `run_admin_plugin` on
`ritornello_plugin_sdk::admin_socket_path()`). It answers three requests
of the admin protocol:

- `GetAsset("ui.js")` → an **ESM module** exporting `contract` (the
  contract version, see `web/kit/src/contract.ts`) and, as default, a Vue
  component;
- `GetAsset("ui.css")` → the module's stylesheet (its own Tailwind pass,
  important: the core's CSS only contains the classes the core sees);
- `GetCatalog` → its flat i18n catalog, which the view consumes through
  `t()`.

The shell mounts the module's default component passing it **two props**,
which are the entirety of the data-side contract:

- `catalog`: the flat i18n catalog returned by `GetCatalog`, to be
  consumed through `createT(catalog)`;
- `base`: the **absolute** prefix under which the core serves this
  plugin's routes, trailing slash included (`/plugins/<name>/`). Every
  URL in the module is built from it — `api.get(`${base}api/data`)` — and
  **never** relatively. A `./api/data` is resolved against the browser's
  URL, not against the plugin's prefix: on `/plugins/<name>` (no trailing
  slash) it designates `/plugins/api/data`, which the core interprets as
  a plugin named "api", hence a 404. The shell's router now canonicalizes
  the URL, but a module must not depend on that form: `base` is the
  guarantee, the displayed URL is not one. Both bundled modules declare
  `base` **required**, with no default value: the name a plugin is served
  under comes from `plugins.toml`, hence from the deployment, and a
  module that rebuilt `/plugins/<its-name>/` would be wrong — silently —
  as soon as an operator declares it under another name.

The module imports `vue` and `@ritornello/ui` **without bundling them**:
the shell provides them through an import map, so a single Vue instance
and a single set of components serve everyone. An incompatible contract
is reported in the interface rather than breaking the page.

Native ESM requires no build step: a simple plugin can ship a
**hand-written** `ui.js`. The two bundled plugins use a Vite build (see
`crates/ritornello-plugin-radio/ui/`) to benefit from `.vue` files and
TypeScript — a comfort choice, not a requirement.

Four things learned during this work stream, to know before writing a
third-party plugin's UI:

- `assets/vue.js` is the **runtime-only** build of Vue (no embedded
  template compiler): a plugin module must ship **precompiled templates**
  (`.vue` SFCs put through `@vitejs/plugin-vue`, or hand-written `h()`),
  never a `template: "<div>...</div>"` string evaluated at runtime — it
  would fail silently at runtime, not at build time. `vue-router`, for
  its part, is **deliberately not** in the import map: a plugin module
  must not use `useRoute` or `RouterLink` — its own copy of `vue-router`
  would bundle its own injection keys, incompatible with the shell's
  router.
- The admin protocol only carries **text** (`AdminResult::Asset { body:
  Option<String>, .. }`, see `crates/ritornello-proto/src/admin.rs`): a
  binary asset (font, sprite, wasm) would have to be base64-encoded by
  the plugin then decoded on the ESM module side. This is an accepted
  ceiling of the relay, not an oversight.
- A plugin's assets are only served on **a single path segment**
  (`/plugins/<name>/<file>`, no subdirectory): a plugin's build must
  therefore produce **flat** file names. A deeper path (e.g.
  `/plugins/<name>/assets/ui.js`) matches no core route and answers
  **404**. It used to fall through to the SPA fallback, which returned
  200 with the HTML shell: a dynamic `import()` received HTML, a very
  confusing failure mode since nothing flagged the error.
- The fonts declared by the core's themes (see
  [interface.md](interface.md)) come from a CDN, the only external
  resource of the whole interface; a plugin module that wanted its own
  fonts should follow the same fallback logic (system font when offline)
  rather than blocking rendering.
