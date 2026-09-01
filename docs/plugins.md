# The plugins

Plugin architecture: the core (`ritornello-core`) orchestrates plugins —
separate processes communicating over Unix sockets (line-delimited JSON
protocol) — of four kinds: **source** (content to play: radio, CD, audio files),
**input** (remote control), **display** (screens) and **metadata**
(now-playing metadata). Each kind has a stable interface; adding a new
plugin (e.g. a Bluetooth source, an OLED display) does not touch the core.

`ritornello-core` loads `/etc/ritornello/plugins.toml` at startup (see
`deploy/plugins.example.toml`): each entry only needs a `name` and the
path to its `exec`utable. The kind(s) a plugin serves (`source`,
`display`, `input`, `metadata`) and whether it ships an **admin page**
are not written in the manifest — they are properties of the *binary*,
which announces them to the core itself when it starts (see [Declaring
the plugins](#declaring-the-plugins)). The admin page, when there is
one, is served by the core under the same origin, with a link shown on
the home page (`http://<host>:8080/`) — no configuration line to know
about, hence nothing to forget.

A plugin's death is tolerated: it is marked unavailable on the config
page, the others keep working. None of these plugins is Pi-specific:
`ritornello-plugin-radio` and `ritornello-plugin-cd` are pure portable
Rust, `ritornello-plugin-generic-input` and `ritornello-plugin-console`
only depend on generic Linux hardware (respectively a USB infrared
receiver recognized by `evdev`, and a `/dev/ttyN` console) — not on GPIO
or any Pi-specific bus.

## Declaring the plugins

`plugins.toml` is not user data — it is the list of which installed
binaries the core launches — and `deploy/deploy.sh` treats it as such.
Each entry now needs only two keys:

```toml
[[plugin]]
name = "radio"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"
```

There is no `kind` key. The kind(s) a plugin serves, and whether it has
an admin page, are announced by the *binary* over a **register socket**
that the core opens before launching a single plugin:

1. The core binds its register socket first, then launches every
   plugin. Each one receives three arguments: `--register <path>` (the
   core's register socket), `--name <name>` (the name under which the
   manifest declared it — the plugin echoes this back verbatim in its
   announcement rather than inventing its own identity), and
   `--socket-prefix <prefix>` (the base path for the sockets *it* is
   responsible for binding).
2. The plugin binds its own sockets — `{prefix}-{kind}.sock` for each
   kind it serves (`source`, `display`, `input`, `metadata`), plus
   `{prefix}-admin.sock` if it has an admin page — and **only then**
   connects to the register socket and writes a single line of JSON
   describing exactly what it just bound, e.g.:

   ```json
   {"name":"mpd","kinds":["input","display"],"admin":true,"covers":true}
   ```

   before closing the connection. That last flag is a display's opt-in for
   the bytes of album covers, described with the display protocol
   below — like `admin`, it is **derived** from what the binary actually
   implements rather than asked of its author, which is the invariant of
   this handshake: an announcement cannot lie.

This "bind first, announce second" order is not merely a convention:
the SDK's `Runtime` enforces it structurally (see [Writing a `metadata`
plugin](#writing-a-metadata-plugin)) — each kind-specific builder method
binds its socket the instant it is called, and only `run()` writes the
announcement, once every requested socket already exists. The
announcement line is therefore a **readiness barrier**: once the core
has read it, it can connect to every socket it names with a single bare
`connect`, no retry loop needed. This replaces two guesses the core used
to make: retrying `connect` up to a hundred times, 100 ms apart, because
a plugin's socket might not exist yet; and probing the filesystem for up
to 2 s to learn whether a plugin had an admin page. Both of those delays
were paid **on every healthy startup**; a plugin that dies before
announcing, or never announces within the core's 10 s grace period, is
now named in the logs, and the delay is paid only on that failure.

That 10 s grace period **no longer condemns anyone**. The core owns the
register socket, so it keeps accepting on it for the whole life of the
process: the deadline only decides when startup stops waiting, never
what ends up wired. A plugin that announces at t+12 s — a cold boot from
an SD card, eight Rust binaries bringing up their runtimes at once — is
wired the moment it speaks, and so is a plugin restarted by hand weeks
later (its status lines are then **replaced**, not added to). A
`metadata` plugin arriving late takes its place **from the file**, not
the last one: the arbitration list is recomputed in full from
`plugins.toml` rather than appended to.

The status page therefore reports four states rather than two: wired
(`connected: true`), dead before announcing (`connected: false`),
**starting** (`starting: true`) — launched moments ago and not yet heard
from, which is normal and not yet worth reporting as a fault — and
**stalled** (`connected: false`, `stalled: true`) — the process is
alive, said nothing by the deadline, and may still speak. Only the last
two are worth waiting on; the first two are worth acting on.

Four, and `busy` is not a fifth: it sits on a different axis. These four
describe whether a *process* is alive and has spoken, and they are stored
state. `busy` describes whether an already-wired plugin's *admin page*
answers right now, is computed on every `/api/status` rather than stored,
and says nothing about liveness — see [A plugin's UI](#a-plugins-ui),
which describes the admin protocol's budgets and its `Ping`.

Starting and stalled are the same silence read at two different ages, and
they are mutually exclusive: a plugin gets ten seconds of benefit of the
doubt, after which its line is downgraded — but only if it still says
"starting" at that moment, re-read rather than assumed. A plugin that
announced itself, died, or was switched off in the meantime already says
something truer, and overwriting that would replace a fact with a guess.

**The death of a plugin the core did not launch is noticed too.** A
plugin restarted by hand escapes supervision — the core is not its
parent, so it will never see its exit code — and its death used to
produce nothing but a log line, leaving the page showing it as connected
forever. What the core does own are its sockets, and their closing is
now reported: the line flips to disconnected, and the name becomes
manageable again, so switching it on from the admin UI launches a real,
supervised process instead of being refused. Closing proves the peer
closed, not that the process died — either way it is no longer
reachable, which is what the page claims and all it claims.

Its **admin page goes with it**: the status lines stop advertising one, so
the entry leaves the top menu, and the core forgets the plugin's admin
backend along with any UI assets it had cached — `/plugins/<name>/`
answers a plain 404 instead of reaching for a closed socket. That last
part also fixes a development annoyance that had nothing to do with
death: the asset cache promised to last only "for the lifetime of the
plugin's process" but was never cleared, so a plugin restarted by hand
with a rebuilt `ui.js` kept serving the old one until the core itself
restarted. Re-announcing now clears it, which is the same act.

A dead `source` is also **unwired** from the core, exactly as it is when
the core watched the process exit itself: the two death paths must leave
the same state, or behaviour would depend on who launched the process.
Nothing switches to another source, though — nobody asked for this
shutdown. The music keeps playing, the active source keeps its name, and
it is the conjunction "active source X, plugin X not connected" that
carries the diagnosis.

This register/announce handshake is exercised end to end by the Rust
test suite (bind ordering, a plugin dying mid-registration, an unknown
or duplicate announcement, `metadata` ordering — see
`ritornello-plugin-sdk::runtime` and `ritornello-core::register`), but
has not yet been run on the actual Pi deployment: treat it as verified
by tests, not by hardware, until it has.

`plugins.toml`'s **order** still arbitrates one thing for `metadata`, and
it is now the only thing it arbitrates: between two plugins that both
*overwrite* the same track, the one declared **first in the file** wins,
whatever order they actually announced themselves in at startup —
network and process-start jitter would otherwise make the display
non-reproducible from one boot to the next. It no longer decides anything
beyond that tie — a plugin that only fills in what is missing never
competes with one that overwrites (see [Now-playing
metadata](#now-playing-metadata-the-metadata-kind)).

### Turning a plugin off

A third key, `enabled`, is optional and absent by default — absence
means active, so no `plugins.toml` in service changes meaning by
gaining this feature. It is not meant to be typed by hand: the switch
lives on the configuration page, and the core rewrites the file itself,
preserving whatever comments it carries.

Turning a plugin off **kills its process** — `SIGTERM`, then `SIGKILL`
if it lingers past a two-second grace — which is what actually frees
`/dev/sr0`, an evdev device, or a console: a plugin still running would
keep holding it regardless of what the core stopped calling. Turning it
back on relaunches the binary, which announces itself on the register
socket exactly as at startup, and is wired by the same hot-wiring path
a late announcement already takes — no restart of the core either way.

Switching off the active source hands over to the next one in the
cycle, or leaves the device with no source at all, which is a
legitimate state. And switching everything off does not fail startup:
if it did, the admin UI would disappear along with everything else, and
nothing would ever be switchable back on.

The file's order keeps arbitrating `metadata` priority even for a
plugin currently switched off: a plugin turned back on regains the
place its line occupies in the file, not the end of the queue.

`deploy/deploy.sh` treats `plugins.toml` as installed state, not user
data. On a device with no such file, it provisions
`deploy/plugins.example.toml` whole. On a device already in service, it
**completes** the file: the blocks of the reference list whose `name` is
not already declared are appended, comments included, and the script
prints the names it added.

Nothing already present is rewritten, because the merge only ever
appends: an `exec` edited by hand (the mce → generic-input migration
below), a metadata chain deliberately reordered, a plugin of your own
that the reference list knows nothing about — all survive a deployment
untouched. Matching is on `name` alone, never on the exec path, so an
entry whose binary you moved is still recognized as declared.

Two consequences worth knowing, both coming from the same place: the
script cannot tell a plugin you never had from one you removed on
purpose.

- A plugin **deleted** from `plugins.toml` comes back on the next
  deployment, and commenting its block out changes nothing — a commented
  `name` is not a declaration, so the block is simply appended again
  below. Keeping a plugin off a device is therefore decided in the
  repository, not on the device: remove it from
  `deploy/plugins.example.toml` — `deploy/deploy.sh` derives the list of
  binaries it ships from that file, so there is nothing else to keep in
  step. `enabled = false` (see [Turning a plugin
  off](#turning-a-plugin-off)) is a different, reversible thing — it
  stops a *declared* plugin from launching without touching the
  declaration itself, so the entry stays exactly where it is and a
  deployment leaves it alone, same as any other entry already present.
- An appended `metadata` entry lands **at the end** of the chain, hence
  last to break a tie should one ever arise (order matters for that kind
  only, and only between two plugins that both overwrite — see
  [Now-playing metadata](#now-playing-metadata-the-metadata-kind)). The
  bundled ones answer for disjoint stations, so the position is harmless
  today; move the block by hand if you ever add two that overlap.

Older versions of the script only ever provisioned the file, so every
plugin added after a device went into service — the `files` source,
`radiofrance-metas`, the `metadata` plugins split out of `cd` — needed an
entry written by hand on the device before it existed at all. That step
is gone.

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
beyond 99, adding is refused. The highest preset number in use is declared
to the core as `preset_count` — through the admin page presets are
contiguous 1..N, so this doubles as a plain station count in the normal
case. A hand-edited, sparse `RITORNELLO_RADIO_STATIONS` file breaks that
equivalence: stations at 1 and 40 declare `preset_count: 40`, and the
console display then reads "RADIO  1/40" — not a bug, the field's contract
is the highest number in use, not a count of how many exist (see
[interface.md](interface.md) for the general rule). It is what lets the web
grid show only the numbers that exist and reach past ten through its own
`<`/`>` page arrows — the remote's bare digits alone only reach 1-10. The
order is changed **by dragging a row** (or with the ▲▼ arrows, which remain
the keyboard-and-touch-accessible path): moving a station therefore changes
its remote digit.

Saving a new station list from the admin page announces the fresh
`preset_count` to the core right away, as a spontaneous notification
(`SourcePlugin::poll_notification`) rather than waiting for a preset to be
played — otherwise the web grid kept showing the old set of numbers until
something was played on the radio. That notification carries only
`preset_count`: `identity`, `preset`, `preset_name` and `status` are all
left unset (the radio plugin never fills the last two on this particular
frame), so it disturbs neither the display nor whatever is currently
playing.

Playing a preset also declares its `preset_name`: the configured station
name, alongside the `preset` number, in the same frame. The field exists
because the name used to live only in a display line the core composed for
itself — one a `metadata` plugin was free to overwrite — and the SPA never
received it in structured form, so the web Player card had no stable name to
show. `preset_name` is absent (not cleared) on the "empty preset" branch:
nothing new is playing there, the previous station carries on, so its name —
if any — must stay exactly as it was.

**It is also the only source that enumerates its presets by name.** The
source protocol carries a request for it, `SourceReq::ListPresets`, served
by `SourcePlugin::list_presets`; the radio answers its station table sorted
by number, and that answer is what fills the catalogue frame the displays
receive. The default body of that method returns the **empty list**, which
does not mean "not implemented yet" but "I only have numbers" — the truth
for the cd, where a track has no name without a database, and for `files`,
whose list *is* the queue rather than a set of presets. The list may be
**sparse** (stations 1, 5 and 99): `Preset::index` is the index `Select`
expects, never a rank, so nothing may derive one from the other by
subtraction.

The list travels **outside the correlation**, exactly like `preset_count`,
and the reason is worth writing down because the shape looks like an
oversight. Nothing in the source pipe can carry a list: a `SourceReq`
resolves to exactly one `SourceAction`, and `SourceClient` requires
`(Some(id), Some(action))` to untie its `oneshot`. So the correlated answer
to `ListPresets` is a plain `Noop` — which satisfies the correlation and
teaches the core nothing — and the list rides beside it, in
`SourceMessage::presets`, through the "is this frame worth forwarding"
predicate that already relays `preset_count`. Nothing had to be widened:
neither `pending`, nor `Source::request`, nor the core's pending-request
bookkeeping. A source that does not enumerate stays inert on this path too,
since the SDK converts an empty list into an *absent* field (both forms
remain readable, so a hand-written plugin may send `[]`).

Because the answer teaches the core nothing, the core asks in a **detached
task** and joins none of them: once at startup, one per wired source, and
again on every hot-wiring of a source (a late announcement, a plugin
switched back on, a plugin restarted by hand). Waiting for those answers
would put the source protocol's 5 s timeout on the startup path, once per
unreachable source — precisely the class of delay the previous chantier
existed to remove. The same detachment covers a genuine defect: without a
fresh `ListPresets` on every wiring, a source announced late would have
entered the catalogue with a **permanently** empty list, nobody ever asking
again, and a plugin whose configuration changed while it was switched off
would have kept the core on the old list. Saving the station table also
republishes the list on the radio's own spontaneous notification, which is
what propagates a station **rename** without any client asking for it.

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
core's admin protocol, which gives a `GetData` 5 s, so a search
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
`metadata` path (see below), not through the preset name. For the same reason
it keeps `SourcePlugin::list_presets`' default empty answer: a track has no
name without a database, so "no named presets" is the truth about a disc, not
a gap left to fill. What a consumer wanting to list a disc's tracks falls
back on is `preset_count` — see the MPD server's section for the one place
that difference is visible.

It is also the only source that overrides `SourcePlugin::can_eject` to return
true — the capability that greys the web remote's Eject key everywhere else.
It answers true **with or without a disc**: the capability describes the tray,
not what sits in it, and an empty tray is exactly what one opens. Deriving it
from "is a disc present" would grey the key precisely when it is wanted.

That method is how a source declares an eject capability at all: the SDK
stamps its result on every frame the plugin sends (`can_eject` in
`SourceMessage`), so a plugin author overrides one method instead of
remembering a builder call on each of the ten declaration paths — a capability
forgotten on one path would give a key flickering between live and greyed. The
default is **false**: not knowing means offering nothing, which is what leaves
radio, files and generic-input compiling unchanged with a correctly greyed key.
The field deliberately does **not** make a frame "interesting" enough to be
forwarded to the core (see `SourceClient`): a frame carrying only a capability
must stay inert, because a permanent frame without `status` *erases* the
remembered status, so waking up frames that are dropped today would wipe "no
disc" off the display. The capability rides the frames the core already
listens to instead.

What it declares instead is a `status`: "audio CD" whenever a disc sits in
the tray, "no disc" otherwise. Unlike `preset` and `preset_count`, whose
absence means "this frame says nothing, keep the previous value", an absent
`status` means **no status at all** — and the cd plugin restates one on
every frame it produces through its own status-issuing path (`activate`,
`wake`, `select`, `next`/`prev` while playing, `player_track`, `eject`, and
`stop`) precisely because of that convention: it is the only one that lets
a status be cleared. Had absence meant "keep the previous one" instead,
"no disc" would stay on screen forever after a disc was inserted, with no
later frame able to cancel it. A display picks between this sentence and
the album once a `metadata` plugin resolves one — see the plugin console's
own choice below.

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

## `ritornello-plugin-files` — audio files, local or on a share

It plays audio files sitting in a **root**: a folder of the device (a USB
stick, an internal disk) or an authenticated SMB share. A local folder and
a share are the same thing to the whole plugin — the mount is only a
detail of `kind = "smb"` — which is what makes local playback come almost
for free.

Its page, `http://<host>:8080/plugins/files/`, is where roots are
declared (host, share, subfolder, user, password, domain, whether writing
is allowed), where a folder is browsed and added recursively to the
current playlist, and where a playlist is saved and loaded again.
Declaring a share writes two things: the root into
`/etc/ritornello/media-roots.toml` (see `deploy/media-roots.example.toml`)
and its password into `/etc/ritornello/media-credentials/<name>.cred`
(mode `0600`) — the password is deliberately kept out of the roots file.

**mpv holds the playlist**, the plugin only drives the index: it hands
over a generated `.m3u` (`/var/lib/ritornello/plugin-files.m3u`, never
shown to anyone) and a starting index. Automatic advance therefore comes
back through `SourceReq::PlayerTrack`, exactly as for a disc, and the
plugin has nothing to pace itself. The `Play` it issues is marked
`finite`: a list of files ends normally, and without that word mpv going
idle at the end would look like a dropped stream and the core would
restart the list in a loop.

### Package prerequisites

Two system packages, only one of which is indispensable.

| Package | Role | Without it |
|---|---|---|
| `cifs-utils` | mounting an SMB share | A share that fails to mount is refused, not declared: see below. Folders of the device are unaffected. |
| `smbclient` | browsing a share **before** mounting it | The wizard opens straight into manual entry, no toggle or connect button. A share is still declared by entering host, share and subfolder by hand. Nothing else is affected. |

```sh
sudo apt install cifs-utils smbclient
```

The plugin **probes** for `smbclient` at startup and re-exposes the answer
as `can_browse_smb`. Unlike the System tab greying out rebooting when logind
refuses it, the wizard here offers no toggle to grey out: without the
package it is manual entry from the start. The probe is redone on every
connection attempt: installing the package without restarting the service
gives a correct answer, not a stale refusal.

### When a share stops answering

A mounted share that goes quiet is not the same failure as a mount that
failed, and the page says so separately. It is also the failure that once took
the whole plugin down, so the reasoning is worth keeping.

The kernel's cifs client does not notice a dead session until `echo_interval`
elapses — 60 s by default — and only then does `soft` start counting its
retries. A NAS that drops idle connections, or spins its disks down, therefore
produces accesses that block for tens of seconds while answering a Windows
machine instantly: Windows re-establishes the session itself, and has a user to
show a dialog to. `mount.cifs` is given `echo_interval=10`, `retrans=1` and
`actimeo=30` to shorten that window, but none of them bring the worst case
under a second.

That matters because the admin protocol bounds the *wait*, not the syscall:
`set_data` is exclusive, and a blocking `stat` inside it runs to completion
whatever the budget says. One is enough to hold the plugin's data for as long
as the kernel takes, even if `ui.js` and the catalog now answer on their own.
When it happens, the core's `504` names the cause and distinguishes a
request that ran past its budget from a plugin that is not answering at
all (`502`) — the two used to look identical on the page. So every filesystem access
a request triggers goes through a circuit
breaker (`sante.rs`): it runs off the async thread under a 1.5 s deadline, and a
mount point whose probe never returned is remembered, so later requests are
refused instantly instead of losing another thread — a syscall in
uninterruptible sleep cannot be killed, not even with `SIGKILL`. The abandoned
thread is also the only recovery detector: when the kernel finally releases it,
the mount point is cleared.

What you see while a share is quiet: the mount points are listed in a banner,
affected tracks are badged *unknown* rather than *missing* — they are not gone,
nobody could look — and their lengths simply do not arrive. Local sources and
playback are unaffected.

### Declaring a source

No address is typed blind any more: you browse first, you declare after.

- **A folder of the device** — the dialog opens on the mounted volumes,
  read from `/proc/mounts`. Pseudo-filesystems (`/proc`, `/sys`, `/run`,
  `/dev`) are neither offered nor browsable: without that bound, an "add to
  playlist" launched on `/` would wander off into `/proc/self`'s recursive
  links. The filter is a **blacklist of those pseudo types**, not a whitelist
  of accepted ones — a whitelist was tried first and made real disks
  unreachable with no workaround (`/mnt/c` under WSL is `9p`, an NTFS stick
  mounted by ntfs-3g shows up as `fuseblk`). An incomplete blacklist merely
  lets a stray entry into a list of choices.
- **A network share** — you enter an address, connect, `smbclient`
  enumerates the shares, and you walk down the folders from there. Nothing
  is mounted until you have confirmed.

The root's technical name is **derived** from the share name or from the
last segment of the chosen path, and deduplicated. It is no longer typed:
it becomes a component of `/mnt/ritornello/<name>` *and* a credentials file
name, so the derivation produces a valid name by construction rather than
by luck.

The mount **follows the declaration**: the plugin asks for reconciliation
itself. A failure **undoes** the declaration — the table entry and the
credentials file both, and the refusal goes back to the wizard, which stays
open with what was just typed still in it. It is precisely to avoid losing
that entry to a sleeping NAS that the popin is kept open on refusal, rather
than closing on a declaration that would then need to be found again and
removed by hand. The rollback is scoped to `add_source` only: a source
already accepted stays declared until removed by hand, so one sick share
does not take an unrelated, healthy one down with it.

### Track lengths

Only tracks **in the playlist** are measured, and only those whose length is
still unknown. A scanned folder carries none — an `#EXTINF` line in an m3u is
the only thing that does — so the column used to show a dash forever.

Lengths are read from the file **header**, in-process, and the choice was
measured on sixty files: 0.33 ms each that way, against 42 ms with one
`ffprobe` per file. Over two thousand tracks that is under a second instead of
a minute and a half, and it spares a Raspberry Pi two thousand process spawns
while music is playing. No system package is involved.

**`lofty` is kept out of the log below `error`**, and that is not tidiness. It
emits a `WARN` per MP3 without a Xing header — "MPEG: Using bitrate to estimate
duration" — which is not an incident but the normal estimation method for that
format, calls for no action, and repeats once per track. The owner reported it
as flooding the log, and the cost is real: the core keeps only `warn` and
above for its "recent errors" card, so that noise pushes actual errors out of
the buffer. Its `error` lines still go through — a frame the library judges
broken is information. A `filter_fn` and not an `EnvFilter`: the latter sits
behind `tracing-subscriber`'s optional `env-filter` feature, which pulls in
`regex` — one more dependency to compile and ship on a Pi, for a single rule
known in advance.

The work runs **in the background** and is polled by the page, like the
recursive scan: a `GetData` has a 5 s budget and a playlist coming off
a share needs longer. Results are applied by **path**, never by position — the
page may reorder or remove tracks while measuring, and applying by index would
write one file's length onto another. Each batch is persisted, so an
interrupted pass keeps what it found and a restart re-measures nothing.

A length that is already known is never overwritten: an `#EXTINF` is the
authority, since the file may be an excerpt of what the list claims.

### The privilege boundary

The service is unprivileged and runs with `NoNewPrivileges=true`, so it
cannot mount anything itself. It writes a configuration that a **root**
binary consumes: `/usr/local/lib/ritornello/ritornello-media-mount`, run
by `ritornello-media-mount.service`, which the plugin asks systemd to
start (`systemctl start`, the same way the System tab talks to logind —
no D-Bus dependency in Rust). A polkit rule,
`deploy/51-ritornello-media.rules`, grants the `ritornello` user
`manage-units` **on that one unit**.

Said plainly: whoever reaches the web UI decides what root mounts. So the
validation that counts lives on the **privileged side** — the plugin
validates too, but only as a courtesy to whoever is typing. The mount
binary re-reads and re-validates the whole file, and accepts only:

- a `name` matching `^[a-z0-9][a-z0-9-]{0,31}$`: it becomes a path
  component *and* a file name;
- `kind = "smb"` (local roots are not mounted, they are read where they
  are);
- a `host` and a `share` with **no comma**, no space and no `..`. The
  comma is the one to watch: `mount.cifs` separates its options with
  commas, so a host `nas,uid=0` would add an option to the line root
  executes;
- a mount point that is **never read from the configuration** — it is
  always `/mnt/ritornello/<name>`, built from a constant and the
  validated name. A free-form path would be one more path to validate,
  and root would be the one using it;
- a **closed list** of mount options (`ro` unless the root is declared
  writable, `soft`, `iocharset=utf8`, the service's `uid`/`gid`,
  `credentials=<path>`). There is no pass-through to `mount -o`.

The binary **reconciles**: it mounts what is declared and absent,
unmounts what is no longer declared, and is idempotent — hence rerunnable
without precaution, including at boot (the unit is enabled by
`deploy.sh`). A single share failing to mount does not fail the service:
the others still go up.

systemd offers no equivalent of logind's `CanPowerOff` for `manage-units`
— there is no "CanStartUnit" — so the plugin cannot grey a button out
ahead of time. It tries, and **reports `systemctl`'s error output
verbatim** on the page: a polkit refusal is explicit and actionable there
("install `51-ritornello-media.rules`"), where a message of our own would
have made it opaque.

The NAS password sits in clear text in a file the service can read. Same
level of trust as the rest of the appliance — whoever reaches the UI can
already do everything — but it is worth writing down.

### Two ceilings

**99 presets.** `preset` is a `u8` in the 1–99 range, so a list longer
than that declares `preset_count: 99` and numbers only its first 99
tracks. The rest still plays: mpv walks the whole list, so next/previous
reach every track, and the page lists them all — no digit designates
them, that is all. (Same field as the radio's presets and the CD's
tracks, driving the same web grid, see [interface.md](interface.md).)

**And they are named.** `list_presets` returns those 99 entries with each
track's title — the same name `preset_name` already published for the current
track, and the same one the m3u writes as `#EXTINF`. Without that override the
trait's default returned an empty list, so the home page's tiles carried a bare
number where the radio shows "1 · FIP". The names travel again with the count,
on the same channel: the watch fires on **every** list change (`watch::send`
signals even on an equal value), so a mere reordering — which does not change
the count — renames the tiles too. Keeping the old titles under the new numbers
would be worse than no title at all. Dense and capped at 99, exactly like
`preset_count`, because the two describe the same thing and must agree: a file
list has no holes, so the index really is "position plus one" — which is *not*
true of a sparse station table (see the MPD server's section, § Dense
positions, sparse indices).

**2000 tracks per list.** A recursive add that would go past it is
**refused with a message** naming the ceiling, rather than silently
truncated: a playlist that quietly shrinks is a defect that takes months
to attribute. Narrow the folder down, or add its subfolders one by one.
The recursive walk filters by extension (`mp3`, `flac`, `ogg`, `oga`,
`opus`, `m4a`, `aac`, `wav`, `wma`, `aiff`, `ape`, `wv`, `mpc`, case
insensitively) and guards against symlink loops.

Track titles need no plugin here: the core reads the file's own tags from
mpv's `metadata` property (see the `metadata` section below), and the
`preset_name` the source declares — the `#EXTINF` title, else the file
name without its extension — makes sure the screen is never mute even
with no tags at all.

Variables: `RITORNELLO_FILES_ROOTS`, `RITORNELLO_FILES_CREDENTIALS` and
`RITORNELLO_USER` (read by the **mount binary**, which runs on its own,
outside the service's environment), `RITORNELLO_FILES_STATE`,
`RITORNELLO_FILES_MPV_PLAYLIST`, `RITORNELLO_FILES_PLAYLISTS` (where
playlists saved "internally" live, as opposed to those written onto a
root) and `RITORNELLO_LOCALES` (read by the plugin).

**Saving onto a share needs one extra word.** Shares are mounted `ro`, so
saving a playlist onto one is refused with a message rather than a kernel
I/O error nobody could attribute; a root has to be declared writable for
that. Playlists are written as m3u with paths **relative** to the root
they sit on, which is what makes them readable by any other player and
survivable across a change of mount point. Reading back a playlist a NAS
wrote is deliberately forgiving — a `Z:\Musique\…` or `/volume1/music/…`
entry is retried under the root — and whatever stays unresolved is
**reported on the page rather than dropped**: a playlist that silently
shrinks is a defect that takes months to attribute.

**Updating an existing installation.** As for every other plugin,
`deploy.sh` installs the binary and, on a device already in service,
appends the `files` entry to `/etc/ritornello/plugins.toml` if it is
missing (it never rewrites what is already there — see [Declaring the
plugins](#declaring-the-plugins)). The unit, the polkit rule,
`/mnt/ritornello` and the credentials directory are installed by the same
run, so the source is usable straight after a deployment.

## `ritornello-plugin-console` — the display

A display plugin receives the appliance's full state — `PlayerState`, the
same structured payload that feeds the SPA's Player card (see
[interface.md](interface.md)) — through a one-way call,
`DisplayPlugin::show(state)`, no answer expected. It is not the only call:
the protocol carries **three** kinds of frame today, and a plugin may ignore
two of them. **The core imposes no
layout**: it hands over data, never composed lines, so a future display (an
SSD1306 OLED over SPI/I2C, a wall panel with a scrolling ticker) is free to
lay its own screen out, at whatever size, with no fallback rule to
reimplement and no core change to request one.

Each line on that socket is a **frame**, tagged by a `frame` key with the
payload beside it in `data`. The tagging is adjacent rather than internal
because `PlayerState` flattens `Morceau`, and flatten crossed with an
internally-tagged enum is a known serde blind spot; the state frame's `data`
is therefore byte-for-byte the payload that used to travel bare, which is
what made that migration verifiable.

- `{"frame":"state","data":{…}}` — the `PlayerState`, up to once a second
  while something plays.
- `{"frame":"catalog","data":{…}}` — the declared sources, in
  `SourceCycle` order, and for each one that can enumerate them its named
  presets (see [`ListPresets`](#ritornello-plugin-radio--internet-radio) in
  the radio section). A source that does not enumerate is still listed, with
  an empty list: it exists, and the consumer falls back on `preset_count`,
  which stays the truth about the number.
- `{"frame":"cover","data":{…}}` — the bytes of the cover of what is
  playing, base64 on the wire, pushed **only** to the displays that asked
  for them.

**Why the catalogue has a channel of its own** rather than a field in
`PlayerState`, since that is the decision a future reader is most likely to
want to undo. A field present only when it changed would be an *event*, not
a snapshot, and `PlayerState` is a snapshot: the core rebuilds it on every
publication and deduplicates it by equality. A field always present would
therefore send the 51 station names on every per-second frame of playback,
and deduplication would catch nothing, since both values change together by
construction. So there are two `watch` channels, `publie_etat` never calls
`publie_catalogue` and `publie_catalogue` never calls `publie_etat`. The
catalogue is republished only where it can actually change: at core
construction (the sources wired at startup), when a source's presets
arrive, on `add_source` and on `remove_source` — a plugin switched off must
leave the list, otherwise an MPD client would keep a stored playlist to act
on. It is deduplicated the same way the state is, for the same reason: the
radio republishes the same list on every save of its admin page, and that
must not wake the displays.

Both new frames have a **default body that ignores them** —
`DisplayPlugin::catalogue` returns `Ok(())`, `DisplayPlugin::cover` too —
which is what makes each new kind of frame a non-breaking addition, and is
also why **no behaviour of this console plugin changed** when the two were
added: a twenty-column screen has no use for either. A frame of a kind the
SDK does not know is treated like an unreadable line (warn, then continue)
and the connection survives.

Covers are **opt-in, and the opt-in cannot lie**: a display that wants the
bytes overrides `DisplayPlugin::wants_covers`, and the SDK *derives* the
`covers` flag of the register announcement from that method rather than
asking the plugin author to declare it (same shape as the `kinds` and
`admin` flags — see [Declaring the
plugins](#declaring-the-plugins)). The core pushes bytes only to the
displays whose announcement carried the flag, and only when the cover
actually changes — change being detected on the state frame's `cover_href`,
which is the identity of the image (the cache key), not a timestamp, so a
cover that stays on screen is never re-sent. The materialization of the
bytes, the only moment the whole image exists in the core's memory, sits
**behind** that filter: a display that does not want covers does not make
anyone pay the file read either. The flag is read once, at registration, and
never re-read; a display that changed its mind has nothing to expect from
it, and needs nothing — `cover` can simply ignore.

A cover frame is **self-contained**: one line carries one whole image, never
a slice. That is what makes it compatible with the SDK's unreadable-line
policy — skipping a self-contained line loses one image, whereas skipping a
slice would produce a truncated image that no check would catch. It carries
the same `cover_href` the state frame publishes for that image, because
frames do arrive in order on a single socket but nothing *inside* an image
says which one it is, and a plugin that must answer "the cover of that
track" (the MPD server does) has no other correlation available. Its ceiling
is `COVER_MAX_BYTES`, **20 MiB** — see the MPD server's section below for
what that number costs and what it excludes.

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
was published. That prediction has since been tested twice, by the catalogue
frame and by the cover frame: both were added, both have default bodies, and
**not one line of this plugin's behaviour changed** for either — its only
edit was a test pinning that it does *not* ask for covers, written on the
side that must stay silent, since that is where the regression would happen
if someone added an override by mimicry.

This bundled plugin (`RITORNELLO_CONSOLE_TTY` variable, default
`/dev/tty1`) targets a text screen of about twenty columns. Its layout is
**its own choice**, not a contract every display must follow:

- first line: `SOURCE  n/total` (just `SOURCE  n` when the source hasn't
  declared a count, or declared it as zero — nothing to number, an empty CD
  tray — since "1/0" would be absurd; bare `SOURCE` when nothing is
  selected) — its own idiom, standing in for what each source used to
  compose for itself ("RADIO  P4", "CD 1/3");
- second line: the source's preset name when it has one, else the album
  once a `metadata` plugin has resolved one, else the source's `status` —
  most specific first;
- third line: `artist — title`, with the same four fallbacks (both, either
  one alone, neither) it always had.

The position that now rides in every `PlayerState` frame finds no line of
its own here: all three are already spoken for — source and count, name or
album or status, artist and title — and the core publishes one such frame
per second throughout playback, so a fourth line built around a clock would
cost a full screen erase every second it played. A test locks the choice
down: a frame that only changes `position_s` composes the exact same three
lines as the one before, so nothing is rewritten and nothing flickers. Any
other display plugin is free to draw the field instead — the console is
simply too narrow a screen to be the one that does.

An overlay, when present, takes the whole first line and blanks the rest —
the display owner's own call. The volume/mute overlay used to span two
lines ("VOLUME" then "65 %") before this protocol moved its text into a
single `text` field; folding it onto one line is the one visual change
accepted during that move — every other view stayed pixel-identical.
Control characters coming from any field — a station name, a status word,
an ICY title — are filtered before writing to the tty: now that the plugin
composes from raw network-sourced strings itself, every one of its three
lines is data that needs sanitizing, not just the title line as it used to
be.

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
lets you learn the key — or the keys — of each action, load a bundled preset
(`mce`, `keyboard`) and save; it also lets you import a preset from an
uploaded `.toml` file and export the selected device's current bindings to
such a file. Variables: `RITORNELLO_INPUT_BINDINGS`,
`RITORNELLO_INPUT_PRESETS`, `RITORNELLO_LOCALE`.

Learning listens for thirty seconds, in a dialog naming the action and the
device; the four ways out of that dialog — its "Cancel", the cross, Escape,
a click on the veil — all cancel the listening session on the device, not
just the box on screen. A checkbox there **adds** the captured code to the
codes already on the row rather than replacing them, so a single action can
answer to several keys. And a code claimed by two actions is flagged under
both fields while it is being typed, each message naming the other action,
with saving refused until one of them releases it — the very
`duplicate_code` this plugin would answer with, said one round trip
earlier.

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
`ritornello-plugin-mce`): `deploy/deploy.sh` removes the old
`ritornello-plugin-mce` binary from the target, so it does not keep
running after an update, and appends the `generic-input` entry to
`/etc/ritornello/plugins.toml` if it is absent. What it does **not** do
is delete the old entry — it never removes anything (see [Declaring the
plugins](#declaring-the-plugins)) — and that entry now names a binary
that no longer exists, which the core reports at every startup. Delete
the `name = "mce"` block by hand, once.

## `ritornello-plugin-mpd` — the appliance seen as an MPD server

It speaks the MPD protocol over TCP so that a phone's MPD client — M.A.L.P.
is the one this was built against — can act as a remote control, with a
screen, without an app to write. One binary serving **two kinds**: the
`display` half receives the core's frames and drops them in a shared state,
the `input` half pushes protocol `Command`s the client's actions translate
into. There is no third path: everything it can show is something a display
already receives, and everything it can do goes down the very channel the
infrared remote feeds — including the two commands added for it,
`Command::SetVolume` (`setvol`) and `Command::SelectSource` (`load`), which
are absolute where the remote's keys are relative and are therefore now
available to any input plugin (see [interface.md](interface.md)). Nothing
here reaches into the core sideways.

**What a client sees.** Each audio **source** declared in `plugins.toml` is
a **stored playlist** (`listplaylists`), named exactly as the manifest names
it, and listed **in the order the catalogue frame carries** — the plugin does
not sort, deliberately: that order is the core's source-cycle order (today,
the names sorted alphabetically, re-sorted when a source is wired late so
that the cycle does not depend on boot chronology), which is the order the
remote's source key walks. Re-sorting on this side would just be a second
opinion about it. Its entries are that source's named presets
(`listplaylistinfo`), and the **active** source's presets are the **queue**
(`playlistinfo`, `plchanges`, `currentsong`, `status`). That correspondence
is what makes the appliance readable from a client at all: "load the radio
playlist" means "listen to the radio". So `load` does not concatenate as it
does in MPD — the queue *is* the active source's list, hence loading one is
choosing one, and it emits `Command::SelectSource`. Playing an entry emits
`Command::Select`. `listplaylistinfo` answers for a source that is not
playing too, since what a source contains is a fact about the source, not
about what plays.

Two details of that listing are decisions rather than accidents. Each entry
carries a `Last-Modified` of `1970-01-01T00:00:00Z`, a **constant**: no date
exists on this side — a source is not a file — and deriving one from the
clock would make a client believe a list had just changed every time it
re-read it. The field is nonetheless emitted rather than omitted, optional
though the protocol makes it, because clients read it without keeping it
(libmpdclient sorts its lists on it) and its absence trips them; a value that
will never move can never lie. And a client that asks **before the first
catalogue frame** gets nothing at all, which is the truth of that instant —
the plugin then knows of no source — and it will re-read after its
`stored_playlist` wake-up.

**Browsing, and what browsing means here.** There is no tag database — nothing
indexes artists or albums on this appliance — but there *is* content to walk:
the sources, and each one's presets. `lsinfo` therefore answers the stored
playlists at the root (exactly what real MPD does there, and exactly what
`listplaylists` returns), and a source name lists that source's entries, the
same lines `listplaylistinfo` gives. A client's file browser thus shows the
library the appliance actually has. The tag queries — `list`, `find`, `search`,
`count`, `listall`, `listallinfo`, `listfiles` — answer **well-formed and
empty** (`count` answers `songs: 0`, `playtime: 0`, which clients read without
testing). That is a correction, not a softening: they used to be refused, on
the theory stated below that a client greys out whatever `commands` omits.
M.A.L.P. does not — its Albums tab showed an error where an empty list shows
nothing — and the whole point of that paragraph is the difference between
"empty tabs" and "tabs that crash". A search with no filter is still refused,
because a truncated request must be learned rather than mistaken for a search
that found nothing.

**Playing an entry of a stored playlist.** Touching a track in a playlist has to
play it, and it used to answer `ACK 5`. A client that "plays" an entry adds it to
the queue first (`add` / `addid`), often after emptying it (`clear`), so those
three are handled — by translating "play this entry" into the only vocabulary
the appliance has: pick the source, then the preset. The URI makes that possible
because **we** published it (`currentsong`, `listplaylistinfo`, `lsinfo`) and it
names both. Two commands go out when the entry belongs to another source, one
when it is the active one; `addid`'s position is ignored, there being no queue to
insert into, and an index falling in a hole of a sparse list is refused just as
`playid` refuses it. `clear` answers OK and does nothing: there is no queue to
empty, and an `ACK` there would abort the `clear`/`add`/`play` list a client
sends to play a track — the refusal would cost exactly the feature. The client
re-reads `status` and finds the queue unchanged: a benign surprise, against a
gesture that works.

**What a client does not get**, and this is a list, not an apology: no queue
*rearranging* (`delete`, `move`, `swap` — the queue is not ours to reorder, it is
what the active source offers), no writing playlists (`save`, `rm`,
`playlistadd` — a source's presets are edited on that source's own admin page),
and no `update` (there is no database to index). `repeat`, `random`, `single` and
`consume` are reported as `0` and cannot be set: reported rather than
omitted, because clients read them unconditionally and misbehave without
them, but writing them is refused. There is one audio output, always
enabled, and `enableoutput`/`disableoutput` are refused — a client that sees
no output at all displays "muted" and stops trying.

**`commands` is what makes this honest rather than merely incomplete.** A
well-behaved client reads that list and greys out the rest itself; the
difference between "empty tabs" and "tabs that crash" is exactly that
answer, so the list must never promise more than the dispatch really
handles, and a test walks it to check that each name it carries is. Its
counterpart `notcommands` answers empty, which is the honest answer and not
a stub: `notcommands` lists what the current password *forbids*, and there
is no password here, so nothing is forbidden by permission — what does not
exist is simply absent from `commands`. The banner, on the other hand,
announces version `0.23.5`, which the plugin does not implement in full.
That overstatement is deliberate: clients derive their capabilities from the
version number and not from `commands` alone (libmpdclient and M.A.L.P.
compare it before emitting `plchanges`, `seekcur` or `tagtypes`), so
announcing a low version would make them give up commands that do work. The
opposite risk is bounded by `commands`, which tells the truth, and by the
`ACK 5` everything else gets. `binarylimit` is part of that bargain: at the
version announced a client treats it as a given and sends it **while
connecting** (M.A.L.P. does), so refusing it meant an `ACK 5` at the worst
possible moment. It is honoured, clamped to `[64, 65536]` rather than refused —
the upper bound is our decision and not a rule of the protocol, and a slice
smaller than asked for is always legal, since the value is a maximum. The gain
is real: a 500 KiB cover took sixty-two round trips in the 8 KiB default.

### Idle, and what counts as an event

**The clock is not an event.** The core pushes a state frame **once per
second** while playing, and the only field that moves is `position_s`. Treating
that like any other change woke every client asleep on `idle player` once a
second; M.A.L.P. then re-asked `status`, `currentsong` **and the cover art** at
that rate, which chopped up the very chunked transfer it had just started. Real
MPD never emits `player` for elapsed time — `elapsed` is read from `status`,
whenever the client wants it. So a position change only counts when it is a
*move*: an appearance or disappearance, any step backwards, or a jump forward
of more than five seconds. Five and not one because frames travel through a
`watch`, which coalesces: a relay momentarily behind sees the clock skip two or
three seconds with nobody having moved anything. The price is that a seek under
five seconds does not wake sleepers; they read it at their next `status`, where
`elapsed` is always right.

**A cover that is announced is waited for, not refused.** The core sends the
state first and the bytes second, so at every track change there is a window —
the time to read a `folder.jpg` off a share — where the frame already names the
next `cover_href` while the plugin still holds the previous image. That window
is exactly when the client asks, since that same frame is what woke it.
`albumart` used to answer "No file exists" there. The reasoning (the client will
re-ask at the next wake-up) holds for an ideal client; M.A.L.P. **remembers the
absence** per track so as not to hammer the server, and never re-asked — the
cover stayed blank until the next track, where it happened again. The session
now waits up to three seconds for the announced image, and refuses only when
nothing announced one, when the URI names another track, or when the wait runs
out. Waiting costs nothing to anyone else: a session is a task of its own.

**Every refusal is logged with the whole command**, arguments included, at
`info`. A client that hits an unhandled command shows a generic message —
"unsupported" — and the operator then has no way of knowing *which*: that is
precisely what was missing to diagnose M.A.L.P. failing to play a track from a
stored playlist. `info` and not `warn`, because a refusal is an ordinary
protocol answer (a client tries, learns, moves on) and the core only keeps
`warn` and above for its "recent errors" card — pouring every unknown command
from a chatty client in there would fill it with noise.

### Dense positions, sparse indices

**This is the one thing worth reading before touching this plugin.** Preset
indices are **1-based and may be sparse**: `Stations::preset_count` returns
the *maximum* number in use and not a count, so stations numbered 1, 5 and
99 are perfectly legal (see the radio section, and
[interface.md](interface.md) for the general rule). MPD positions, on the
other hand, are **dense**. The mapping is therefore:

| MPD | this appliance |
|---|---|
| `Id`, `songid` | the preset index as-is — sparse, ≥ 1 |
| `Pos`, `song` | the **rank** in the list returned — dense, 0-based |
| `play <POS>` | the index found at that rank, never `POS + 1` |
| `playid <ID>` | `Select(ID)`, but only after checking the id is really *in* the list |
| `playlistlength` | the **length** of the list, never the maximum index |

Three stations numbered 1, 5 and 99 therefore answer `playlistlength: 3`,
never 99 — publishing the maximum would make a client ask for ninety-six
entries that do not exist. And `playid` checks membership rather than
comparing against a bound, because an id below the maximum yet absent from a
sparse list must be refused, where a bound check would let it through. The
distinction is invisible on a dense list, which is exactly why it has to be
written down: the bug it prevents only appears on a hand-edited, sparse
station file.

The queue itself is built on two branches. When the catalogue holds a
**non-empty** list for the active source, that list is the queue, sparse
indices included. Otherwise the plugin **synthesizes** `1..=preset_count`,
which is dense by construction (`Pos = Id - 1`); that fallback is what shows
the twelve tracks of a disc, since the cd names nothing and its catalogue
entry carries an empty list — meaning "I only have numbers", not "I have
nothing". An absent `preset_count` becomes **zero** entries, not the ten of
the web UI's default grid: that grid is a keypad, and announcing ten
entries would make a client ask for ten things of which none exists. Note
what the synthesis cannot do: `preset_count` describes the *active* source
only, so `listplaylistinfo` on an idle source that does not enumerate
answers a well-formed **empty** list rather than a guessed number.

### Network posture

`0.0.0.0:6600` by default — the same surface the appliance's web server
already exposes — and **no password**. The consequence, stated plainly:
**anyone on the local network can change the station, switch source, move
the volume and cut the sound**, exactly like anyone holding the remote in
the room. That is the deliberate trade (`password` is even accepted without
being checked, so that a client configured with one is not rejected for it;
it grants nothing, because nothing is restricted), and it is worth knowing
before exposing port 6600 on a network you do not trust. One capability is
withheld rather than granted by that logic: `kill` is **refused**, not
ignored, because shutting the appliance down from the network without
authentication is something no remote in the room can do.

Settings live in `/etc/ritornello/mpd.toml` (`RITORNELLO_MPD_CONFIG` moves
the path), two keys, `listen` and `port`. No file has to be provisioned: the
defaults are exactly what `deploy/mpd.example.toml` contains, and a file
that is missing, unreadable or refused by validation falls back to those
defaults **with a log line** rather than refusing to start — a plugin that
refuses to start disappears from the status page instead of explaining
itself on it, which is the same policy the radio's station table follows.
The admin page is at `http://<host>:8080/plugins/mpd/`; it writes the file
through a temporary and a rename, so no power cut leaves a truncated toml.

**A change takes effect at once**: a successful save pushes the new settings to
the network half, which binds the new address/port itself — no restart, which
is what the page used to ask for. Three properties of that rebinding are worth
knowing. The old listener is dropped **only once the new one is bound**, so a
port already taken, or an address no interface carries, leaves the appliance
serving where it was serving (the failure goes to the log; the page still says
"saved", because the file *was* saved and validation cannot foresee an occupied
port). Sessions already open are **not cut** — they hold their own socket,
which closing a listener does not touch, so a phone that is listening keeps its
connection on the previous port until it closes it itself, where a real MPD
restart would have torn it away. And the session cap survives rebinding: the
semaphore lives outside the loop, so `MAX_SESSIONS` cannot be worked around by
saving repeatedly.

**The port is bound before the plugin announces itself.** That is the same
doctrine the SDK holds for its Unix sockets (bind first, announce second —
see [Declaring the plugins](#declaring-the-plugins)), and here it buys
something concrete: a port 6600 already taken makes `main` fail *before* a
`Runtime` even exists, so the plugin dies **unannounced**, the core reports
it as dead-before-announcing, and the **status page shows it**. Without that
ordering, an occupied port would be something an operator had to guess from
the logs.

### Four limits, and why each exists

This is a port open to the whole local network on a device with a single
gigabyte of RAM shared between mpv, the core, the web UI and the nine
plugins the reference manifest declares.
None of these four needs malice to be reached — a port scanner or a buggy
client gets there by accident, and taking down the plugin here takes down
the music with it.

- **Per line: 8 KiB** (`MAX_LIGNE`). Without it, a client that connects and
  sends bytes while never sending a newline makes the plugin allocate until
  the allocator gives up. The line reader is written by hand
  (`fill_buf`/`consume`) for exactly this reason: `BufReader::lines()`
  accumulates to the `\n` without any bound. 8 KiB is twice MPD's own input
  buffer and an order of magnitude above the longest legitimate line (a
  quoted playlist name, a few hundred bytes). Over the limit the connection
  is **closed**, not `ACK`ed: the command name is unknowable by then, so
  there would be nothing to name in the ACK, and keeping a connection that
  has already left the protocol buys nothing.
- **Command-list bytes: 256 KiB** (`MAX_OCTETS_LISTE`), alongside a count of
  **2048 commands** (`MAX_COMMANDES_LISTE`). Between `command_list_begin`
  and its `end` nothing executes and every line is *kept*, so a client that
  never sends the `end` grows a `Vec` without bound. The count alone was not
  enough: 2048 legitimate lines of 8 KiB is 16 MiB, the very order of
  magnitude the line limit exists to forbid — which is also why MPD
  expresses its own limit in bytes. One caveat, because the unit invites a
  wrong reading: these count **bytes of text, not bytes of heap**. Tokenizing
  allocates a `String` per token, so a legal line of `"a a a a …"` becomes
  thousands of one-character strings, and 256 KiB counted can weigh several
  real mebibytes.
- **Composed response: 1 MiB** (`MAX_REPONSE`) — the most instructive of the
  four, because it is the one a command-list limit does *not* cover: that
  one bounds commands, not what they **produce**. A list of 2048
  `playlistinfo` — **26 KiB of input**, a loop, no malice whatsoever —
  returns four lines per queue entry, up to 1020 lines per command at the
  maximum `preset_count` of 255: two million `String`s, and above all **a
  single contiguous allocation of tens of mebibytes** at the moment of
  flattening it all for the `write_all`. On a Pi 2 B a contiguous request
  that size fails against fragmented memory long before the total is
  reached. 1 MiB still admits some sixty complete `playlistinfo` in one
  list, the longest legitimate response being about fifteen kibibytes.
- **Simultaneous sessions: 16** (`MAX_SESSIONS`). The multiplier of the
  other three: each of them bounds one connection, and nothing bounded the
  number of connections, so a hundred sessions reached the device's gigabyte
  by the one path the other ceilings left open. 16 is three times any
  legitimate population (two phones, `mpc` on the device, a tablet, a
  desktop client — and MPD clients open one connection each, sometimes a
  second to hold an `idle` apart). Over it a connection is **refused at
  once** rather than queued — making it wait would hold a descriptor open
  and let the client believe it is being served, whereas an unreachable MPD
  server is a state every client knows how to display — and the log names
  the ceiling so the cause reads without guessing.

### `idle`, and the wake-up that must not be missed

The delicate part of this plugin is not in the protocol. A client that sends
`idle` just after something changed must return **immediately**, not wait
for the next change; a bare `Notify` loses that, because the notification
fires while the session is still reading its versions and composing its
request, before it has subscribed. So the shared state keeps a **monotonic
counter per subsystem**, the session remembers them before going to sleep,
and the wait begins with a **comparison**. It is that comparison that
forbids the missed wake-up; the `Notify` only spares a polling loop. Four
subsystems are named: `player` (playback, pause, stop, preset change,
position), `mixer` (volume or mute), `playlist` (the queue changed — since
the queue *is* the active source's presets, that means a source change) and
`stored_playlist` (the catalogue of sources or of their presets changed). An
`idle` naming only subsystems this server never emits waits **for ever**,
which is the correct MPD behaviour and not an oversight; a *misspelled*
subsystem is refused, because a client left silent for ever diagnoses far
worse than one that got an `ACK`.

### Album art

`albumart` and `readpicture` answer **exactly the same bytes**, and that is
not a shortcut. For MPD they are two different origins — a file *beside* the
track, versus a picture *embedded* in its tags — whereas this appliance has
exactly **one** cover per track whatever its origin, the core having already
arbitrated between the neighbouring file, the embedded tag and the network
(see [the cover chain](#the-cover-chain)). Distinguishing them here would
need information the display protocol does not carry, and M.A.L.P. tries one
then the other: answering only one of the two would make the cover depend on
which the client happens to try first. The single difference is MPD's own —
`readpicture` publishes a `type:` line.

The image is served **in chunks of 8 KiB**, which is MPD's own `binarylimit`
default and therefore the ceiling a client that never sends `binarylimit`
(all of them here, since the command is not handled) expects never to be
exceeded; serving 64 KiB to a client sized for 8 would be a buffer overrun
in its process caused by ours. `size:` is the size of the **whole** image
and not of the chunk, since that is what tells the client how many
round-trips remain. The bytes never travel through the session's text
accumulator — the amplification factor described under `MAX_REPONSE` has no
purchase on them — and the image itself is not counted per connection: it
lives **once** in the process, behind an `Arc`, however many sessions read
it.

Those bytes come from the cover frame the core pushes (see the console
section above). The plugin cannot fetch them itself: the `cover_href` a
state frame carries is a URL of the *core's* HTTP server, which the plugin
has neither the right nor the means to read. It is the only plugin in the
repository that overrides `wants_covers`, and that single line is what turns
cover pushing on for the whole appliance.

**The ceiling is 20 MiB, and its consequence is user-visible.** A cover above
that is not pushed, so it appears **in the SPA and not in MPD**: the web
route streams the file and never has to materialize it, while pushing on a
socket **forces** materialization — the bytes, their base64, the rendered
line. The value was raised from an initial 2 MiB because the owner keeps
album scans on his NAS that exceed 2 MiB, and the cost was measured before
accepting it: pushing a 20 MiB cover took the producing process's resident
high-water to about **97 MiB**, some 4.8 times the image, once per track
change, under a tenth of the device's 1024 MiB. (That measurement was taken
on a dev build under WSL, not on the Pi.) The encoded line is built **once
per publication** and shared between relays by `Arc`, so a second asker for
the same cover — a second subscribed display, or the same one returning to a
cover it has already seen — costs nothing measurable: the peak drops by about
22 % from the second ask onward, and stays flat however many follow. Beyond
the ceiling it is not a cover but an accident: the 150 MB PNG on a share that
the core's HTTP route names as a real case would cost half the machine.

The limit is checked on the **file's size, before a single byte of content
is read** (on `metadata`), and the reason is worth keeping: a file size
requires **no knowledge of the format** — no header to interpret, no
decoder — so it is indifferent to JPEG, PNG, WebP or whatever comes next. A
`take` before the read stays in place as a net, in case the file grows
between the stat and the read. Exceeding the limit is a **refusal, not an
allocation error**: no frame is emitted, the display simply has no image,
the same silent-failure policy as cover fetching itself. Re-encoding or
generating thumbnails was considered and **rejected**: only JPEG has a cheap
reduced decode, the input is not necessarily JPEG, and the feature would
become a multi-decoder project. It stays possible later, and its place would
be the core's `cover.rs`, where the SPA would benefit too. A dedicated
range-request socket was considered and rejected the same way: the ceiling
covers the real cases, and the pattern keeps its own value for the day a
*second* display wants covers.

One rigour that is easy to mistake for a bug: **the requested URI is checked
strictly against what is playing at that instant**. `currentsong` publishes
`file: ritornello://<source>/<index>`, so `albumart ritornello://radio/17`
means "the cover of whatever preset 17 is playing *now*" — a URI whose
content changes underneath it, which never happens in an ordinary MPD where
a URI is a file. Serving anyway would hand the client the **wrong** image as
soon as its request is one track late, and the damage would be durable,
since clients cache the cover **under the URI they asked for** (M.A.L.P.
does): the poisoned entry would never be invalidated. So a mismatch is
refused, and so is a mismatch of the `href` between the current state frame
and the cover held — the core sends state first and cover second, so that
window really exists. The refusal is transient and repairs itself: a cover
change wakes `player`, and the client asks again.

### `pause 0` / `pause 1`, and why there is no `SetPause`

The protocol's pause is `Command::PlayPause`, a **toggle**, while MPD's
`pause 1` is absolute. The plugin bridges the two with an **optimistic local
state**: right after pushing a command it records the only two effects it can
predict — `PlayPause` flips playing↔paused, `SetVolume` sets the volume —
and `status` reports *that*, so a client that sends `pause` then `status` in
the same breath does not read the state from before its own command and see
its button fail to move. The next frame from the core is the authority and
overwrites it. `pause 1` therefore emits **only if the state differs from
the target**, which closes the remaining race: a client that repeats `pause
1` having missed the confirmation must not resume playback. Stopped, `pause`
emits nothing at all whatever its argument, since `PlayPause` would start a
playback neither the plugin nor the source knows the what or the where of —
though a malformed `pause 2` is still an `ACK` even when stopped, argument
validation coming before that guard.

Nothing else is guessed. Predicting what a `Select` does to the position,
the track or the preset would be wrong more often than right — the active
source decides that, and it alone. A slightly late `status` is benign; a
`status` that invents a track is not.

**No `Command::SetPause` was added**, and that is a decision rather than an
omission. It would weigh down a protocol shared by every input plugin with a
variant only a network client would ever emit, and it would ask the core and
every source for a guarantee they do not have — pausing a live stream is not
something a source necessarily honours. The optimistic layer buys the same
client-visible behaviour exactly where it was needed, in the client's own
`status`, without asking anyone to promise anything.

### Not yet verified on hardware

Like the rest of this chantier, **none of this has ever run on the
device**: the test suite covers it, hardware does not. Two behaviours in
particular are open rather than settled, and are the first things to try on
the Pi:

- **disabling a source while an MPD client is connected.** Turning a plugin
  off removes it from the catalogue, which is republished, so the client's
  `listplaylists` should **shrink** and its `stored_playlist` wake up should
  fire. The path is there and tested in Rust; nobody has watched a phone do
  it.
- **a cover arriving while a permanent status is on screen.** Nothing read in
  the code says the two interfere — a cover landing republishes the state,
  and that frame carries the remembered status unchanged — but the
  combination has never been watched on the device, and it is on the list
  precisely because a status that vanished the moment a cover arrived is the
  kind of defect only a screen shows.

## Now-playing metadata (the `metadata` kind)

A `metadata` plugin enriches what the active Source is playing **without
the Source knowing**. The core tells it what is playing, it answers with
what it knows about it.

**The current track is a partial state, completed by layers rather than
overwritten by them.** `NowPlaying` carries `known` — what is already
known of the track (artist, title, album, duration, release year, plus a
boolean saying whether a cover is already held) — so a contributor can see what is
missing and either fill it in or abstain, instead of blindly declaring
everything it knows and letting the freshest answer win. Three layers
feed it, from least informed to most:

1. **What the stream announces itself.** The core watches mpv's
   `metadata` property and reads the ICY header (`icy-title`), displayed
   **raw**, without splitting on `" - "`: the convention exists but is not
   guaranteed — OUI FM's webradios actually emit `Title - ARTIST`, in the
   reverse of the usual order. This layer works without any plugin, and
   without the Source having to declare anything.
2. **What the file itself carries.** From that very same `metadata`
   property, the core also reads a played file's tags — `title`, `artist`,
   `album`, `date`. FFmpeg normalises the keys, so ID3 (mp3), Vorbis
   comments (flac, ogg, opus), iTunes atoms (m4a) and RIFF INFO (wav) all
   surface under those four names; `date` yields the release year — read
   from the string's **leading digits**, four of them being a year and
   eight a compact `YYYYMMDD` whose first four are kept, any other length
   being discarded (which is what keeps a five-digit number from becoming
   a plausible-looking year). Shown with origin `tags`. Like ICY, this layer
   works **without any plugin** and serves *any* Source that plays a
   tagged file — nothing has to be declared for it.

   Two rules are worth knowing. The core picks **four named keys** rather
   than absorbing the object: an m4a also carries container keys
   (`major_brand`, `handler_name`) that have no place on a screen. And the
   layer stays silent as soon as **any `icy-*` key is present**: some
   stations fill in a `title` holding their own name next to an
   `icy-title` carrying the actual track, so preferring the former would
   replace the song by the station name. Stream and file tags therefore
   never coexist.
3. **What a `metadata` plugin has learned**, if it matches what is
   playing — and, since `known` travels alongside the identity, if
   there is anything left worth answering at all.

**Each contributor declares its own intention: overwrite, or merely
complete.** `Enrichment` carries `fill_only`, whose default is `false` —
"overwrites". That default is exactly what preserves the rule this
project has always had: **a plugin still outranks ICY and file tags in
all circumstances**, as long as the station does not change, because
overwriting is the default and a plugin is the last layer to speak. What
used to be stated as a rule in its own right is now a *consequence* of
that default — and a deliberately chosen one, since it is what lets all
three bundled plugins keep exactly the precedence they have today without
a single line of theirs changing (only `musicbrainz` declares
`fill_only`, and only on the path where it does not already know what is
playing — see [the cover chain](#the-cover-chain) below). The reason a
plugin outranks tags too is the same one that puts it on top at all — a
plugin fetches what the file cannot say (an online database, a separate
feed), so letting the file overrule it would discard the more informed
answer. These streams' ICY is of lesser quality — reversed order (`Title
- ARTIST`), sometimes just the station name as filler — and letting it
take over on every track made the display change shape twice per track.

**The first contributor that overwrites supplies its whole block, holes
included; a `fill_only` contributor only fills whatever field stayed
empty.** There is deliberately no field-by-field composition between two
contributors that both overwrite: mixing one's artist with another's
album would put two different readings of the same stream on screen at
once and display a track that does not exist. A `fill_only` contributor
never runs that risk in the first place — it only ever writes into a
field nobody has touched yet.

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

**Declaration order still matters, but only to break a tie**, and this
remains the only kind for which it matters at all: between two plugins
that both *overwrite* the same track, the first one declared in
`plugins.toml` wins, and a plugin declared lower down never overwrites
it. The chosen criterion is predictability for whoever is debugging:
"first to arrive" would depend on network latency, so the same
installation would display different things from one boot to the next.
A `fill_only` plugin is never party to that tie: it only ever writes into
a field an overwriting plugin left empty, whatever its own position in
the file.

**A Source can declare metadata too, on its own channel, without becoming
a `metadata` plugin.** The channel already exists: `SourceMessage`
accepts `id: None` as a spontaneous notification, and `SourcePlugin`
already has `poll_notification` for it — the same mechanism the radio
plugin uses above to announce a fresh `preset_count` outside the
request/response cycle of `Play`. A Source's own declaration reaches
`known` the same way, which matters whenever finding it takes time: see
[the cover chain](#the-cover-chain) below for how `files` uses exactly
this to announce a `folder.jpg` without making playback wait on an SMB
`readdir`.

**Updating an existing installation.** `deploy/deploy.sh` installs the
new binaries and appends the missing `metadata` plugin entries to an
existing `/etc/ritornello/plugins.toml` (see [Declaring the
plugins](#declaring-the-plugins)), so a device already in service keeps
its CD track titles — which the cd plugin used to provide itself before
this version — and gains the Radio France ones. They are appended at the
end, hence last to break a tie should one ever arise; since the three
answer for disjoint content, that costs nothing. Reorder the blocks by
hand if you add a plugin that overlaps one of them.

A `plugins.toml` completed by an older version of the script, which only
ever provisioned the file, is brought up to date by the next deployment
without anything being lost: the entries already there are not touched.

### The three bundled plugins

- `ritornello-plugin-musicbrainz` recognizes a disc through MusicBrainz.
  This is the code that used to live in `ritornello-plugin-cd`, where a
  multi-second network call shared the process that had to answer track
  commands. The release year it reports is the **album's first release**
  (`release-group.first-release-date`), the recognized pressing's own
  `date` serving only as a fallback: measured, a 1987 pressing of a 1959
  record carries `date: "1987"`, and it is the record's year a listener is
  after. The lookup already asks for `release-groups` — the same block that
  resolves an album-level cover — so this costs no extra request. It also splits a radio's single metadata string into artist and
  title, learning each station's format — see [Splitting the ICY
  string](#splitting-the-icy-string) below, which is where its one variable
  (`RITORNELLO_MUSICBRAINZ_STATE`) and its admin page are described.
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
- `ritornello-plugin-radiofrance-metas` reads the *live* endpoint of Radio
  France's stations. **Nothing to configure**: the table of the 74 stations
  is embedded in the binary (`src/stations.toml`) — the six national brands,
  FIP's 12 webradios, France Musique's 11, and the 45 `ici` (ex-France Bleu)
  local stations.

  This is the plugin with the strongest case of the three: Radio France's
  streams carry **no ICY metadata at all** — no `icy-metaint` header, not even
  filler text — so without it a Radio France station on the device shows
  nothing. The endpoint states itself when to be called again
  (`delayToRefresh`), so the polling rate is the server's own, not ours.

  The last segment of the live URL is not the station but a **rendering
  profile**, and the wrong one makes the plugin silent: measured on Mouv' at
  one instant, `webrf_fip_player` answered `Le direct / Mouv'` — the station's
  baseline — while `webrf_mouv_player` answered `La Playlist / SOOLKING - Bye
  Bye (feat. TAYC)`, which was what actually aired. Each station therefore
  carries its profile in the table. Two are in use: one returns the **song**
  (title and artist already separated, and the time window is the song's, hence
  a duration), the other the **programme** (its name, plus what is playing
  inside it as a single `ARTIST - Title` string, and no duration — the window
  is the programme's, an hour on Mouv'). `position_s` follows the exact same
  filter as `duration_s`, computed from that same `startTime` when it is a
  song and left out when it is a programme (see the general `metadata`
  contract above) — the programme's own hour-long window would make for a
  position as meaningless as its duration. Outside music the second one carries
  the programme and its detail, and they are displayed too: there is no ICY to
  fall back on, so the alternative is a blank line.

  The **album** comes from a third place, the station's schedule, where the
  current song is matched by its identifier. It is fetched **once per track**,
  never per refresh, and it is best-effort: that schedule is frequently one
  track behind, so the album comes and goes on the same station — observed
  present on FIP, most of its webradios, France Musique's, Mouv' and France
  Inter, absent on the local ones. When it is missing the enrichment goes out
  without it, and after five consecutive tracks without an album the plugin
  stops asking that station's schedule rather than doubling its requests to a
  third party for nothing. It fills `morceau.album`, which a display is free to
  show — the bundled console gives it its own line when the source names no
  preset.

  Recognition is based on the **mount** of the stream, matched as a *token* of
  the URL (bordered by non-alphanumeric characters), not as a raw substring:
  `fip` is a prefix of `fipgroove` and `francemusique` of
  `francemusiquebaroque`, so a plain substring search would have made the first
  entry capture the others and display the wrong station's titles, with no sign
  of error. The token rule allows a single entry per station covering every
  form the same station is served under: `icecast.radiofrance.fr`, the
  historical `direct.fipradio.fr/live/` name — the one directories reference —
  and the `stream.radiofrance.fr` HLS playlist, whatever the quality suffix.

  The table's provenance is Radio France's own: the Open API documentation
  publishes, in a single object per station, both the `liveStream` (hence the
  mount) and the `playerUrl` carrying `id_station=<n>`. That covers 61 of the
  74; the remaining 13 (France Musique's webradios, FIP Sacré français, FIP
  Cultes) come from the site's own webradio cards, where the brand slug and the
  identifier sit on the same card, and each of their mounts is verified against
  the Icecast server at every run. `scripts/fetch-stations.mjs` regenerates the
  file from those sources (with `--verifier`, it reports a drift without
  writing anything).

  The optional `/etc/ritornello/radiofrance-metas.toml` file
  (`RITORNELLO_RADIOFRANCE_METAS` variable, example in `deploy/`) works exactly
  like OUI FM's: consulted **before** the embedded table, to fix an entry gone
  stale or add one without recompiling.

  The plugin's own
  [README](../crates/ritornello-plugin-radiofrance-metas/README.md) lists every
  station covered — mount, identifier and profile — along with what is
  deliberately left out and why. Its table is regenerated by the same script,
  so it cannot drift from the embedded one.

### Splitting the ICY string

A radio announces what is playing in a single text field: Shoutcast/Icecast's
`StreamTitle`. In practice `Artist - Title`, sometimes `Title - ARTIST` (OUI FM
does this), and often nothing usable at all — a station name during ads, a show
name on talk radio, an empty string. The core deliberately does **not** split
it: splitting is guessing, and a guess you display is a lie.

This plugin can do better because it can **check**. It forms a hypothesis, tests
it against MusicBrainz, and stays silent when it does not hold. That is not a
reversal of the core's decision but a respect for its reason.

**The format is a property of the station, not of the track.** A station that
emits `Title - ARTIST` will do it for every track it plays, so the plugin probes
once per station, keeps the winning pattern, and afterwards splits **locally** —
no network call is needed to separate artist from title once the pattern is
known. Only the cover still costs a request, and that request doubles as the
ongoing check.

The key is the stream's absolute URL, which the protocol already carries: a
radio declares its identity as `{"kind":"stream","url":"…"}`, and the plugin
receives it in every frame. Not the preset number — those are per-source,
remappable, and the same station can sit at two of them.

To see the raw string at all, the plugin needs `Known::stream_title`, which
carries what the stream itself announced, unsplit and unarbitrated. It is not a
duplicate of `title`: `title` is the outcome of arbitration between
contributors, and once this plugin has overwritten it, `title` is the plugin's
own output. A radio's identity does not change between tracks, so the core's
staleness guard expires nothing and a new `StreamTitle` does not clear
enrichments — without the raw field, the plugin would split exactly once per
session and then go blind.

**What is remembered**, one entry per probed station, in
`/var/lib/ritornello/plugin-musicbrainz.json` (`RITORNELLO_MUSICBRAINZ_STATE`,
written atomically): the pattern — a separator and an order, or "do not split" —
its origin (standard confirmed, learned deviation, or manual), the last time it
was used, and how many titles it has split. That count is not decoration: a
pattern with two hundred hits and one with a single hit do not deserve the same
trust when you decide which to delete.

Stations that follow the standard format get an entry too, rather than being
represented by its absence. Absence would conflate "never probed" with "checked,
and it conforms", and an explicit entry is needed for "do not split" anyway —
otherwise a talk station would be re-probed on every title, forever. The
"exceptions only" view is a filter on the admin page, not a hole in the file.

**How a candidate is accepted.** The raw string is cleaned first (anything after
a `|`, a trailing bracketed group, repeated spaces) — before splitting, because a
station that appends its own name would otherwise fail every candidate and be
filed as "do not split", which is permanent. Candidates are then derived from
the separators actually present, both orders, capped at four. Each is checked by
a recording search, and accepted only if the score clears a threshold **and** the
returned title equals the candidate's after normalization. That second condition
carries the weight: the search almost always returns something plausible, so a
score alone would accept anything. Among accepted candidates the **best** score
wins, not the first — the reversed order of a real pair often also clears the
threshold, with a lower score, and comparing them is exactly what tells the two
apart.

**When a pattern is revisited.** Three *consecutive* validation failures, not
one. A track MusicBrainz does not know is a perfectly legitimate failure on a
correct pattern; re-probing on the first would probe on every obscure title and
could replace a good pattern with a lucky one. A station filed as "do not split"
is never re-probed automatically — deleting its entry from the admin page is the
intended remedy, and that is what the delete button is for. A manual pattern is
never overwritten by re-learning.

Launching a re-probe **consumes** the counter, so a re-probe buys three more
tracks rather than arming itself permanently. Without that, a station that never
validates — a stream in mojibake, say — would re-probe on *every* title for the
life of the process, and the limit described just above would become a
guaranteed request storm.

**And a failed validation still says something.** The plugin does not go silent:
going silent would leave the *previous* track's answer winning the arbitration,
since a radio's identity does not change from one track to the next, and the
screen would announce the artist, title and cover of the track before for the
whole duration of the next one. So on a failure it emits what it still knows:
the locally split pair when the pattern applies — MusicBrainz not knowing a
track says nothing against a split already confirmed on that station — without a
cover, for want of a release to cite; or, when the pattern no longer applies, the
cleaned announced string as the title and no artist at all, which claims no split
and shows only what the stream says, stripped of its own advertising.

**Where it must be declared.** After the station-specific `metadata` plugins.
`metadata` plugins are arbitrated by declaration order and the winner's block is
taken whole, so a `musicbrainz` declared before `ouifm-metas` would win on the
very stations that plugin knows better. This matters more on the ICY path than
elsewhere, because that path **overwrites** (it has validated its guess) where
the cover path merely fills in. `deploy/plugins.example.toml` says so on the
spot.

**Its admin page** lists the stored patterns, filtered to the exceptions by
default, and lets you delete one, clear them all, or set a pattern by hand. The
edit field is a closed set — a separator, an order, or "do not split" — and
deliberately not a regular expression: a free-form pattern would make you debug
regexes, and a bad one would break every title on that station. A hand-set
pattern is persisted and marked manual, so re-learning leaves it alone.

Two known limits. Mojibake — a station emitting latin-1 where UTF-8 is assumed,
or the reverse — never validates, and looks like a bad split when the split was
right; the log names it separately so the search is not led astray, but nothing
repairs it. And a `metadata` plugin never receives `SetLocale` (that frame
exists only for sources), so this page's language is fixed at plugin launch and
a language change shows up only after the plugin restarts — the same limit as
the MPD plugin's page.

### The cover chain

Five contributors can resolve a cover, and the order among them is not a
list of priorities written anywhere — it falls out of the layers and
intentions described above.

1. **`files`** — a `folder.jpg` (or one of its aliases) sitting in the
   file's own directory, announced as a `CoverRef::Path` **on the
   Source's own channel** (see above): `files` stays a plain Source, and
   sends this as a notification once its directory listing has resolved,
   rather than as part of its answer to `Play` — resolving a folder can
   mean a `readdir` on an SMB share, and playback must not wait on it.
2. **The core** — a cover embedded in the file itself, read with
   `lofty` once mpv names the path being played (the core never reads
   the identity for this: it has made a principle of never interpreting
   it). The core **completes** here rather than overwrites, which is
   what gives the folder image its precedence without inverting any
   convention: extraction is only attempted while the slot is still
   empty.
3. **`radiofrance-metas`** — the station's own image for the track,
   overwriting, from the same live feed it already reads for the text.
4. **`ouifm-metas`** — the stream's own cover, overwriting, from the
   same SSE feed — a ready-made URL when the feed carries one, else one
   composed from an identifier the same way the station's own player
   composes it.
5. **`musicbrainz`** — the generic resolver. On a disc it already
   recognizes by its table of contents it overwrites, using the release
   identifier its lookup already carries. Everywhere else it
   **completes**: given both an artist and an album, and only while no
   cover is held yet, it searches the release and asks the Cover Art
   Archive.

Two things about steps 3 and 5 are worth spelling out, because neither is
guessable from the general model:

- **`radiofrance-metas` stays silent when `songUuid` is null.** The
  station serves a **generic antenna image** for "Le direct" and its
  talk programmes — the same picture whatever is airing. Announcing it
  would fill the cover slot and silence the generic resolver behind it,
  since no higher layer can tell a filled field from a filled-but-wrong
  one: once a contributor has declared a cover, that slot is considered
  settled.
- **`files` discards a lone image named like a back cover.** Only the
  rule that *guesses* — the one that picks the sole image in a directory
  when none matches a known name — consults that exclusion list; a
  directory naming both `front.jpg` and `back.jpg` is already settled by
  the preference list before a guess is ever needed. Without the
  exclusion, a lone `back.jpg` or `Scan_verso.png` would be shown as the
  album's face; with it, `files` says nothing and the generic resolver
  gets its turn instead.

What this produces in practice, for a file on the NAS:

| What there is | What's displayed | Who |
|---|---|---|
| a `folder.jpg` | the `folder.jpg` | `files` |
| no `folder.jpg`, an embedded cover | the embedded one | the core, which completes |
| neither, but usable tags | Cover Art Archive | `musicbrainz`, which completes |
| neither, and no tags | nothing | — |

And for a radio: the station's metadata plugin overwrites the ICY and
supplies the station's own image whenever it names a genuine track;
lacking one, `musicbrainz` completes from the very artist and album that
plugin just supplied.

**An outage is retried later, not merely "not remembered".** `cherche_release`
already retries three times in-process (2 s then 4 s), and a failure past that
is deliberately not memoised — a 503 must not turn into "this album has no
cover". But not memoising is only half an answer: the note said "the next frame
will retry", and there is no next frame. The core republishes `NowPlaying` only
when the identity or the `known` block changes (see `publie_etat`), and on a
local file both settle the moment the tags are read. The owner's report has
exactly that shape: nothing for ten seconds, then the cover appears **on the
next track** — the only event that relaunched anything. The plugin therefore
schedules its own deferred retries, twice, at 20 s and 60 s. Two and no more:
past that the absence is no longer a transient outage, and hammering a free
third-party service for one image would be abuse — a track change remains the
last resort, as before. Only the pair still being targeted is retried, and the
in-flight marker stays armed across the wait so a frame arriving meanwhile
cannot start a second search for the same album. The decision (is a retry due,
and after how long) is a separate, pure function from its execution, which is
what makes it testable without a clock or a network.

**Why there is no cover is now readable in the log**, and that was the whole
problem: every way a cover could fail to appear was silent, so the same screen —
a ♫ — meant four different things and none of them could be told apart after the
fact. Each step now says what it did:

- the `files` plugin, on the folder beside the track: "cover file found in …",
  "no cover file in …" (including when the answer is the remembered one, since a
  directory memoised as coverless stays silent forever otherwise), or a **`warn`**
  when the share did not answer at all — the circuit breaker giving up is an
  incident, not an absence;
- the core, on the embedded picture: "no embedded cover in …" against a **`warn`**
  for a share that timed out. Those two used to be flattened into one `None`,
  which is precisely why they could not be told apart;
- `musicbrainz`, on its own search: when it starts, and on the answer, "cover
  found" / "no cover" / "unavailable" **with the elapsed time**. Between the
  throttler, three in-process attempts and their ten-second timeouts, tens of
  seconds can pass — that number is now in the log instead of being guessed from
  the screen;
- the core, when the bytes finally land: the fetch duration, then "cover …
  published". Those two close the timeline, which is what "it turned up much
  later" needed to become a measurement.

Two refusals are `warn` rather than `info` because they are broken promises: a
key requested by the browser that the cache no longer holds (`ENTREES` entries,
FIFO), and a cover evicted between its retrieval and its publication. Both mean
the appliance had the image and lost it.

**`GET /api/cover/{key}`** serves the bytes. It is **the appliance** that
fetches an image, never the browser — the same principle already stated
for admin pages ("the page loads no external resource") — which also
covers the one case a browser could never handle unaided: a cover
embedded inside an audio file. A cover is only published once its bytes
are actually in hand, so the page never receives the URL of a broken
image: a 404 from the Cover Art Archive, common since many releases have
none, becomes silence rather than an empty frame.

The cache is a small table, **in memory**, and it does **not** survive a
restart. It is bounded by memory, not by a count: `cover_cache_budget_mio`
(8 to 256 MiB, 50 by default) is charged against the bytes of covers
downloaded from the internet and the thumbnails kept ready to serve, while a
local cover — a file beside the track, or one embedded in it — keeps only a
path and so costs nothing against this budget. A count-bounded cache used to
force the user to multiply three settings to learn what it cost, and the
product could reach values in the hundreds of megabytes without anything
objecting; a memory-bounded one cannot. Past the budget, eviction proceeds by
bytes, and a cover already published in `cover_href` can answer 404 — the page
falls back to its ♫ for an image the appliance had and lost. The config page
shows the number of covers this implies: at least `cover_cache_budget_mio` /
(`cover_download_max_mio` + thumbnail ceiling) in the worst case, where every
cover comes from the internet and pays both its bytes and its thumbnail, and
about `cover_cache_budget_mio` / thumbnail ceiling for a library of local
covers, which pay only their thumbnail — with the defaults, at least twenty
and about a hundred. This is a deliberate rejection of a
disk cache, not an oversight: a radio changes track every few minutes, a
disc's cover is already remembered per-disc inside `musicbrainz` itself,
and a local file's cover is reread from disk in a fraction of a
millisecond — a disk cache would buy almost nothing here, while adding a
directory to provision, a size to police, and one more piece of state to
reason about when something looks wrong.

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
back its own identity and status, since the core cannot modify an identity
it has made a principle of never interpreting. Display and metadata therefore
follow the advance without any key being pressed. The **end of the disc**
follows the same principle in reverse: the core signals the stop to the
Source, which realigns its state — without which the last track would
stay displayed indefinitely.

### Writing a `metadata` plugin

Implement the SDK's `MetadataPlugin` (`now_playing` / `next_enrichment`),
chain it onto a `Runtime` and `run()` it:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    ritornello_plugin_sdk::Runtime::from_args()?
        .metadata(MonGreffon::new())?
        .run()
        .await
}
```

`Runtime::from_args()` reads the three arguments the core launches every
plugin with (`--register`, `--name`, `--socket-prefix` — see [Declaring
the plugins](#declaring-the-plugins)). Each kind-specific method
(`.metadata()`, `.source()`, `.display()`, `.input()`, `.admin()`) binds
that kind's socket the moment it is called and returns a `Result<Self>`,
hence the trailing `?` on every one of them: a bind failure surfaces
immediately, not once `run()` starts serving. A single binary can chain
**several** of these methods — nothing stops a plugin from announcing
`metadata` alongside `input` or `display` — and the announcement `run()`
eventually writes describes exactly, and only, the kinds that were
actually chained in. Two points of contract for `MetadataPlugin` itself:

- the **identity** of what is playing is an **opaque** JSON produced by
  the Source, which the core only compares and relays. The radio plugin
  puts `{"kind":"stream","url":…}` there, the cd plugin
  `{"kind":"disc","toc":…,"track":…}`. A plugin that does not recognize
  the shape it receives simply stays silent;
- every enrichment must **echo back the identity** it concerns. This is
  the staleness guard: the core discards one that no longer matches what
  is playing, which prevents a slow answer from overwriting the next
  track. An enrichment whose text fields are all empty counts as a
  non-answer, and therefore lets a lower-priority plugin win — among
  those that overwrite; a `fill_only` plugin never competes on priority
  in the first place (see [Now-playing
  metadata](#now-playing-metadata-the-metadata-kind)).

Two optional fields carry what a listener would look up next: `year`, the
release year (a plain number; the core re-checks its range on arrival),
and `links`, a list of listening-platform links. `links` is **not a free
URL field**: each entry is `{"platform": "youtube" | "deezer" |
"apple_music", "url": …}` and the core accepts an entry only if its URL is
`https`, carries neither port nor user info, and names exactly one of the
hosts registered for that platform in `ritornello-proto` (equality, never
a prefix or suffix — `evil-youtube.com` and `youtube.com.evil.example` are
both refused). A link that fails the check is **dropped silently, entry by
entry**, the rest of the enrichment surviving; a plugin that wants its
links shown therefore emits the platform's canonical hosts, and adding a
platform is a change to the protocol crate, on purpose — the host list is
the security boundary that keeps a third party from making the appliance
render a clickable link to a domain of its choosing. Winner-takes-all
applies to them as to the text: whoever comes first in the manifest order
and carries one wins, and a later contributor only fills a year or a link
list that is still empty. Unlike the text, though, these two fields are
filled from **every** retained enrichment and not only from the `fill_only`
ones — they describe the track, not one reading of it, and a plugin that
overwrites but carries nothing but a year or a link would otherwise have
its answer dropped in silence: it is exempt from the "entirely empty"
refusal at the door, yet it cannot become the retained text block (it would
wipe the title the tags or the ICY were showing).

One more field of `Enrichment` needs attention from a plugin only if it can
answer it: `position_s`, an elapsed number of seconds **in the track, at the
instant the plugin sends it** — not a timestamp, so there is nothing to
synchronise between the plugin's clock and the core's. The core anchors it
the moment it arrives and advances it on its own between two answers, at the
one-second pace described in [interface.md](interface.md); a plugin that
only calls its source every few dozen seconds does not need to answer any
faster than that to keep the figure moving on screen. It is discarded by the
same identity check as the rest of the enrichment, position included, so a
plugin need not track staleness for it separately — and only the plugin
currently winning the arbitration gets to anchor it: one held in reserve
answering with an unrelated correction (a fixed title, a late cover) must not
be read as fresh progress, or the bar would jump backward the instant it
spoke. `radiofrance-metas` is the only one of the three bundled plugins that
fills it, computed from the same `startTime` it already reads for
`duration_s`; the other two need no new logic for it — they already write
out every field of `Enrichment` by name, so the addition is one more line
reading `None`, not a decision to make.

`next_enrichment` must be **cancellable without loss**: its future is
dropped as soon as a `NowPlaying` arrives, so any durable state (open
HTTP connection, cache, queue) must live in the plugin, not in the
future's local variables. (The same requirement holds for the Sources'
`poll_notification`, and for the same reason — see the SDK docs.)

## A plugin's UI

A plugin gets its own admin page by chaining `.admin(page)?` onto its
`Runtime` before `.run()`, alongside whatever kind(s) it already serves
(the two are independent: `.admin()` only adds a flag and a socket to
the same announcement) — for instance a plugin serving both `input` and
`display`, plus an admin page:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    ritornello_plugin_sdk::Runtime::from_args()?
        .input(MesEntrees::new())?
        .display(MonAffichage::new())?
        .admin(MaPage::new()?)?
        .run()
        .await
}
```

`.admin()` binds `{prefix}-admin.sock` at the call site, exactly like
every other kind-specific method, which is what lets the resulting
announcement (`{"name":"...","kinds":[...],"admin":true}`) truthfully
say whether a page exists — no single line of the core changes to
support it. The admin half then answers the requests of the admin
protocol:

- `GetAsset("ui.js")` → an **ESM module** exporting `contract` (the
  contract version, see `web/kit/src/contract.ts`) and, as default, a Vue
  component;
- `GetAsset("ui.css")` → the module's stylesheet (its own Tailwind pass,
  important: the core's CSS only contains the classes the core sees). The
  shell injects it into a **cascade layer of its own, `greffon`, declared below
  `utilities`** by `web/app/src/app.css`, through a
  `<style>@import url(…) layer(greffon)</style>` — the only way to put an
  *external* sheet in a named layer, which also makes it work for a third-party
  plugin whose CSS nobody here builds. That is a fix, not tidiness: both passes
  used to write into the same `utilities` layer, and the plugin's sheet —
  injected later and deliberately left in place — won on equal specificity.
  `InputAdmin.vue` carries `class="hidden"` on its file input, so
  `generic-input/ui.css` contains `.hidden{display:none}`, which overrode the
  `md:flex` of the shell's own top navigation: visiting that page made the menu
  disappear for the rest of the session. Any class both passes emit carries the
  same declarations (same theme), so losing changes nothing visible; what
  changes is that a plugin can no longer undo the shell's layout. A Playwright
  journey locks it, because the defect lives in the cascade of two really
  served sheets, which jsdom does not compute;
- `GetCatalog` → its flat i18n catalog, which the view consumes through
  `t()`;
- `GetData` / `SetData` → the page's data, opaque JSON both ways;
- `Ping` → `Pong`, without touching the plugin's state or taking any lock.

**The protocol is concurrent.** `serve_admin` spawns one task per request
and a single writer for the socket: responses leave in the order they
*complete*, correlated by `id`, not by arrival order. Historically it was
serial (read, await, write, read again), so a `set_data` mounting a
sleeping network share held back `ui.js` — a plain `include_str!` — until
the core gave up, and the admin page simply vanished. The plugin now sits
behind an `RwLock`: `asset`, `catalog` and `get_data` read in parallel,
`set_data` is exclusive (legitimately: it is a write). Assets are cached
by the SDK the first time they are seen, and `ui.js`/`ui.css` are read
before the socket accepts, so they never wait behind a write.

**Each request carries a budget** (`deadline_ms`), decided by the core
from the request's nature — an in-memory asset does not get the budget of
a network mount: `Ping` 500 ms, `GetAsset`/`GetCatalog` 1 s, `GetData`
5 s, `SetData` 30 s. The SDK enforces it server-side, **lock wait
included**, and answers `Expired` at the deadline instead of going silent;
the core maps `Expired` and silence alike to `AdminIpcError::Timeout`,
rendered as a `504` with a catalog message, while a closed socket is a
`502`. `/api/status` pings every admin page and reports `busy: true` for a
plugin whose ping expires — alive and wired, but held up.

**What the budget does not absorb.** `tokio::time::timeout` drops the
future at its next `await`, so an interrupted `set_data` releases the
lock — but a blocking syscall inside `spawn_blocking` runs to completion.
A plugin that touches a network path therefore still has to run it off
the async thread and behind a circuit breaker
(`crates/ritornello-plugin-files/src/sante.rs`); the protocol bounds the
*wait*, not the syscall.

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
  guarantee, the displayed URL is not one. The bundled modules all declare
  `base` **required**, with no default value: the name a plugin is served
  under comes from `plugins.toml`, hence from the deployment, and a
  module that rebuilt `/plugins/<its-name>/` would be wrong — silently —
  as soon as an operator declares it under another name.

The module imports `vue` and `@ritornello/ui` **without bundling them**:
the shell provides them through an import map, so a single Vue instance
and a single set of components serve everyone. An incompatible contract
is reported in the interface rather than breaking the page.

Native ESM requires no build step: a simple plugin can ship a
**hand-written** `ui.js`. The four bundled plugin pages (radio, files,
generic-input, mpd) use a Vite build (see
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
