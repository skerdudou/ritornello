**Action required** — if the device has the `lrclib` plugin (beta.9),
replace it with **`lrclib-lyrics`**, by hand, once: the update does not do
it.

The lyrics plugin is renamed: `lrclib` becomes `lrclib-lyrics` (same
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

With **ritornello-install** instead: `--keep` leaves `lrclib` in place (it
is listed as a third-party plugin, since the release no longer knows it)
and does not add `lrclib-lyrics`. Name the plugins with `--plugins`,
`lrclib-lyrics` included and `lrclib` left out; add `--purge-data` to
erase `lrclib`'s cache in the same run. Interactively, untick `lrclib` and
tick `lrclib-lyrics`.

Left in place, both plugins run side by side: the lyrics shown do not
change, but a song not yet cached can be looked up twice on LRCLIB, and
is cached twice.

This rename is not a wire break: no contract moved, and the bootstrap
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
     core is reinstalled.

     For the release that ships this block: move the fr/de/es/it language
     packs too (deploy/language-packs.toml). Their published archives still
     carry `lrclib.toml`, not `lrclib-lyrics.toml`, so until they move the
     renamed plugin's page stays in English on a non-English device, and
     changed-components.sh only warns about it. -->

## Behaviour changes

**The lyrics plugin is now `lrclib-lyrics`.** The plugin that finds lyrics
on LRCLIB, published in beta.9 as `lrclib`, is renamed `lrclib-lyrics`: its
page is at `/plugins/lrclib-lyrics/`, its cache in
`/var/lib/ritornello/plugins/lrclib-lyrics`, and it introduces itself to
LRCLIB under the new name. What it does is unchanged. See the action above
to switch a device that has `lrclib`.

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
