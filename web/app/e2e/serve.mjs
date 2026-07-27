// Lance un coeur jetable pour les parcours Playwright : repertoire d'etat
// temporaire, port dedie, les deux plugins a IHM declares. Volontairement
// proche de la configuration de developpement de docs/developpement.md.
//
// Particularite de cet atelier : Node/npm/Playwright tournent cote Windows,
// mais les binaires du coeur sont des ELF Linux compiles sous WSL (Node est
// absent de WSL). Ce script detecte donc la plate-forme :
//  - sous Linux (l'environnement documente par docs/developpement.md, et celui d'une
//    eventuelle CI) : lance le binaire directement, comme avant ;
//  - sous Windows : ecrit la configuration dans un repertoire temporaire
//    puis lance le coeur via `wsl.exe -- bash <script.sh>`, avec des chemins
//    `/mnt/c/...` et `RITORNELLO_HTTP=0.0.0.0:8099` — mesure faite, un
//    service lie a 127.0.0.1 *dans* WSL n'est pas joignable depuis Windows,
//    alors qu'un service lie a 0.0.0.0 l'est, sur 127.0.0.1 cote hote.
//
// Sous Windows, deux repertoires distincts sont necessaires, pas un seul :
//  - un repertoire de *configuration*, sous l'arbre du depot (donc visible
//    a la fois de Windows et, via `/mnt/c/...`, de WSL) : il ne contient que
//    les fichiers dont Node genere le contenu (plugins.toml, stations.toml,
//    le script de lancement) — de simples fichiers, aucun probleme sur ce
//    montage ;
//  - un repertoire d'*execution*, natif du systeme de fichiers WSL (sous
//    `/tmp`) : mesure faite, `mpv --input-ipc-server=<chemin>` ne cree pas
//    sa socket Unix quand `<chemin>` est sous `/mnt/c/...` (le montage 9p
//    de DrvFs ne supporte pas les sockets Unix), alors que le meme appel
//    reussit sous `/tmp`. Toutes les sockets (mpv, plugins) et le fichier
//    de PID vivent donc ici — le coeur cree lui-meme ce repertoire (et ceux
//    de state.json, etc.) via `create_dir_all`, inutile de le pre-creer.
//
// Le lancement passe par un fichier `.sh` (plutot qu'un enorme `bash -lc
// '<script inline>'`) : mesure faite, une commande inline combinant guillemets
// simples et doubles, `$(...)` et `$$` a travers Node -> wsl.exe -> bash se
// corrompt parfois en route (cause exacte non identifiee avec certitude —
// vraisemblablement une re-interpretation de l'argument par une des couches
// de l'interop Windows/WSL) ; un chemin de fichier, lui, ne contient aucun
// caractere sensible a ce passage.
//
// L'arret propre est delegue a `teardown.mjs` (voir globalTeardown dans
// playwright.config.ts) : tuer ce process ou `wsl.exe` depuis Windows ne
// tue pas forcement le processus Linux lance a l'interieur de WSL2, donc on
// ecrit ici un fichier d'etat (PID reel cote WSL + repertoire d'execution)
// que `teardown.mjs` pourra retrouver et arreter explicitement, quel que
// soit le sort de *ce* process node.
import { randomBytes } from 'node:crypto'
import { spawn } from 'node:child_process'
import { chmodSync, mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const estWindows = process.platform === 'win32'
const racineNative = process.cwd().replace(/[\\/]web[\\/]app$/, '')

// Convertit un chemin Windows (`C:\a\b`) en son equivalent WSL
// (`/mnt/c/a/b`) : c'est le seul moyen pour un process Linux, lance depuis
// Windows via `wsl.exe`, de retrouver les fichiers ecrits ici par Node.
function versWsl(cheminWindows) {
  const normalise = cheminWindows.replace(/\\/g, '/')
  const correspondance = /^([A-Za-z]):\/(.*)$/.exec(normalise)
  return correspondance ? `/mnt/${correspondance[1].toLowerCase()}/${correspondance[2]}` : normalise
}

// Sous Windows, le repertoire de configuration est cree sous l'arbre du
// depot (donc sous un point de montage `/mnt/c/...` predictible pour WSL2)
// plutot que dans le `tmpdir()` systeme, dont la racine (souvent `AppData`)
// n'offre aucune garantie de ce genre.
mkdirSync(join(racineNative, 'target'), { recursive: true })
const dirConfigNative = estWindows
  ? mkdtempSync(join(racineNative, 'target', 'e2e-'))
  : mkdtempSync(join(tmpdir(), 'ritornello-e2e-'))

const racine = estWindows ? versWsl(racineNative) : racineNative
const dirConfig = estWindows ? versWsl(dirConfigNative) : dirConfigNative
// Repertoire d'execution (sockets, PID) : natif WSL sous Windows — voir
// l'en-tete —, confondu avec le repertoire de configuration sous Linux
// natif ou la question ne se pose pas.
const dirExec = estWindows ? `/tmp/ritornello-e2e-${randomBytes(6).toString('hex')}` : dirConfig

writeFileSync(
  join(dirConfigNative, 'plugins.toml'),
  `[[plugin]]
name = "radio"
kind = "source"
exec = "${racine}/target/debug/ritornello-plugin-radio"
admin = true

[[plugin]]
name = "generic-input"
kind = "input"
exec = "${racine}/target/debug/ritornello-plugin-generic-input"
admin = true
`,
)
writeFileSync(
  join(dirConfigNative, 'stations.toml'),
  '[[stations]]\nname = "FIP"\nurl = "http://icecast.radiofrance.fr/fip-midfi.mp3"\npreset = 1\n',
)

const env = {
  // Voir l'en-tete : 0.0.0.0 sous Windows (joignable depuis l'hote sur
  // 127.0.0.1 via le transfert WSL2), 127.0.0.1 sous Linux natif (meme
  // machine, pas de traversee de VM).
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

// Nom fixe (pas celui, aleatoire, du repertoire jetable) : `teardown.mjs`
// tourne dans un process node distinct, lance independamment par
// Playwright, et doit pouvoir retrouver cet etat sans rien partager d'autre
// que le systeme de fichiers.
const etatPath = join(racineNative, 'target', 'e2e-etat.json')

let enfant

if (estWindows) {
  // Fichier de PID a cote du repertoire d'execution (pas dedans) : ce
  // dernier n'existe pas encore a cet instant (le coeur le cree lui-meme
  // au premier `create_dir_all`), alors que `/tmp` existe toujours.
  const pidFile = `${dirExec}.pid`
  const affectations = Object.entries(env)
    .map(([cle, valeur]) => `${cle}='${valeur}'`)
    .join(' ')
  // `echo $$` puis `exec` : `exec` remplace l'image du shell par celle du
  // coeur tout en conservant le PID — le fichier ecrit ici designe donc
  // bien le futur PID reel du coeur, joignable par un appel `wsl.exe`
  // ulterieur et independant (WSL2 est une VM unique, partagee entre tous
  // les appels `wsl.exe`, donc les PID y restent valides d'un appel a
  // l'autre).
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

// Filet de securite pour les cas ou ce process recoit reellement le signal
// (par ex. Ctrl+C en developpement, hors du `taskkill /T /F` de Playwright) :
// sous Linux, ce `kill` atteint directement le coeur ; sous Windows il ne
// vise que le process `wsl.exe` cote Windows — l'arret qui compte reste
// celui de `teardown.mjs`.
process.on('SIGTERM', () => enfant.kill('SIGTERM'))
process.on('exit', () => enfant.kill('SIGTERM'))
