// Launches a throwaway core for the Playwright journeys: temporary state
// directory, dedicated port, every UI-bearing plugin declared (radio, files,
// generic-input), plus the audio fixtures the `files` journey browses.
// Deliberately close to the development setup of docs/development.md.
//
// This workshop's peculiarity: Node/npm/Playwright run on the Windows
// side, but the core binaries are Linux ELFs compiled under WSL (Node is
// absent from WSL). This script therefore detects the platform:
//  - under Linux (the environment documented by docs/development.md, and
//    that of a possible CI): launches the binary directly, as before;
//  - under Windows: writes the configuration into a temporary directory
//    then launches the core through `wsl.exe -- bash <script.sh>`, with
//    `/mnt/c/...` paths and `RITORNELLO_HTTP=0.0.0.0:8099` — measured: a
//    service bound to 127.0.0.1 *inside* WSL is not reachable from
//    Windows, while one bound to 0.0.0.0 is, on 127.0.0.1 host-side.
//
// Under Windows, two distinct directories are needed, not one:
//  - a *configuration* directory, under the repo tree (hence visible both
//    from Windows and, through `/mnt/c/...`, from WSL): it only holds the
//    files whose content Node generates (plugins.toml, stations.toml, the
//    launch script, the `files` fixtures) — plain files, no problem on
//    that mount;
//  - an *execution* directory, native to the WSL filesystem (under
//    `/tmp`): measured, `mpv --input-ipc-server=<path>` does not create
//    its Unix socket when `<path>` is under `/mnt/c/...` (the DrvFs 9p
//    mount does not support Unix sockets), while the same call succeeds
//    under `/tmp`. All the sockets (mpv, plugins) and the PID file
//    therefore live here — the core creates this directory itself (and
//    those of state.json, etc.) via `create_dir_all`, no need to
//    pre-create it.
//
// The launch goes through a `.sh` file (rather than a huge `bash -lc
// '<inline script>'`): measured, an inline command combining single and
// double quotes, `$(...)` and `$$` across Node -> wsl.exe -> bash
// sometimes gets corrupted on the way (exact cause not identified with
// certainty — plausibly a re-interpretation of the argument by one of the
// Windows/WSL interop layers); a file path, by contrast, contains no
// character sensitive to that crossing.
//
// Clean shutdown is delegated to `teardown.mjs` (see globalTeardown in
// playwright.config.ts): killing this process or `wsl.exe` from Windows
// does not necessarily kill the Linux process launched inside WSL2, so a
// state file is written here (real WSL-side PID + execution directory)
// that `teardown.mjs` can find and stop explicitly, whatever the fate of
// *this* node process.
import { randomBytes } from 'node:crypto'
import { spawn, spawnSync } from 'node:child_process'
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const isWindows = process.platform === 'win32'
const rootNative = process.cwd().replace(/[\\/]web[\\/]app$/, '')

// Converts a Windows path (`C:\a\b`) into its WSL equivalent
// (`/mnt/c/a/b`): the only way for a Linux process, launched from Windows
// through `wsl.exe`, to find the files Node wrote here.
function toWsl(cheminWindows) {
  const normalise = cheminWindows.replace(/\\/g, '/')
  const correspondance = /^([A-Za-z]):\/(.*)$/.exec(normalise)
  return correspondance ? `/mnt/${correspondance[1].toLowerCase()}/${correspondance[2]}` : normalise
}

// Under Windows, the configuration directory is created under the repo
// tree (hence under a `/mnt/c/...` mount point predictable for WSL2)
// rather than in the system `tmpdir()`, whose root (often `AppData`)
// offers no such guarantee.
mkdirSync(join(rootNative, 'target'), { recursive: true })
const configDirNative = isWindows
  ? mkdtempSync(join(rootNative, 'target', 'e2e-'))
  : mkdtempSync(join(tmpdir(), 'ritornello-e2e-'))

const root = isWindows ? toWsl(rootNative) : rootNative
const configDir = isWindows ? toWsl(configDirNative) : configDirNative
// Execution directory (sockets, PID): WSL-native under Windows — see the
// header —, same as the configuration directory under native Linux where
// the question does not arise.
const execDir = isWindows ? `/tmp/ritornello-e2e-${randomBytes(6).toString('hex')}` : configDir

