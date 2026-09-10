# Working on Ritornello

Ritornello turns a Raspberry Pi into an internet radio and CD player: an
unprivileged Rust core, ten plugins that speak to it over Unix sockets, and
a Vue web UI embedded in the binary. It runs every day in one living room.

This file is short on purpose. The project's knowledge lives in `docs/` and
in the code comments, and duplicating it here would create a second source
of truth that rots — which has already happened here more than once, in
documents that then went on being believed. What follows is a map, plus the
handful of rules that are invisible from the code you happen to be reading.

## Which document answers which question

| You need to | Read |
|---|---|
| Run the thing locally without a Pi, run the tests, regenerate embedded data | [docs/development.md](docs/development.md) |
| Build, install, deploy, arm the self-update, **cut a release or a prerelease** | [docs/installation.md](docs/installation.md) |
| Understand a plugin, write one, publish a third-party one | [docs/plugins.md](docs/plugins.md) |
| Understand the web UI, the command API, the remote, updates | [docs/interface.md](docs/interface.md) |

`docs/installation.md` ends with **What has not been verified** — a list of
what has been built and tested but never run on real hardware. Read it
before claiming any of it works. It is kept honest deliberately; add to it
rather than quietly shrinking it.

## Rules you cannot infer from the code in front of you

**Everything written into this repository is in English.** Code, comments,
doc comments, test names, commit messages, documentation. No exceptions.

**Three numbers, three questions.** The product number
(`[workspace.package] version`) names the release and the git tag. Each
shipped component — the core and each plugin — declares **its own** version,
and that is what its archive is named after. `PROTOCOL_VERSION` is the
core/plugin wire contract and moves only on a break. Major and minor stay
identical everywhere; only the patch digit is free, component by component.

**A device compares versions for equality, never for order.** This is what
makes rollback and channel-switching work, and it is the reason every
delivery must move a number: an archive republished under its old number is
never fetched. The one silent failure to know about is a change to a shared
crate (`ritornello-proto`, `ritornello-i18n`, `ritornello-plugin-sdk`,
`ritornello-updater`): every binary is rebuilt and republished, under
unchanged numbers, so the release looks complete and delivers nothing.

**Never delete a published release, or any file attached to one.** The
archive of a component that has not changed in a long time lives in the
release where it last changed, and that is where a device installs it from.

**The security boundary.** The core runs unprivileged. A small root binary
(`crates/ritornello-updater`) can form **exactly two** path shapes —
`/usr/local/bin/ritornello-core`, and the plugins directory joined with a
validated bare name — and reads no archive at all. Read
`crates/ritornello-updater/src/target.rs` before touching anything near it;
adding a third location is a design decision, not an oversight. An update
can never write a systemd unit or a polkit rule: those are placed by
`deploy/deploy.sh` or by hand. A plugin archive carrying a unit, a polkit
rule, a nested path or a binary that is not the plugin's own is refused.
**The core is exempt from that last rule; no third-party component ever is.**

**No HTTP route may block.** The admin protocol is serial with a five-second
ceiling, and an I/O left without a deadline has already made a page
*disappear* rather than fail.

**The npm build always precedes the cargo builds.** The SPA and each
plugin's `ui.js` are embedded at compile time (`rust-embed`,
`include_str!`), so cargo consumes whatever npm last produced.
`deploy/build.sh` applies that order.

## Commands

    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    npm test --workspaces
    npm run typecheck
    npm run e2e -w app          # Playwright; it really plays audio

`scripts/ci-local.sh [web|rust|e2e]` runs what CI runs, in CI's order. If
one changes, the other must follow.

## Guards, and what they are for

Several tests exist only to refuse a mistake that has actually been made
here. Do not "fix" one by relaxing it — if a guard fires, it has found
something.

- `version_coherence.rs` — the three numbers above, including what a
  prerelease may and may not declare.
- `packaging_manifest.rs` — `deploy/packaging.toml` against reality, and
  against `deploy/deploy.sh`: the two installation paths must agree on every
  privileged file.
- `scripts/package-release.sh --self-test` — the same generation rules
  without cargo, since the release job runs without our toolchain.
- `scripts/changed-components.sh` — exits 2 when no component moved, rather
  than publishing an empty release that looks like success.
- `scripts/release-notes-guard.sh` — refuses a release that changed a unit,
  a polkit rule or the updater while the notes still say "Nothing to do".
- `check-dist.mjs` / `check-plugin-dist.mjs` — the import map and a single
  Vue runtime, after every npm build.

## How work is expected to be done here

Tests are written against **real captures** (mpv frames, radio-browser
responses, live Radio France answers), and they are **discriminating**:
several encode a regression that actually happened and name it in a
comment. A test that passes against a deliberately broken implementation
proves nothing — when a guard matters, break it on purpose and check it
fires, once per branch of the predicate.

Verify rather than reason about it. The habit is cheap here and it has paid
repeatedly: measure the command's exit code instead of reading its output
through `| tail`, which swallows it; check what the parser does with the new
shape of data before assuming a flag is enough.

Design records — specifications, implementation plans, reviews — are kept
outside this repository. Durable decisions live in the code comments and in
`docs/`.
