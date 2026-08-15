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
that exist and reach past nine through its own `<`/`>` page arrows (see
[interface.md](interface.md)) — the remote's bare digits alone only
reach 1-9. The order is changed **by dragging a row** (or with the ▲▼
arrows, which remain the keyboard-and-touch-accessible path): moving a
station therefore changes its remote digit.

Saving a new station list from the admin page announces the fresh
`preset_count` to the core right away, as a spontaneous notification
(`SourcePlugin::poll_notification`) rather than waiting for a preset to be
played — otherwise the web grid kept showing the old set of numbers until
something was played on the radio. That notification carries `preset_count`
and **nothing else**: no view, no identity, no preset, so it disturbs
neither the display nor whatever is currently playing.

Playing a preset also declares its `preset_name`: the configured station
name, alongside the `preset` number, in the same frame. The field exists
because the name used to live only in a display line the core composed for
itself — one a `metadata` plugin was free to overwrite — and the SPA never
received it in structured form, so the web Player card had no stable name to
show. `preset_name` is absent (not cleared) on the "empty preset" branch:
nothing new is playing there, the previous station carries on, so its name —
if any — must stay exactly as it was.

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

Selecting an **empty** preset declares a **transient** `status` — "empty
preset" — for a few seconds, then whatever was already showing (the
station's own status, or nothing at all) returns on its own: nothing was
started, so nothing stopped, and the message must not durably describe a
state that does not exist. `transient` only ever qualifies `status`: it
feeds a passing overlay message and leaves whatever a source has
permanently declared untouched underneath, ready to reappear once the
message's time is up.

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

It never fills `preset_name`: a track number is not a name, and what is
interesting about a disc (album, title, artist) already arrives through the
`metadata` path (see below), not through the preset name.

What it declares instead is a `status`: "audio CD" whenever a disc sits in
the tray, "no disc" otherwise. Unlike `preset` and `preset_count`, whose
absence means "this frame says nothing, keep the previous value", an absent
`status` means **no status at all** — and the cd plugin restates one on
every single frame precisely because of that convention: it is the only one
that lets a status be cleared. Had absence meant "keep the previous one"
instead, "no disc" would stay on screen forever after a disc was inserted,
with no later frame able to cancel it. A display picks between this
sentence and the album once a `metadata` plugin resolves one — see the
plugin console's own choice below.

**A known limitation.** `preset` only travels while playback is under way:
a disc sitting in the tray, stopped, declares no preset at all, even though
its track index is perfectly known. This is not an oversight — `preset`
means "the key matching what is playing", the one the web remote
highlights, and a stopped disc has nothing playing to highlight. Loosening
that meaning to cover a merely-selected-but-stopped track would break the
very property the field exists for. The practical consequence: where the
console display used to show "CD 1/3" for a disc sitting idle in the tray,
it now shows "CD" alone in that state — the "audio CD" status stays visible
throughout, only the track number disappears until playback resumes.

## `ritornello-plugin-console` — the display

A display plugin receives the appliance's full state — `PlayerState`, the
same structured payload that feeds the SPA's Player card (see
[interface.md](interface.md)) — through a single one-way call,
`DisplayPlugin::show(state)`, no answer expected. **The core imposes no
layout**: it hands over data, never composed lines, so a future display (an
SSD1306 OLED over SPI/I2C, a wall panel with a scrolling ticker) is free to
lay its own screen out, at whatever size, with no fallback rule to
reimplement and no core change to request one.

Every piece of information the core knows travels both raw and already
resolved into words: `volume` is a number a display can turn into a gauge,
`status` is a sentence a display can just print — no display ever needs a
catalogue of its own to write what a source or the core already put into
words. `overlay` works the same way for a transient overlay (`Volume {
level, muted, text }`, `Tens { offset, text }`, `Message { text }`): the raw
value for whoever wants to draw something, `text` for whoever wants to
print it. Its `remaining_ms` is informative only — the core alone owns the
deadline and pushes a fresh frame the instant an overlay expires — so a
display may animate a countdown but must never decide for itself when the
overlay ends.

There is no `SetLocale` for displays: everything a display has to write
already arrives translated, by the source's own catalogue or the core's, so
a display plugin never has to resolve a word by itself. This is
deliberately not built ahead of need: the day a display wants its own words
(a scrolling ticker with an idiom no one else uses, say), adding
`SetLocale` to the display protocol is a new message a plugin can ignore
until it cares about it — non-breaking, unlike the rest of this protocol
change, which was only safe to make because it happened before the project
was published.

This bundled plugin (`RITORNELLO_CONSOLE_TTY` variable, default
`/dev/tty1`) targets a text screen of about twenty columns. Its layout is
**its own choice**, not a contract every display must follow:

- first line: `SOURCE  n/total` (just `SOURCE  n` when the source hasn't
  declared a count, bare `SOURCE` when nothing is selected) — its own
  idiom, standing in for what each source used to compose for itself
  ("RADIO  P4", "CD 1/3");
- second line: the source's preset name when it has one, else the album
  once a `metadata` plugin has resolved one, else the source's `status` —
  most specific first;
- third line: `artist — title`, with the same four fallbacks (both, either
  one alone, neither) it always had.

An overlay, when present, takes the whole first line and blanks the rest —
the display owner's own call, made and unchanged since before this
protocol moved. Control characters coming from any field — a station name,
a status word, an ICY title — are filtered before writing to the tty: now
that the plugin composes from raw network-sourced strings itself, every one
of its three lines is data that needs sanitizing, not just the title line
as it used to be.

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

On the displays, arbitration's result lands in the same structured state as
everything else: the resolved `artist`/`title`/`album` travel in
`PlayerState`'s flattened `morceau`, right next to whatever `status` the
active Source (or standby) declared. Which one a display shows, and how,
is now the display's own call — see the plugin console's layout choice
above for one example (preset name, then album, then status, most specific
first). The core never destroys information only the Source has: `status`
and `morceau.album` travel side by side, so a display that wants both can
show both.

In the web UI, the home page carries a **Player** card, above the remote:
active source, volume, and two badges for mute and standby. The preset
number a Source declares is shown alongside the readable name it gives it,
when it declares one (`preset_name`) — "Preset: 4 — FIP" for the radio,
just "Preset: 4" for a Source that names nothing at that slot. The track
**joins it** when known — with a badge indicating its **origin** (`icy`,
or the name of the winning plugin), the first question one asks in front
of a wrong title. The active Source's `status` sentence shows too, when
there is one — see [interface.md](interface.md) for the full shape of
that pushed payload. None of this is polled: the card updates over a
pushed stream, so the volume follows the infrared remote and the other
tabs.

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