// Fixtures for the `files` journey: three short tracks in a subfolder, under
// the *configuration* directory — the only one Node (Windows side) can write
// AND the core (WSL side) can read, through `/mnt/c/...`. They are audio only
// by their extension and their existence: the journey never listens.
//
// The bytes come from ffmpeg when it is there, and it is looked for on the
// side that runs the core (WSL under Windows, the machine itself under Linux):
// mpv lives there too, and a Windows-side ffmpeg would say nothing about the
// tools the core actually has. When ffmpeg is missing, a few placeholder bytes
// take over — the scan filters on the extension (`scan::is_audio`) and the
// journey declares `finite` playback, so an unreadable track costs an mpv
// error line, never a hung or looping journey.
const mediaNative = join(configDirNative, 'media')
const albumNative = join(mediaNative, 'Album')
const mediaRoot = `${configDir}/media`
mkdirSync(albumNative, { recursive: true })
const tracks = ['01', '02', '03']
{
  // Through a `.sh` file, and for the same reason as the launch below: an
  // inline command crossing Node -> wsl.exe -> bash sometimes gets corrupted
  // when it mixes quotes and `$`, whereas a path does not.
  const script =
    `#!/usr/bin/env bash\n` +
    `command -v ffmpeg >/dev/null 2>&1 || exit 1\n` +
    tracks
      .map(
        (n) =>
          // 30 s and not 1 s: with one-second tracks, the playlist ran out
          // before the journey had finished observing it, and the "playlist
          // finished" state got confused with "selection failed". A
          // realistic duration is what makes playback observable.
          `ffmpeg -loglevel error -y -f lavfi -i 'sine=frequency=440:duration=30' ` +
          `'${mediaRoot}/Album/${n}.mp3' || exit 1`,
      )
      .join('\n') +
    `\n`
  writeFileSync(join(configDirNative, 'fixtures.sh'), script)
  if (isWindows) {
    spawnSync('wsl.exe', ['--', 'bash', `${configDir}/fixtures.sh`], { stdio: 'inherit' })
  } else {
    spawnSync('bash', [`${configDir}/fixtures.sh`], { stdio: 'inherit' })
  }
  for (const n of tracks) {
    const chemin = join(albumNative, `${n}.mp3`)
    let taille = 0
    try {
      taille = statSync(chemin).size
    } catch {
      // Absent: ffmpeg missing, or refused this build's encoders.
    }
    if (taille === 0) writeFileSync(chemin, `not audio, only an extension: ${n}\n`)
  }
}

// A real `.m3u` sitting next to the tracks, with paths relative to itself — the
// form the format prescribes. The journey loads it through the browse tree, so
// this file is what proves the whole path: listed apart from the audio files,
// carrying a different action, parsed and resolved by the plugin.
writeFileSync(join(albumNative, 'tout.m3u'), tracks.map((n) => `${n}.mp3`).join('\n') + '\n')

// The device wizard opens on the mounted volumes, read from `/proc/mounts`.
// A journey has no privilege and can mount nothing, so it *describes* a volume
// instead of creating one: the plugin reads the table through
// `RITORNELLO_FILES_PROC_MOUNTS`, which exists for exactly this.
//
// `proc` is listed on purpose. Without it the journey would only prove that a
// declared volume is browsable; with it, it also proves that a pseudo
// filesystem is kept out of the list — the guard that stops a recursive add
// from walking into `/proc/self`.
const procMounts = `${configDir}/proc-mounts`
writeFileSync(
  join(configDirNative, 'proc-mounts'),
  `/dev/sda1 ${configDir} ext4 rw,relatime 0 0\nproc /proc proc rw,relatime 0 0\n`,
)

// A folder with a deliberately long name, sitting next to the fixtures inside
// the volume the wizard opens on.
//
// It exists for one assertion only: that the dialog does not overflow its own
// box. `DialogContent` is a grid, and a grid child's minimum width defaults to
// its content's — so a long name used to push the panel past its white
// background, painting the scrollbar and the buttons outside it. jsdom has no
// layout engine and cannot see that; Playwright measures it.
const LONG_NAME =
  'Un nom de dossier volontairement tres long pour eprouver la mise en page de la popin'
