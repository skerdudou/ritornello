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
import { chmodSync, mkdirSync, mkdtempSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const estWindows = process.platform === 'win32'
const racineNative = process.cwd().replace(/[\\/]web[\\/]app$/, '')

// Converts a Windows path (`C:\a\b`) into its WSL equivalent
// (`/mnt/c/a/b`): the only way for a Linux process, launched from Windows
// through `wsl.exe`, to find the files Node wrote here.
function versWsl(cheminWindows) {
  const normalise = cheminWindows.replace(/\\/g, '/')
  const correspondance = /^([A-Za-z]):\/(.*)$/.exec(normalise)
  return correspondance ? `/mnt/${correspondance[1].toLowerCase()}/${correspondance[2]}` : normalise
}

// Under Windows, the configuration directory is created under the repo
// tree (hence under a `/mnt/c/...` mount point predictable for WSL2)
// rather than in the system `tmpdir()`, whose root (often `AppData`)
// offers no such guarantee.
mkdirSync(join(racineNative, 'target'), { recursive: true })
const dirConfigNative = estWindows
  ? mkdtempSync(join(racineNative, 'target', 'e2e-'))
  : mkdtempSync(join(tmpdir(), 'ritornello-e2e-'))

const racine = estWindows ? versWsl(racineNative) : racineNative
const dirConfig = estWindows ? versWsl(dirConfigNative) : dirConfigNative
// Execution directory (sockets, PID): WSL-native under Windows — see the
// header —, same as the configuration directory under native Linux where
// the question does not arise.
const dirExec = estWindows ? `/tmp/ritornello-e2e-${randomBytes(6).toString('hex')}` : dirConfig

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
const mediaNative = join(dirConfigNative, 'media')
const albumNative = join(mediaNative, 'Album')
const mediaRoot = `${dirConfig}/media`
mkdirSync(albumNative, { recursive: true })
const pistes = ['01', '02', '03']
{
  // Through a `.sh` file, and for the same reason as the launch below: an
  // inline command crossing Node -> wsl.exe -> bash sometimes gets corrupted
  // when it mixes quotes and `$`, whereas a path does not.
  const script =
    `#!/usr/bin/env bash\n` +
    `command -v ffmpeg >/dev/null 2>&1 || exit 1\n` +
    pistes
      .map(
        (n) =>
          `ffmpeg -loglevel error -y -f lavfi -i 'sine=frequency=440:duration=1' ` +
          `'${mediaRoot}/Album/${n}.mp3' || exit 1`,
      )
      .join('\n') +
    `\n`
  writeFileSync(join(dirConfigNative, 'fixtures.sh'), script)
  if (estWindows) {
    spawnSync('wsl.exe', ['--', 'bash', `${dirConfig}/fixtures.sh`], { stdio: 'inherit' })
  } else {
    spawnSync('bash', [`${dirConfig}/fixtures.sh`], { stdio: 'inherit' })
  }
  for (const n of pistes) {
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

// `files` is declared **without** `admin = true`: that field no longer exists
// (see plugins.rs) — the core offers `--admin-socket` to every plugin, and the
// one with a page declares it by *binding* that socket. An old file carrying
// the field still loads, serde ignores it.
//
// Its position in the file does not decide the starting source: the core sorts
// `source_order` by name and starts on the *persisted* source, which a fresh
// state.json sets to `radio`. `files` is therefore one `SourceCycle` away —
// and a second one comes back, which is what lets the journey put the harness
// back the way it found it.
writeFileSync(
  join(dirConfigNative, 'plugins.toml'),
  `[[plugin]]
name = "radio"
kind = "source"
exec = "${racine}/target/debug/ritornello-plugin-radio"

[[plugin]]
name = "files"
kind = "source"
exec = "${racine}/target/debug/ritornello-plugin-files"

[[plugin]]
name = "generic-input"
kind = "input"
exec = "${racine}/target/debug/ritornello-plugin-generic-input"
`,
)
writeFileSync(
  join(dirConfigNative, 'stations.toml'),
  '[[stations]]\nname = "FIP"\nurl = "http://icecast.radiofrance.fr/fip-midfi.mp3"\npreset = 1\n',
)

const env = {
  // See the header: 0.0.0.0 under Windows (reachable from the host on
  // 127.0.0.1 through WSL2 forwarding), 127.0.0.1 under native Linux
  // (same machine, no VM crossing).
  RITORNELLO_HTTP: estWindows ? '0.0.0.0:8099' : '127.0.0.1:8099',
  RITORNELLO_PLUGINS: `${dirConfig}/plugins.toml`,
  RITORNELLO_STATE: `${dirExec}/state.json`,
  RITORNELLO_RUNTIME_DIR: dirExec,
  RITORNELLO_MPV_SOCKET: `${dirExec}/mpv.sock`,
  RITORNELLO_RADIO_STATIONS: `${dirConfig}/stations.toml`,
  RITORNELLO_RADIO_STATE: `${dirExec}/plugin-radio.json`,
  RITORNELLO_INPUT_BINDINGS: `${dirExec}/input-bindings.toml`,
  RITORNELLO_INPUT_PRESETS: `${racine}/deploy/input-presets`,
  // Every file the `files` plugin writes goes to the throwaway execution
  // directory. Its defaults are `/etc/ritornello` and `/var/lib/ritornello`:
  // left alone, a journey run on a machine where Ritornello is installed would
  // overwrite the owner's roots table, playlist and saved lists. The directory
  // itself is created by the core before any plugin starts (the plugin sockets
  // live there), so none of these paths needs pre-creating here.
  RITORNELLO_FILES_ROOTS: `${dirExec}/media-roots.toml`,
  RITORNELLO_FILES_CREDENTIALS: `${dirExec}/media-credentials`,
  RITORNELLO_FILES_STATE: `${dirExec}/plugin-files.json`,
  RITORNELLO_FILES_MPV_PLAYLIST: `${dirExec}/plugin-files.m3u`,
  RITORNELLO_FILES_PLAYLISTS: `${dirExec}/playlists`,
}

// Fixed name (not the random one of the throwaway directory):
// `teardown.mjs` runs in a distinct node process, launched independently
// by Playwright, and must be able to find this state while sharing
// nothing but the filesystem. `files.spec.ts` reads it too, for the same
// reason: the fixtures root is drawn at random here, and the journey has to
// type it into the page as the core sees it (a `/mnt/c/...` path under
// Windows), not as Windows spells it.
const etatPath = join(racineNative, 'target', 'e2e-etat.json')

let enfant

if (estWindows) {
  // PID file next to the execution directory (not inside it): the latter
  // does not exist yet at this point (the core creates it itself on the
  // first `create_dir_all`), whereas `/tmp` always exists.
  const pidFile = `${dirExec}.pid`
  const affectations = Object.entries(env)
    .map(([cle, valeur]) => `${cle}='${valeur}'`)
    .join(' ')
  // `echo $$` then `exec`: `exec` replaces the shell's image with the
  // core's while keeping the PID — the file written here therefore really
  // names the core's future real PID, reachable by a later, independent
  // `wsl.exe` call (WSL2 is a single VM, shared between all `wsl.exe`
  // calls, so PIDs stay valid from one call to the next).
  const scriptLancementNative = join(dirConfigNative, 'lancer.sh')
  writeFileSync(
    scriptLancementNative,
    `#!/usr/bin/env bash\necho $$ > '${pidFile}'\nexec env ${affectations} '${racine}/target/debug/ritornello-core'\n`,
  )
  chmodSync(scriptLancementNative, 0o755)
  writeFileSync(
    etatPath,
    JSON.stringify({ estWindows, dirConfigNative, dirExec, pidFile, mediaRoot }, null, 2),
  )
  enfant = spawn('wsl.exe', ['--', 'bash', `${dirConfig}/lancer.sh`], { stdio: 'inherit' })
} else {
  writeFileSync(etatPath, JSON.stringify({ estWindows, dirConfigNative, mediaRoot }, null, 2))
  enfant = spawn(`${racine}/target/debug/ritornello-core`, {
    stdio: 'inherit',
    env: { ...process.env, ...env },
  })
}

// Safety net for the cases where this process really receives the signal
// (e.g. Ctrl+C in development, outside Playwright's `taskkill /T /F`):
// under Linux, this `kill` reaches the core directly; under Windows it
// only targets the Windows-side `wsl.exe` process — the shutdown that
// matters remains `teardown.mjs`'s.
process.on('SIGTERM', () => enfant.kill('SIGTERM'))
process.on('exit', () => enfant.kill('SIGTERM'))
