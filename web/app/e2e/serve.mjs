// Launches a throwaway core for the Playwright journeys: temporary state
// directory, dedicated port, both UI-bearing plugins declared.
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
//    launch script) — plain files, no problem on that mount;
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
import { spawn } from 'node:child_process'
import { chmodSync, mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
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

writeFileSync(
  join(dirConfigNative, 'plugins.toml'),
  `[[plugin]]
name = "radio"
kind = "source"
exec = "${racine}/target/debug/ritornello-plugin-radio"

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
}

// Fixed name (not the random one of the throwaway directory):
// `teardown.mjs` runs in a distinct node process, launched independently
// by Playwright, and must be able to find this state while sharing
// nothing but the filesystem.
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
    JSON.stringify({ estWindows, dirConfigNative, dirExec, pidFile }, null, 2),
  )
  enfant = spawn('wsl.exe', ['--', 'bash', `${dirConfig}/lancer.sh`], { stdio: 'inherit' })
} else {
  writeFileSync(etatPath, JSON.stringify({ estWindows, dirConfigNative }, null, 2))
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