mkdirSync(join(configDirNative, LONG_NAME), { recursive: true })

// A fake `smbclient`, so the network wizard can be played end to end on a
// machine with no NAS — and, more to the point, on anyone's machine. It prints
// output *captured from a real Synology*, so the parsing is exercised against
// what it will actually meet rather than against a reconstruction.
const fakeBinDir = `${configDir}/bin`
mkdirSync(join(configDirNative, 'bin'), { recursive: true })
{
  const cible = join(configDirNative, 'bin', 'smbclient')
  copyFileSync(join(rootNative, 'web', 'app', 'e2e', 'fake-smbclient.sh'), cible)
  chmodSync(cible, 0o755)
}

// Neither `admin = true` nor `kind` appears here: both fields are gone from
// the manifest (see plugins.rs). The core binds one registration socket per
// plugin before spawning it, then launches the binary with `--register`,
// `--name` and `--socket-prefix`; the plugin binds its own sockets and only
// then announces itself on the registration socket with a single line of
// JSON naming its kinds and whether it carries an admin page. An old file
// still carrying `kind` loads fine, serde ignores it.
//
// Its position in the file does not decide the starting source: the core sorts
// `source_order` by name and starts on the *persisted* source, which a fresh
// state.json sets to `radio`. `files` is therefore one `SourceCycle` away —
// and a second one comes back, which is what lets the journey put the harness
// back the way it found it.
writeFileSync(
  join(configDirNative, 'plugins.toml'),
  `[[plugin]]
name = "radio"
exec = "${root}/target/debug/ritornello-plugin-radio"

[[plugin]]
name = "files"
exec = "${root}/target/debug/ritornello-plugin-files"

[[plugin]]
name = "generic-input"
exec = "${root}/target/debug/ritornello-plugin-generic-input"
`,
)
writeFileSync(
  join(configDirNative, 'stations.toml'),
  '[[stations]]\nname = "FIP"\nurl = "http://icecast.radiofrance.fr/fip-midfi.mp3"\npreset = 1\n',
)

// mpv gets its own configuration directory, with a null audio output in it.
//
// The journeys really start a playback, and that playback used to need a
// working sound card. On a machine without one -- a CI runner, precisely --
// mpv finds no output at all (measured on ubuntu-latest: ALSA answers
// "Unknown PCM default", then mpv gives up with "Could not open/initialize
// audio device -> no sound" and ends the file on an error), so
// `[data-position]` never appears and the `files` journey fails on its
// playback step. Worse, it then leaves the active source on `files`, which
// knocks over the three journeys that expect the persisted `radio`: one
// missing sound card, four red tests.
//
// `ao=null` is paced like a real device (mpv's null output is timed, not
// untimed), so the position still advances -- which is all the journeys
// observe. Two side benefits: the run no longer plays a 440 Hz sine out loud
// on the developer's speakers, and `XDG_CONFIG_HOME` pointing here shields
// the journeys from whatever sits in the developer's own ~/.config/mpv.
mkdirSync(join(configDirNative, 'mpv'), { recursive: true })
writeFileSync(join(configDirNative, 'mpv', 'mpv.conf'), 'ao=null\n')

