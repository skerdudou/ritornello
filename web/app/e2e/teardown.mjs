// Explicit shutdown of the throwaway core launched by `serve.mjs`
// (globalTeardown of playwright.config.ts, run after all the journeys,
// whatever their outcome).
//
// Under Windows, Playwright stops the `node e2e/serve.mjs` process with
// `taskkill /pid <pid> /T /F` (see launchProcess in playwright-core:
// `attemptToGracefullyClose` always throws there on win32, so the forced
// path is what systematically plays). That `taskkill` only kills the
// *Windows* process tree: `wsl.exe` is part of it and dies, but the Linux
// process it launched inside the WSL2 VM is not in that tree and survives
// it — a core then stays alive, holding port 8099 against the next run.
// Hence this independent shutdown, which does not depend on the fate of
// the webServer's node process.
//
// Two-step fallback, executed by a standalone `wsl.exe`:
//  1. `kill -TERM` on the core's real PID, found through the file written
//     WSL-side by serve.mjs (reliable: `exec` kept the PID at launch).
//  2. A `pgrep -f` on the temporary execution directory, which picks up
//     mpv and the plugins: the core does not kill its children on a mere
//     SIGTERM (their `kill_on_drop` only plays on a normal return from
//     `main`, not on a signal's default disposition), so only a sweep by
//     directory guarantees no process survives. mpv and the plugins
//     receive their socket path under that directory (see
//     `RITORNELLO_MPV_SOCKET`, `--socket`), which therefore shows up in
//     their command line.
//
// Two pitfalls verified empirically, both worked around below:
//  - a `pkill -f <pattern>` also kills the shell invoking it if that
//    pattern is itself present in ITS own command line — which is exactly
//    the case here, since we search for the execution directory having
//    first written it into the script doing the search. `pkill` excludes
//    itself but not its parent shell, which therefore dies mid-script
//    before the following lines. Hence the `pgrep` loop + explicit filter
//    on `$$` rather than a plain `pkill`.
//  - an inline command passed as an argument (`bash -lc '<script>'`)
//    across Node -> wsl.exe -> bash sometimes gets corrupted on the way
//    when it combines single/double quotes, `$(...)` and `$$` (exact
//    cause not identified with certainty, plausibly a re-interpretation
//    by one of the Windows/WSL interop layers) — reproduced with `bash:
//    -c: syntax error near unexpected token` quoting a real PID as the
//    token. Hence writing this script to a `.sh` file, of which only the
//    *path* (no sensitive character) crosses that boundary.
import { spawnSync } from 'node:child_process'
import { existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

export default async function globalTeardown() {
  // Same computation as in serve.mjs: `process.cwd()` is `web/app` (npm
  // puts the process there for a `-w app` script), the state file lives
  // at the repo root, under `target/` (git-ignored).
  const racineNative = process.cwd().replace(/[\\/]web[\\/]app$/, '')
  const etatPath = join(racineNative, 'target', 'e2e-etat.json')
  if (!existsSync(etatPath)) return
  const etat = JSON.parse(readFileSync(etatPath, 'utf8'))

  if (etat.estWindows) {
    // `balayer`: kills everything matching the pattern, explicitly
    // excluding `$$` (the shell executing this script — see the note
    // above on `pkill -f`/`pgrep -f` self-matching).
    const balayer = (signal) =>
      `for pid in $(pgrep -f '${etat.dirExec}'); do [ "$pid" = "$$" ] && continue; kill -${signal} "$pid" 2>/dev/null; done`
    const script =
      `#!/usr/bin/env bash\n` +
      `kill -TERM "$(cat '${etat.pidFile}' 2>/dev/null)" 2>/dev/null\n` +
      `sleep 0.5\n` +
      `${balayer('TERM')}\n` +
      `sleep 0.3\n` +
      `${balayer('KILL')}\n` +
      // Execution directory (WSL-native, under /tmp) and PID file: out of
      // reach of `rmSync` on the Windows side, cleaned up here WSL-side.
      `rm -rf '${etat.dirExec}' '${etat.pidFile}'\n`
    const scriptNative = join(etat.dirConfigNative, 'arreter.sh')
    const scriptWsl = `${versWsl(etat.dirConfigNative)}/arreter.sh`
    writeFileSync(scriptNative, script)
    spawnSync('wsl.exe', ['--', 'bash', scriptWsl], { stdio: 'inherit' })
  }
  // Under native Linux, the group SIGKILL Playwright already sends to the
  // webServer's process (see launchProcess: `detached: true` outside
  // Windows) takes the direct core and its children out in the same call
  // — nothing left to do here.

  try {
    rmSync(etat.dirConfigNative, { recursive: true, force: true })
  } catch {
    // Best effort: a locked file (a socket still held for an instant by a
    // process on its way out) must not fail the journeys.
  }
  try {
    rmSync(etatPath, { force: true })
  } catch {
    // same
  }
}

// Same conversion as in serve.mjs (duplicated: both files are launched as
// independent entry points by Playwright, not modules importing each
// other).
function versWsl(cheminWindows) {
  const normalise = cheminWindows.replace(/\\/g, '/')
  const correspondance = /^([A-Za-z]):\/(.*)$/.exec(normalise)
  return correspondance ? `/mnt/${correspondance[1].toLowerCase()}/${correspondance[2]}` : normalise
}
