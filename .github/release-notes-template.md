**Action required** — run **ritornello-install** once, from a workstation,
and, if the device has the `lrclib` plugin (beta.9), replace it with
**`lrclib-lyrics`**: the update does neither by itself.

**1. The mount helper changed one last time for this reason.** `files-mount`
moves from 1.0.2 to 1.0.3: the `files` plugin's own preferences
(`archive_covers`, `archive_lyrics`) leave the library the root helper is
built from, so that a future `files` preference never again needs the
installer. What root mounts does not change, and `media-roots.toml` stays
byte for byte the same. Only ritornello-install places the helper, so the
page refuses to update `files` on its own and says so:

```sh
ritornello-install --host account@host --version v0.2.0-beta.10 --keep
```

Everything else — the core, the other plugins and the language packs —
updates from the page as usual: no systemd unit, polkit rule or privileged
updater changed since beta.9.

**2. The lyrics plugin is renamed**: `lrclib` becomes `lrclib-lyrics` (same
behaviour, still fed by LRCLIB). This release ships `lrclib-lyrics` and no
longer ships `lrclib`. A device that has `lrclib` keeps running it
untouched — the updater still finds its beta.9 archive, so it reports it
up to date and never offers anything for it again — and never installs
`lrclib-lyrics` on its own, automatic updates included. To switch:

1. **Settings → Plugins**: **Uninstall** `lrclib`. That removes its entry
   from `/etc/ritornello/plugins.toml` and its binary, and keeps its data.
2. **Settings → Plugins → Add a plugin**: install **`lrclib-lyrics`**. The
   list may still offer `lrclib` too, without a description: do not pick
   it.
3. Optional: the old lyrics cache is not migrated (the new plugin starts
   an empty one in `/var/lib/ritornello/plugins/lrclib-lyrics`). Delete the
   old one with `sudo rm -r /var/lib/ritornello/plugins/lrclib`.

Or both points in the one ritornello-install run: instead of `--keep`, name
the plugins with `--plugins`, `lrclib-lyrics` included and `lrclib` left
out (add `--purge-data` to erase `lrclib`'s cache). Interactively, untick
`lrclib` and tick `lrclib-lyrics`. With `--keep`, `lrclib` stays (listed as
a third-party plugin, since the release no longer knows it) and
`lrclib-lyrics` is not added.

Left in place, both lyrics plugins run side by side: the lyrics shown do
not change, but a song not yet cached can be looked up twice on LRCLIB.

This is not a wire break: no contract moved, and the bootstrap
`PROTOCOL_VERSION` did not move.
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

**The lyrics plugin is now `lrclib-lyrics`.** The plugin that finds lyrics
on LRCLIB, published in beta.9 as `lrclib`, is renamed `lrclib-lyrics`: its
page is at `/plugins/lrclib-lyrics/`, its cache in
`/var/lib/ritornello/plugins/lrclib-lyrics`, and it introduces itself to
LRCLIB under the new name. What it does is unchanged. See the action above
to switch a device that has `lrclib`.

**Web streaming: videos that need JavaScript now play, faster.** Some
YouTube videos no longer resolve without a JavaScript runtime ("This video
is not available"). Measured on a Raspberry Pi 2:
- On 32-bit ARM the plugin now installs and updates its own **node 22**
  (about 108 MB), the last node line built for that architecture; its list
  of sums must be signed by one of node's release keys, an older version
  never replaces a newer one, and after node 22's end of life (April 2027)
  it is kept and the page says so. Such a video: 2 min 10 s with QuickJS,
  about 30 s with node, then **about 15 s** once yt-dlp's player cache,
  which the plugin now turns on, has the current YouTube player.
- Adding a link, refreshing a playlist or probing a download may now take
  up to 5 minutes before the page gives up (was 2).
- The capabilities panel says the JavaScript runtime only serves when a
  video fails without one.

**Web streaming downloads: format, cover, name.**
- **Format**: original (as the site gives it, webm for YouTube), **m4a** or
  **mp3**, remembered. mp3 (and m4a from a site without an m4a stream) is
  converted on the device by ffmpeg, offered only when ffmpeg is installed
  (`sudo apt install ffmpeg`); the page says when it is missing.
- **Cover of a single video**: beside the track as `<name>.jpg` (default),
  or embedded in the file (m4a, mp3, opus, ogg; a webm falls back beside the
  file). No more `cover.jpg` for a single video: it became the cover of
  every later video in the folder. Playlists keep their folder's
  `cover.jpg`.
- **Name**: the file's (or the playlist folder's) name can be changed in
  the dialog, prefilled with the usual one; nothing is ever overwritten.
- Creating a folder in the dialog now enters it.

**Files: a track's own cover, and more formats.** `<track>.jpg` (or
`.jpeg`, `.png`) beside a track is now its cover, before the folder's
image, and a folder's lone image is no longer guessed from another track's
own cover. The files plugin now lists every audio format mpv was seen to
play, `.webm` included (a web-streaming download in its original format).

**Plugins table after an install.** The configuration page reads the
plugins table again once an install job is over, instead of showing the
state from before it.

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
