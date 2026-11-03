**Action required** — update the `files` plugin with **ritornello-install**
(`ritornello-install --host account@host --version v0.2.0-beta.9 --keep`),
once, from a workstation.

The mount helper changed: `files-mount` moves from 1.0.1 to 1.0.2, for a new
per-share flag (`archive_lyrics`) in the roots table it reads too. Only
ritornello-install places the helper, so the page refuses to update `files`
on its own and says so. Everything else — the core, the other plugins and
the language packs — updates from the page as usual: no systemd unit,
polkit rule or privileged updater changed since beta.8.

This is not a wire break: three contracts gained compatible additions
(`source` 1.3, `display` 1.2, `metadata` 1.2) and the bootstrap
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

**New plugin: Web streaming** (`web-streaming`, official, not installed by
default — add it from Settings → Plugins). Paste links from YouTube,
SoundCloud, Bandcamp or any site yt-dlp reads, a single video or a
playlist; each becomes a preset, played without ads from the page or the
remote. Next and Previous move within a playlist, and otherwise to the
neighbouring preset.
- **Download** the audio of chosen presets from the plugin's page into a
  folder you pick on a writable share of the `files` plugin (a new folder
  can be created there). The dialog reopens on the last folder used.
- The plugin keeps its own **yt-dlp** up to date: its signature is checked
  against the yt-dlp maintainers' key, and an older version is never put
  back over a newer one. A stable or nightly channel, and a check button,
  sit on the plugin's page.
- **Never reaches your local network**: a link, a playlist entry or a
  download that leads to a local address is refused. One known gap remains:
  mpv itself follows redirections, playlists and manifests, which the
  plugin cannot judge.
- Needs **Python 3** on the device (DietPi and Raspberry Pi OS have it).
  About 6.5 s pass on a Raspberry Pi 2 before each video is heard.

**Lyrics.** A microphone key in the player card opens the lyrics of the
current track, the sung line highlighted when they are timed. They come
from a `<track>.lrc` beside the file, else from the file's tags, else from
LRCLIB through the new **`lrclib`** plugin (official, not installed by
default). The `files` plugin's Sources tab gains a per-share switch, off by
default, to keep lyrics found online as a `.lrc` beside the track; it never
overwrites one.

**System alerts.** A warning triangle beside **System** (top bar, and the
System tab of the phone's bottom bar) when a metric runs away, with one
sentence per alert at the top of the System page: temperature (80 °C),
processor (90 % of one core over three minutes), memory or disk (90 %), and
under-voltage. A past under-voltage stays lit until you press
**Acknowledge**, which holds until the next reboot.

**The core's version in Settings.** The Updates card opens on the version
of the core that runs, even before any check. The System page's line shows
the same number and is now labelled "Core version" too, rather than
"Ritornello version": the release's number differs when the core did not
move.

**Source keys away from standby.** In the player card's header the source
icon keys now start at the left edge, and the corner keeps standby alone.

**MPD: adding never wakes the device.** In standby, an `add` from an MPD
client no longer wakes the device, whatever the source of the entry.

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
