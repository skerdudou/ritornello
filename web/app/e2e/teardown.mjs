// Arret explicite du coeur jetable lance par `serve.mjs` (globalTeardown de
// playwright.config.ts, execute apres tous les parcours, quel que soit leur
// resultat).
//
// Sous Windows, Playwright arrete le process `node e2e/serve.mjs` par
// `taskkill /pid <pid> /T /F` (voir launchProcess dans playwright-core :
// `attemptToGracefullyClose` y leve toujours sur win32, donc c'est
// systematiquement la voie forcee qui joue). Ce `taskkill` ne tue que
// l'arbre de process *Windows* : `wsl.exe` en fait partie et meurt, mais le
// process Linux qu'il a lance a l'interieur de la VM WSL2 n'est pas dans cet
// arbre et lui survit — un coeur reste alors vivant, bloquant le port 8099
// pour l'execution suivante. D'ou cet arret independant, qui ne depend pas
// du sort du process node du webServer.
//
// Repli en deux temps, execute par un `wsl.exe` autonome :
//  1. `kill -TERM` sur le vrai PID du coeur, retrouve via le fichier ecrit
//     cote WSL par serve.mjs (fiable : `exec` a conserve le PID au lancement).
//  2. Un `pgrep -f` sur le repertoire d'execution temporaire, qui ramasse mpv
//     et les plugins : le coeur ne tue pas ses enfants sur un simple
//     SIGTERM (leur `kill_on_drop` ne joue qu'au retour normal de `main`,
//     pas sur la disposition par defaut d'un signal), donc seul un balayage
//     par repertoire garantit qu'aucun processus ne survit. mpv et les
//     plugins recoivent leur chemin de socket sous ce repertoire (voir
//     `RITORNELLO_MPV_SOCKET`, `--socket`), qui apparait donc dans leur
//     ligne de commande.
//
// Deux pieges verifies empiriquement, tous deux contournes ci-dessous :
//  - un `pkill -f <motif>` tue aussi le shell qui l'invoque si ce motif est
//    lui-meme present dans SA propre ligne de commande — ce qui est
//    exactement le cas ici, puisqu'on cherche le repertoire d'execution en
//    l'ayant d'abord ecrit dans le script qui fait la recherche. `pkill`
//    s'exclut lui-meme mais pas son shell parent, qui meurt donc en plein
//    script avant les lignes suivantes. D'ou la boucle `pgrep` + filtre
//    explicite sur `$$` plutot qu'un simple `pkill`.
//  - une commande inline passee en argument (`bash -lc '<script>'`) a
//    travers Node -> wsl.exe -> bash se corrompt parfois en route quand elle
//    combine guillemets simples/doubles, `$(...)` et `$$` (cause exacte non
//    identifiee avec certitude, vraisemblablement une re-interpretation par
//    une des couches de l'interop Windows/WSL) — reproduit avec `bash: -c:
//    syntax error near unexpected token` citant un PID reel comme jeton.
//    D'ou l'ecriture de ce script dans un fichier `.sh`, dont seul le
//    *chemin* (sans caractere sensible) traverse cette frontiere.
import { spawnSync } from 'node:child_process'
import { existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

export default async function globalTeardown() {
  // Meme calcul que dans serve.mjs : `process.cwd()` est `web/app` (npm y
  // place le process pour un script `-w app`), le fichier d'etat vit a la
  // racine du depot, sous `target/` (ignore de git).
  const racineNative = process.cwd().replace(/[\\/]web[\\/]app$/, '')
  const etatPath = join(racineNative, 'target', 'e2e-etat.json')
  if (!existsSync(etatPath)) return
  const etat = JSON.parse(readFileSync(etatPath, 'utf8'))

  if (etat.estWindows) {
    // `balayer` : tue tout ce qui correspond au motif, en excluant
    // explicitement `$$` (le shell qui execute ce script — voir la note
    // ci-dessus sur l'auto-correspondance de `pkill -f`/`pgrep -f`).
    const balayer = (signal) =>
      `for pid in $(pgrep -f '${etat.dirExec}'); do [ "$pid" = "$$" ] && continue; kill -${signal} "$pid" 2>/dev/null; done`
    const script =
      `#!/usr/bin/env bash\n` +
      `kill -TERM "$(cat '${etat.pidFile}' 2>/dev/null)" 2>/dev/null\n` +
      `sleep 0.5\n` +
      `${balayer('TERM')}\n` +
      `sleep 0.3\n` +
      `${balayer('KILL')}\n` +
      // Repertoire d'execution (natif WSL, sous /tmp) et fichier de PID :
      // hors de portee de `rmSync` cote Windows, nettoyes ici cote WSL.
      `rm -rf '${etat.dirExec}' '${etat.pidFile}'\n`
    const scriptNative = join(etat.dirConfigNative, 'arreter.sh')
    const scriptWsl = `${versWsl(etat.dirConfigNative)}/arreter.sh`
    writeFileSync(scriptNative, script)
    spawnSync('wsl.exe', ['--', 'bash', scriptWsl], { stdio: 'inherit' })
  }
  // Sous Linux natif, le SIGKILL de groupe que Playwright envoie deja au
  // process du webServer (voir launchProcess : `detached: true` hors
  // Windows) emporte le coeur direct et ses enfants dans le meme appel —
  // rien a refaire ici.

  try {
    rmSync(etat.dirConfigNative, { recursive: true, force: true })
  } catch {
    // Meilleur effort : un fichier verrouille (socket encore tenu un
    // instant par un process en cours d'arret) ne doit pas faire echouer
    // les parcours.
  }
  try {
    rmSync(etatPath, { force: true })
  } catch {
    // idem
  }
}

// Meme conversion que dans serve.mjs (dupliquee : les deux fichiers sont
// lances comme des points d'entree independants par Playwright, pas des
// modules qui s'importent l'un l'autre).
function versWsl(cheminWindows) {
  const normalise = cheminWindows.replace(/\\/g, '/')
  const correspondance = /^([A-Za-z]):\/(.*)$/.exec(normalise)
  return correspondance ? `/mnt/${correspondance[1].toLowerCase()}/${correspondance[2]}` : normalise
}