const env = {
  // See the header: 0.0.0.0 under Windows (reachable from the host on
  // 127.0.0.1 through WSL2 forwarding), 127.0.0.1 under native Linux
  // (same machine, no VM crossing).
  RITORNELLO_HTTP: isWindows ? '0.0.0.0:8099' : '127.0.0.1:8099',
  RITORNELLO_PLUGINS: `${configDir}/plugins.toml`,
  RITORNELLO_STATE: `${execDir}/state.json`,
  RITORNELLO_RUNTIME_DIR: execDir,
  RITORNELLO_MPV_SOCKET: `${execDir}/mpv.sock`,
  RITORNELLO_RADIO_STATIONS: `${configDir}/stations.toml`,
  RITORNELLO_RADIO_STATE: `${execDir}/plugin-radio.json`,
  RITORNELLO_INPUT_BINDINGS: `${execDir}/input-bindings.toml`,
  RITORNELLO_INPUT_PRESETS: `${root}/deploy/input-presets`,
  // Every file the `files` plugin writes goes to the throwaway execution
  // directory. Its defaults are `/etc/ritornello` and `/var/lib/ritornello`:
  // left alone, a journey run on a machine where Ritornello is installed would
  // overwrite the owner's roots table, playlist and saved lists. The directory
  // itself is created by the core before any plugin starts (the plugin sockets
  // live there), so none of these paths needs pre-creating here.
  RITORNELLO_FILES_ROOTS: `${execDir}/media-roots.toml`,
  RITORNELLO_FILES_CREDENTIALS: `${execDir}/media-credentials`,
  RITORNELLO_FILES_STATE: `${execDir}/plugin-files.json`,
  RITORNELLO_FILES_MPV_PLAYLIST: `${execDir}/plugin-files.m3u`,
  RITORNELLO_FILES_PLAYLISTS: `${execDir}/playlists`,
  RITORNELLO_FILES_PROC_MOUNTS: procMounts,
  // Read by mpv, not by the core (which knows only its own
  // `RITORNELLO_*`): this is what hands it the `ao=null` written just
  // above, on both branches of the launch below.
  XDG_CONFIG_HOME: configDir,
}

// Fixed name (not the random one of the throwaway directory):
// `teardown.mjs` runs in a distinct node process, launched independently
// by Playwright, and must be able to find this state while sharing
// nothing but the filesystem. `files.spec.ts` reads it too, for the same
// reason: the fixtures root is drawn at random here, and the journey has to
// type it into the page as the core sees it (a `/mnt/c/...` path under
// Windows), not as Windows spells it.
const statePath = join(rootNative, 'target', 'e2e-state.json')

let child

if (isWindows) {
  // PID file next to the execution directory (not inside it): the latter
  // does not exist yet at this point (the core creates it itself on the
  // first `create_dir_all`), whereas `/tmp` always exists.
  const pidFile = `${execDir}.pid`
  const affectations = Object.entries(env)
    .map(([cle, valeur]) => `${cle}='${valeur}'`)
    .join(' ')
  // `echo $$` then `exec`: `exec` replaces the shell's image with the
  // core's while keeping the PID — the file written here therefore really
  // names the core's future real PID, reachable by a later, independent
  // `wsl.exe` call (WSL2 is a single VM, shared between all `wsl.exe`
  // calls, so PIDs stay valid from one call to the next).
  const scriptLancementNative = join(configDirNative, 'lancer.sh')
  // `PATH` is exported rather than passed through `env KEY='value'`: the
  // assignments above are single-quoted, so a `$PATH` written there would reach
  // the plugin literally instead of expanded — and the fake `smbclient` would
  // shadow nothing while the real `PATH` would be destroyed.
  writeFileSync(
    scriptLancementNative,
    `#!/usr/bin/env bash\necho $$ > '${pidFile}'\nexport PATH='${fakeBinDir}':"$PATH"\nexec env ${affectations} '${root}/target/debug/ritornello-core'\n`,
  )
  chmodSync(scriptLancementNative, 0o755)
  writeFileSync(
    statePath,
    JSON.stringify({ isWindows, configDirNative, execDir, pidFile, mediaRoot }, null, 2),
  )
  child = spawn('wsl.exe', ['--', 'bash', `${configDir}/lancer.sh`], { stdio: 'inherit' })
} else {
  writeFileSync(statePath, JSON.stringify({ isWindows, configDirNative, mediaRoot }, null, 2))
  child = spawn(`${root}/target/debug/ritornello-core`, {
    stdio: 'inherit',
    // Same reason as the `export PATH` of the Windows branch: the fake
    // `smbclient` has to come first, without losing the real `PATH`.
    env: { ...process.env, ...env, PATH: `${fakeBinDir}:${process.env.PATH ?? ''}` },
  })
}

// Safety net for the cases where this process really receives the signal
// (e.g. Ctrl+C in development, outside Playwright's `taskkill /T /F`):
// under Linux, this `kill` reaches the core directly; under Windows it
// only targets the Windows-side `wsl.exe` process — the shutdown that
// matters remains `teardown.mjs`'s.
process.on('SIGTERM', () => child.kill('SIGTERM'))
process.on('exit', () => child.kill('SIGTERM'))
