**Action required** — this is the first release, so it carries every
privileged file at once: the updater's binary, two new systemd units, a new
polkit rule, and a `ritornello.service` that gained the start limit and the
`OnFailure=` line arming the rollback. An update can write none of those, by
design, so they are placed by hand or by `deploy.sh`.

On a device deployed with `deploy/deploy.sh`, nothing to do: the script
places all four and reloads systemd.

On a device installed from these archives, after extracting the core's:

```sh
sudo systemctl daemon-reload
sudo systemctl restart ritornello
```

The four files are inside `ritornello-core-<version>-<arch>.tar.gz` at the
paths they occupy on the device, so the ordinary `tar -C /` of the Install
section below already put them there. What does not happen on its own is the
`daemon-reload`: without it systemd keeps the old `ritornello.service`, the
rollback stays unarmed, and every install attempt is refused by polkit with
`Access denied` — a message that names no file.

Until this release is installed, the update card checks and reports
correctly and every install fails. That is expected: the feature installs
its own privileged half exactly once, and this is that once.
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

     The updater cannot write any of those, by design. Say so here, with the
     command, or the device will update its binaries and silently keep the
     old unit. -->

## Behaviour changes

Behaviour change: the source key's cycle now follows the order of
`/etc/ritornello/plugins.toml` instead of being sorted alphabetically. On a
device in service the cycle order therefore changes at the first start after
this update. The configuration page reorders it, and the same order now also
sets metadata priority.

Behaviour change: updating a plugin now refuses when its `plugins.toml`
entry declares an `exec` outside
`/usr/local/lib/ritornello/plugins/`. Before this release that case was a
silent half-success — the archive's binary landing where the entry is never
read from, and the row then claiming a version that is not the one actually
running. If an existing device meets this refusal on its next update, the
fix is to move the binary under the plugins directory, or to point `exec`
there.

New setting, and no behaviour change unless it is ticked: "Offer
prereleases", in the automatic-checks card. Off by default, so a device that
is not told otherwise is offered finished releases only, exactly as before.
Ticked, betas and release candidates are offered too — on every check, for
the core, the plugins and any third-party plugin. A device on prereleases
moves to a finished release as soon as one is published, since the two carry
different version numbers.

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

The `files` plugin brings a unit of its own. After installing
`ritornello-plugin-files` (or the bundle) for the first time:

```sh
sudo systemctl enable ritornello-media-mount.service
```

Enabled, **not** started: what it is enabled for is machine boot, where it
reconciles the declared network shares. Skip it and declared shares silently
stop being mounted after a reboot.

## What this release carries

This release carries the components whose own version moved since the
previous one — plus, when a shared crate changed, every component, since all
eleven binaries were rebuilt. A component missing from the assets below is
unchanged, not removed — do not read its absence as a regression.

**If a shared crate changed** (`ritornello-proto`, `ritornello-i18n`,
`ritornello-plugin-sdk`, `ritornello-updater`), check that **every**
component's version was bumped in the same commit. Devices install on version
inequality alone: an archive republished under its old number is never
fetched, so the release ships everything and delivers nothing.

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
