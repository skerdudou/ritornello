**Nothing to do** — replace the binaries.

No systemd unit, polkit rule, privileged updater or mount helper changed
since beta.7, so the page updates everything by itself. This assumes the
device already went through ritornello-install 0.3.0 or later once, as
beta.7 asked.

This is not a wire break: three contracts gained compatible additions
(`source` 1.2, `display` 1.1, `metadata` 1.1) and the bootstrap
`PROTOCOL_VERSION` did not move. Any mix of old and new components still
talks: an older core ignores what it does not know.
<!-- The opening block above is this release's answer to "is there anything
     to do by hand?", and it is rewritten at every release rather than left
     to rot. When the next one needs nothing, the whole block above becomes
     one line:

     **Nothing to do** — replace the binaries.

     and when it does need something, it stays as it is here:

     **Action required** — <what to do by hand>.

     In 0.x semver the minor carries breaks, so the digit alone cannot say
     this. The sentence can.

     Action required when this release changes any of:

     - a systemd unit (`deploy/*.service`)
     - a polkit rule (`deploy/*.rules`)
     - `ritornello-media-mount`, the helper root runs for network shares
       (the `files-mount` companion archive, which only ritornello-install
       places)

     The updater cannot write any of those, by design. Say so here, with the
     command, or the device will update its binaries and silently keep the
     old unit.

     A release that moves the bootstrap `PROTOCOL_VERSION` (or a contract's
     major) is a wire break, and needs this line too:

     **Action required** — wire break: install the core and every plugin
     together. A rollback of the core leaves every plugin refused until the
     core is reinstalled. -->

## Behaviour changes

**Stop keeps the track on screen, and Next and Previous move it.** While
nothing plays, the source keeps one track *armed*: the one Play starts.
- The screen (web page and physical display) keeps its title, artist and
  cover.
- Next and Previous move the armed track without any sound, and Play
  starts it from its beginning.
- **Files**: Stop arms the track that was playing. Next and Previous
  follow the current order, shuffled or not, and wrap around. At the end
  of the list (repeat off), the first track of the next pass is armed.
- **CD**: a disc that is inserted and not played shows its armed track at
  once, and MusicBrainz is asked for its titles then. Next and Previous
  stop at both ends of the disc. Eject forgets the track. Play after Stop
  restarts the track that is armed, whatever the "on insertion" setting
  says: that setting governs arrival and insertion only.
- **Radio**: Stop keeps the station's number and name, but not the song
  that was on air. Next and Previous change station without tuning in.
- A third-party source keeps today's behaviour until it learns to arm a
  track: Stop empties the screen.

**The display shows whether it plays.** The first line of the console
display now starts with ▶ (playing), ║ (paused) or ■ (stopped).

**Choosing a source wakes the device.** In standby, a source key — on the
page, the remote or from an MPD client — wakes the device on that source.
The active source wakes where it was, and another one is switched to.
Everything else still waits for Power.

**Source icons in the player card.** The home page chooses the source with
one icon key per source in the player card's header, the active one
pressed. When they do not fit, the single Source key cycles through them
as before. A source without an icon shows its initial. The bar of named
source keys above the cards is gone.

**The presets card in standby, and its pages.** In standby, the presets
grid keeps showing the active source's stations. With several pages, the
pagination sits in the card header ("11–20 of 23"), and the lone count
under the title is gone.

**Choose the name of an archived cover.** The files plugin's Sources tab
picks, once for every source, the name a found cover is saved under next
to the music: `cover` (the default, as before), `folder`, `front`,
`albumart` or `album`. A folder that already holds an image under any of
those names, in any case, is never written to.

## Install

Common case — the core and every plugin:

```sh
sha256sum -c SHA256SUMS --ignore-missing
sudo tar --no-same-owner -C / -xzf ritornello-core-<version>-<arch>.tar.gz
sudo tar --no-same-owner -C / -xzf ritornello-plugins-<version>-<arch>.tar.gz
sudo chown -R ritornello: /etc/ritornello
sudo systemctl daemon-reload && sudo systemctl restart ritornello
```

`--no-same-owner` is belt and braces, not a workaround: the archives are
built with every entry owned by `root`, and the packaging script refuses to
produce one that is not. But extracting as root restores whatever ownership
an archive carries, so the flag makes the recipe safe even for an archive
built before that rule existed — a polkit rules file (JavaScript `polkitd`
runs as root) owned by a non-root uid is a local privilege escalation.

Each archive holds the tree as it will exist on the device, and holds no
configuration you may have edited: `stations.toml`, `input-bindings.toml` and
`plugins.toml` are never in it. Example files and the `plugins.toml` block of
each plugin travel beside the tree, to be copied by hand.

Replacing a single plugin is the same two commands with that plugin's archive.

The `files` plugin needs its companion, `ritornello-files-mount`, which
carries the root mount helper, its unit and its polkit rule: the plugin's own
archive does not. After installing `ritornello-plugin-files` together with
`ritornello-files-mount` (or the bundle, which carries both) for the first
time:

```sh
sudo systemctl enable ritornello-media-mount.service
```

Enabled, **not** started: what it is enabled for is machine boot, where it
reconciles the declared network shares. Skip it and declared shares silently
stop being mounted after a reboot.

A language pack (`ritornello-lang-<language>-<version>.tar.gz`) is not part
of the recipe above, and not part of any component's archive either. It is
**flat** — `pack.toml` plus one file per plugin it covers, no leading
path — so the `tar -C /` above must never be run against it: doing so drops
those files straight into `/`. Install one from the config page, where the
core fetches, verifies and writes it itself with no privileged step at all;
or, by hand, extract it into its own directory:

```sh
sudo mkdir -p /etc/ritornello/language-packs/ritornello-lang-<language>
sudo tar --no-same-owner -C /etc/ritornello/language-packs/ritornello-lang-<language> \
  -xzf ritornello-lang-<language>-<version>.tar.gz
sudo chown -R ritornello: /etc/ritornello/language-packs/ritornello-lang-<language>
```

## What this release carries

This release carries the components whose own version moved since the
previous one. When the bootstrap `PROTOCOL_VERSION` or the product's major changed, every
component moved; when a wire contract's major changed, the core and every
plugin that speaks it moved (the release script refuses a break that left one
of them on its old number). A component missing from the assets below is
unchanged, not removed — do not read its absence as a regression.

**If a shared crate changed** (`ritornello-proto`, `ritornello-i18n`,
`ritornello-plugin-sdk`, `ritornello-updater`) compatibly, nothing was
republished for it: devices install on version inequality alone, so a
component only reaches them under a new number. If the fix must reach a
plugin, that plugin's version must have been bumped by hand in the same
commit.

The version in an attached file's name is that **component's** own version,
not this release's tag: `ritornello-plugin-radio-0.2.1-<arch>.tar.gz` says
nothing about the tag it ships under. Only `ritornello-plugins-<version>`,
the bundle of every plugin, is named after this release's own tag.

**Never delete a published release or its attached files.** The appliance
goes back to fetch, from there, the components that have not changed since.

## Architectures

- `armv7` — Raspberry Pi 2 and similar, the reference hardware.
- `x86_64` — PC/server; the target this project's tests and end-to-end
  journeys run on.
- `arm64` — Pi 3/4/5 class. **Cross-compiled but never started on hardware**,
  for lack of a device to try it on.
