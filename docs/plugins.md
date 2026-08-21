# The plugins

Plugin architecture: the core (`ritornello-core`) orchestrates plugins —
separate processes communicating over Unix sockets (line-delimited JSON
protocol) — of four kinds: **source** (content to play: radio, CD, audio files),
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

## Declaring the plugins

`plugins.toml` is not user data — it is the list of which installed
binaries the core launches — and `deploy/deploy.sh` treats it as such. On
a device with no such file, it provisions `deploy/plugins.example.toml`
whole. On a device already in service, it **completes** the file: the
blocks of the reference list whose `name` is not already declared are
appended, comments included, and the script prints the names it added.

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
  `deploy/plugins.example.toml` **and** from the `PLUGINS` list in
  `deploy/deploy.sh`, which the script requires to name the same set and
  refuses to run otherwise. The core has no `enabled = false`; a plugin
  it is told to launch, it launches.
- An appended `metadata` entry lands **at the end** of the chain, hence
  last in priority (order matters for that kind only — see [Now-playing
  metadata](#now-playing-metadata-the-metadata-kind)). The bundled ones
  answer for disjoint stations, so the position is harmless today; move
  the block by hand if you ever add two that overlap.

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
grid show only the numbers that exist and reach past nine through its own
`<`/`>` page arrows — the remote's bare digits alone only reach 1-9. The
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
| `cifs-utils` | mounting an SMB share | A network source is declared but never mounts: the plugin reports `mount`'s error and the source stays "not mounted". Folders of the device are unaffected. |
| `smbclient` | browsing a share **before** mounting it | The network wizard is greyed out and says so. A share is still declared by entering host, share and subfolder by hand. Nothing else is affected. |

```sh
sudo apt install cifs-utils smbclient
```

The plugin **probes** for `smbclient` at startup and re-exposes the answer
as `can_browse_smb`. The page greys the wizard out rather than failing on
click, the same way the System tab greys out rebooting when logind refuses
it. The probe is redone on every connection attempt: installing the package
without restarting the service gives a correct answer, not a stale refusal.

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

That matters because the admin protocol is **serial** and the core abandons a
request after 5 s. One blocking `stat` is enough to wedge the plugin, page
included. So every filesystem access a request triggers goes through a circuit
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
itself. A failure does not undo the declaration — everything just entered
would be lost to a sleeping NAS — it is reported on the page, with a retry
button.

### Track lengths

Only tracks **in the playlist** are measured, and only those whose length is
still unknown. A scanned folder carries none — an `#EXTINF` line in an m3u is
the only thing that does — so the column used to show a dash forever.

Lengths are read from the file **header**, in-process, and the choice was
measured on sixty files: 0.33 ms each that way, against 42 ms with one
`ffprobe` per file. Over two thousand tracks that is under a second instead of
a minute and a half, and it spares a Raspberry Pi two thousand process spawns
while music is playing. No system package is involved.

The work runs **in the background** and is polled by the page, like the
recursive scan: the admin protocol has a 5 s ceiling and a playlist coming off
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
`ritornello-plugin-mce`): `deploy/deploy.sh` removes the old
`ritornello-plugin-mce` binary from the target, so it does not keep
running after an update, and appends the `generic-input` entry to
`/etc/ritornello/plugins.toml` if it is absent. What it does **not** do
is delete the old entry — it never removes anything (see [Declaring the
plugins](#declaring-the-plugins)) — and that entry now names a binary
that no longer exists, which the core reports at every startup. Delete
the `name = "mce"` block by hand, once.

## Now-playing metadata (the `metadata` kind)

A `metadata` plugin enriches what the active Source is playing **without
the Source knowing**. The core tells it what is playing, it answers with
what it knows about it.

Three layers stack up, and the later one wins:

1. **What the stream announces itself.** The core watches mpv's
   `metadata` property and reads the ICY header (`icy-title`), displayed
   **raw**, without splitting on `" - "`: the convention exists but is not
   guaranteed — OUI FM's webradios actually emit `Title - ARTIST`, in the
   reverse of the usual order. This layer works without any plugin, and
   without the Source having to declare anything.
2. **What the file itself carries.** From that very same `metadata`
   property, the core also reads a played file's tags — `title`, `artist`,
   `album`. FFmpeg normalises the keys, so ID3 (mp3), Vorbis comments
   (flac, ogg, opus), iTunes atoms (m4a) and RIFF INFO (wav) all surface
   under those three names. Shown with origin `tags`. Like ICY, this layer
   works **without any plugin** and serves *any* Source that plays a
   tagged file — nothing has to be declared for it.

   Two rules are worth knowing. The core picks **three named keys** rather
   than absorbing the object: an m4a also carries container keys
   (`major_brand`, `handler_name`) that have no place on a screen. And the
   layer stays silent as soon as **any `icy-*` key is present**: some
   stations fill in a `title` holding their own name next to an
   `icy-title` carrying the actual track, so preferring the former would
   replace the song by the station name. Stream and file tags therefore
   never coexist.
3. **What a `metadata` plugin has learned**, if it matches what is
   playing.

**A plugin takes precedence over ICY and over file tags under all
circumstances**, as long as the station does not change: what it said
stays displayed even if the stream announces a new title in the meantime.
The reason it outranks tags too is the same one that puts it on top at
all — a plugin fetches what the file cannot say (an online database, a
separate feed), so letting the file overrule it would discard the more
informed answer. These streams' ICY is of
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
new binaries and appends the missing `kind = "metadata"` entries to an
existing `/etc/ritornello/plugins.toml` (see [Declaring the
plugins](#declaring-the-plugins)), so a device already in service keeps
its CD track titles — which the cd plugin used to provide itself before
this version — and gains the Radio France ones. They are appended at the
end, therefore last in priority; since the three answer for disjoint
content, that costs nothing. Reorder the blocks by hand if you add a
plugin that overlaps one of them.

A `plugins.toml` completed by an older version of the script, which only
ever provisioned the file, is brought up to date by the next deployment
without anything being lost: the entries already there are not touched.

### The three bundled plugins

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
