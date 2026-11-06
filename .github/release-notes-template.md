**Action required** — only on a device that is not yet on beta.11 *through
ritornello-install 0.4.0 or newer*: make sure it has **`ca-certificates`**,
then run **ritornello-install** once, from a workstation. A device already
there needs nothing by hand: it updates from the page.

**Ritornello 0.2.0 — the end of the beta.** This is the first finished
release: every component moves from its beta number to `0.2.0` (the root
mount helper `files-mount` 1.0.3 and the privileged updater 1.0.0 keep their
own numbers, unchanged). A device set to finished releases only now has
something to install, and a device that offered prereleases can untick
"Offer prereleases" and stay on finished releases from here on.

**Coming from a beta before beta.11** — the two points beta.11 introduced
still apply:

1. **`ca-certificates` is required.** The core and the plugins check HTTPS
   certificates against the device's own store (`/etc/ssl/certs`). Without
   the package every HTTPS request fails, the update check included:

   ```sh
   dpkg -s ca-certificates >/dev/null 2>&1 || sudo apt install ca-certificates
   ```

2. **A polkit rule only the installer places** (`53-ritornello-time.rules`,
   the time zone from the System page): the page refuses to update the core
   until ritornello-install has run, and says so. The installer asks which
   system permissions to grant (*power off or restart*, *change the time
   zone*); on a device already installed both are granted unless you say
   otherwise:

   ```sh
   ritornello-install --host account@host --version v0.2.0 --keep
   # or, to decline one or both:
   ritornello-install --host account@host --version v0.2.0 --keep --grants power
   ritornello-install --host account@host --version v0.2.0 --keep --grants none
   ```

No systemd unit, polkit rule, privileged updater or mount helper changed
since beta.11. This is not a wire break: no contract moved, and the bootstrap
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

## Behaviour changes since beta.11

**Web streaming: the cover of what plays.** A video played from a preset
now shows its cover in the player: the site's own thumbnail, the same one a
download keeps. The plugin fetches it itself, through the same guard as a
download (never the local network), and hands the core a file. A playlist's
videos each show their own; a single video's thumbnail is looked up once,
the first time it plays, then kept.

**Web streaming: downloading, in two steps.** The download dialog first asks
*where* (the share, then one framed folder browser with the new folder at its
foot), then *how* (format, cover, names), with the destination still shown
and a way back. The cover choice now applies to **each track**, playlists
included: every track of a downloaded playlist gets its own thumbnail,
inside the file or beside it, and the folder keeps its `cover.jpg`.

**Web streaming: the downloads list.** It sits right under the presets,
where a download is started, and **Clear finished** forgets the downloads
that are over. A running line reads "running 42%" again, not "running42%".

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

**Never delete a release that still holds some component's current
archive.** The appliance goes back to fetch, from there, the components that
have not changed since.

## Architectures

- `armv7` — Raspberry Pi 2 and similar, the reference hardware.
- `x86_64` — PC/server; the target this project's tests and end-to-end
  journeys run on.
- `arm64` — Pi 3/4/5 class. **Cross-compiled but never started on hardware**,
  for lack of a device to try it on.
