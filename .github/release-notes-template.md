**Nothing to do** — replace the binaries.
<!-- Or, when a manual step is needed, replace the line above by:
     **Action required** — <what to do by hand>.
     In 0.x semver the minor carries breaks, so the digit alone cannot say
     this. The sentence can. -->

## Install

Common case — the core and every plugin:

```sh
sha256sum -c SHA256SUMS --ignore-missing
sudo tar -C / -xzf ritornello-core-<version>-<arch>.tar.gz
sudo tar -C / -xzf ritornello-plugins-<version>-<arch>.tar.gz
sudo chown -R ritornello: /etc/ritornello
sudo systemctl daemon-reload && sudo systemctl restart ritornello
```

Each archive holds the tree as it will exist on the device, and holds no
configuration you may have edited: `stations.toml`, `input-bindings.toml` and
`plugins.toml` are never in it. Example files and the `plugins.toml` block of
each plugin travel beside the tree, to be copied by hand.

Replacing a single plugin is the same two commands with that plugin's archive.

## Architectures

- `armv7` — Raspberry Pi 2 and similar, the reference hardware.
- `x86_64` — PC/server; the target this project's tests and end-to-end
  journeys run on.
- `arm64` — Pi 3/4/5 class. **Cross-compiled but never started on hardware**,
  for lack of a device to try it on.
