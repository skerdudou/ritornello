# Installation and operations

## Portability

Nothing in the code is specific to the Raspberry Pi: the remote control
goes through `evdev` (the generic Linux input API, not GPIO), sound through
ALSA/mpv, IPC through Unix sockets — all of which run on any Linux, x86_64
and ARM alike. The Pi 2 is this project's historical reference hardware,
not a technical constraint — the examples below merely illustrate it.

## Building

The web interface is a SPA (Vue 3 + shadcn-vue) embedded into the core
binary: **Node 20+** is therefore a development prerequisite, where `cargo`
used to be enough. The reference procedure is `deploy/build.sh`, which
always runs the three steps in this order:

    ./deploy/build.sh                 # npm, then cargo x86_64, then cross ARM
    TARGET=aarch64-unknown-linux-gnu ./deploy/build.sh

The npm build runs only once: its output is read at compile time by both
cargo steps. This is what lets `cross` work with a Docker image that has no
Node.

A `cargo build` run on its own, without building the UI first,
**succeeds**: a placeholder is embedded instead, and the served page
invites you to run `npm run build --workspaces`. This is not a failure.
The tests (`cargo test --workspace`) stay green in that situation; on the
browser side, `npm test --workspaces` covers the UI and `npm run e2e -w app`
the full journeys (see [development.md](development.md)).

The workspace compiles natively for the architecture of the machine
running the command (x86_64 on a typical Linux PC/server), and for ARM by
cross-compilation with [`cross`](https://github.com/cross-rs/cross) (which
needs Docker):

    # Native (e.g. x86_64) — also used for tests during development
    cargo build --workspace
    cargo test --workspace

    # ARM cross-compilation (e.g. Raspberry Pi 2, 32-bit)
    cargo install cross --locked
    cross build --release --workspace --target armv7-unknown-linux-gnueabihf

Both paths are exercised on every project change. Other ARM targets are
possible with `cross`: `aarch64-unknown-linux-gnu` (64-bit ARM boards, Pi
3/4/5 class — untested on this project for lack of hardware, but with no
reason not to work).

## Example: Raspberry Pi 2

Raspberry Pi OS Lite, then:

    sudo apt install mpv cd-discid eject
    sudo cp deploy/stations.example.toml /etc/ritornello/stations.toml
    sudo cp deploy/plugins.example.toml /etc/ritornello/plugins.toml
    sudo cp -r deploy/input-presets /etc/ritornello/input-presets
    sudo cp deploy/input-bindings.example.toml /etc/ritornello/input-bindings.toml
    # analog jack as default output + hardware volume at maximum
    sudo raspi-config nonint do_audio 1
    amixer set PCM 100%

Wifi: `sudo raspi-config` (System Options > Wireless LAN).

## Example: generic x86_64 Linux machine

Same packages, minus the Pi-specific steps (no `raspi-config`, the audio
output is picked directly through `/api/audio-output`):

    sudo apt install mpv cd-discid eject
    sudo cp deploy/stations.example.toml /etc/ritornello/stations.toml
    sudo cp deploy/plugins.example.toml /etc/ritornello/plugins.toml
    sudo cp -r deploy/input-presets /etc/ritornello/input-presets
    sudo cp deploy/input-bindings.example.toml /etc/ritornello/input-bindings.toml

`deploy/deploy.sh` works identically: `TARGET=x86_64-unknown-linux-gnu
PI=user@host ./deploy/deploy.sh` (no need for `cross`/Docker for this
target if the build machine is already x86_64 — a native `cargo build` is
enough then; `cross` is mostly useful for changing architecture).

## Deploying

    PI=pi@raspberrypi.local ./deploy/deploy.sh

`PI` names any target SSH host (Pi or other Linux), and `TARGET` the
compilation target (see [Building](#building)) — the two override
independently, e.g. `TARGET=x86_64-unknown-linux-gnu PI=user@host
./deploy/deploy.sh`. The script chains `build.sh` (so the npm UI build
**then** the cross-compilation — the order guarantees the embedded SPA is
fresh), copies the binaries, the language packs and the presets, installs
the systemd unit and restarts the service.

Web interface: http://<host>:8080 — logs: `journalctl -u ritornello -f`.

`deploy.sh` installs the binaries but **never touches**
`/etc/ritornello/plugins.toml`: on first installation, provision it from
`deploy/plugins.example.toml` (see the examples above); when an update
introduces new plugins, add their entries by hand (see
[plugins.md](plugins.md)).

## Audio dropouts

Two distinct buffers protect playback, and they do not address the same
problem. Confusing them wastes time.

| Variable | Default | What it protects |
|---|---|---|
| `RITORNELLO_AUDIO_BUFFER` | `0.2` | the **output**: an ALSA write deadline missed because the machine was busy |
| `RITORNELLO_NETWORK_READAHEAD` | `1` | the **input**: network jitter draining an internet stream's read-ahead |

Both are in seconds and apply when mpv is launched (`--audio-buffer` and
`--demuxer-readahead-secs`). The defaults are **mpv's own**: with no
variable set, playback behaves exactly as if these options were not passed.
An unreadable or out-of-range value is ignored with a warning in the logs,
without preventing startup.

Before turning a knob, **identify which one** — the two symptoms sound the
same but are not fixed in the same place:

    mpv --no-video --msg-level=ao=v,cache=v <station-url> 2>&1 \
      | grep -iE "underrun|buffering|cache"

`underrun` lines point at the output: raise `RITORNELLO_AUDIO_BUFFER`, for
example to `0.5`. `buffering` lines point at the input: raise
`RITORNELLO_NETWORK_READAHEAD`, for example to `10`, or even `30` on a
flaky link — ten seconds of 128 kbit/s MP3 weigh about 160 KB, negligible
even with 1 GB of RAM.

One case to rule out from the start: during development under **WSL**,
audio crosses the WSLg bridge to Windows, whose own jitter produces
dropouts that neither of these settings will fix. Only draw conclusions
about the buffers after listening on the target machine.

Increasing the **output** buffer helps against dropouts caused by machine
load, at the cost of the same amount of latency on volume or mute taking
effect. **Reducing it makes dropouts worse**: it is the direction of the
change that matters, not its magnitude.

To tell the two causes apart, run `journalctl -u ritornello -f` during a
dropout: mpv logs the network cache draining, not ALSA underruns.
