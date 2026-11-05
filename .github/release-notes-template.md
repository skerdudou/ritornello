**Action required** — make sure the device has **`ca-certificates`**, then
run **ritornello-install 0.4.0** once, from a workstation: the page will not
update the core by itself.

**1. `ca-certificates` is now required.** From this release on, the core and
the plugins check HTTPS certificates against the device's own certificate
store (`/etc/ssl/certs`) instead of a list compiled into them. Without the
`ca-certificates` package every HTTPS request fails: radio search, Radio
France, MusicBrainz covers, lyrics, and the update check itself. Most images
already have it; check, and install it if needed, **before** anything else:

```sh
dpkg -s ca-certificates >/dev/null 2>&1 || sudo apt install ca-certificates
```

ritornello-install 0.4.0 refuses to install or update a device without it.
An update from the page does not check.

**2. A new polkit rule, and the installer now asks.** This release ships
`53-ritornello-time.rules`, which lets Ritornello change the device's time
zone from the System page. Only ritornello-install places a polkit rule, so
the page refuses to update the core until it has run, and says so. Use
**ritornello-install 0.4.0** (the README's download link): it now asks which
system permissions to grant —
*power off or restart the system* and *change the time zone* — each one
placing its polkit rule only when granted. The page is not authenticated:
whatever it may do, anyone on your network may do, so untick what you do not
want. On a device already installed, both are granted unless you say
otherwise:

```sh
ritornello-install --host account@host --version v0.2.0-beta.11 --keep
# or, to decline one or both:
ritornello-install --host account@host --version v0.2.0-beta.11 --keep --grants power
ritornello-install --host account@host --version v0.2.0-beta.11 --keep --grants none
```

Declining a permission removes its rule; running the installer again
changes your mind. A declined permission never blocks the page's own
updates.

An older installer (0.3.x) still installs this release, but grants both
permissions without asking and does not check `ca-certificates`.

The plugins and the language packs update from the page as usual. No
systemd unit or privileged updater changed since beta.10. This is not a wire
break: no contract moved, and the bootstrap `PROTOCOL_VERSION` did not move.
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

**The device's time, and its time zone, on the System page.** A new
**Time** card shows the device's clock and time zone, lets you pick another
zone, and offers "use my browser's zone" when the two differ. The change
applies at once: the automatic update schedule and the standby clock on the
HDMI screen follow it without a restart.

**The update hour says which clock it means.** The automatic update hour
is counted in the **device's** time zone. When the device and your browser
are on different zones, the Updates card now says so under the hour, with
what that hour is for you ("03:00 on the device is 04:00 for you"). A device
left on UTC was the reason an update set for 3 am ran at 4 am.

**Greyed buttons say why.** Shut down, Restart and the time zone controls
stay visible when they cannot work, and one sentence says why: not allowed
when Ritornello was installed, polkit did not answer, or the system service
is unreachable.

**HTTPS trusts the device's certificate store** (see the action above). The
core and the plugins that talk HTTPS (radio, Radio France, NRJ, Oüi FM,
MusicBrainz, lyrics, web streaming) now use the device's certificates, so a
certificate authority Debian removes is removed here too, and they trust
exactly what mpv and yt-dlp already trust on the device. The core and
plugins are about 0.7 MB larger each on armv7.

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
