// Garde-fou repris du plugin radio : toute clé appelée par l'IHM doit exister
// dans le catalogue anglais embarqué du plugin
// (`crates/ritornello-plugin-files/src/locales/en.toml`) ou dans le vocabulaire
// commun (`crates/ritornello-i18n/src/locales/common_en.toml`), que
// `Catalog::entries` fusionne. Sans cette garantie, une clé absente s'affiche
// **telle quelle** à l'écran — l'utilisateur lit « btn_mount_now » — au lieu
// d'échouer au build ou aux tests.
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

// Chemins résolus via `process.cwd()` (le répertoire du paquet, cf. le script
// npm `test`) plutôt que via `import.meta.url` : sous vitest en environnement
// `jsdom`, une URL relative qui remonte hors de la racine du projet vite est
// réécrite en `http://localhost/@fs/...` au lieu de rester un `file://`, et
// `fileURLToPath` lève alors « The URL must be of scheme file ».
const RACINE_PAQUET = process.cwd()

/**
 * Clés de la page **pas encore** dans les catalogues du serveur.
 *
 * Elles y seront ajoutées en même temps que leur traduction française : les
 * deux fichiers (`src/locales/en.toml` et `deploy/locales/files/fr.toml`) sont
 * tenus à la parité par un test côté Rust, donc une clé n'entre que dans les
 * deux à la fois. Cette liste est le contrat entre ce module et ce chantier-là,
 * et elle est vérifiée dans les deux sens : une clé qui y figure alors qu'elle
 * existe désormais dans le catalogue doit en être retirée, sans quoi la liste
 * se transformerait en trou permanent dans le garde-fou.
 */
const EN_ATTENTE: string[] = []

// Lecteur TOML plat écrit à la main : le format des catalogues embarqués est
// une simple suite de lignes `cle = "valeur"`, jamais de table `[[...]]`.
function clesToml(chemin: string): Set<string> {
  const cles = new Set<string>()
  for (const ligneBrute of readFileSync(chemin, 'utf8').split(/\r?\n/)) {
    const ligne = ligneBrute.trim()
    if (!ligne || ligne.startsWith('#')) continue
    const i = ligne.indexOf('=')
    if (i === -1) continue
    const cle = ligne.slice(0, i).trim()
    if (/^[A-Za-z0-9_]+$/.test(cle)) cles.add(cle)
  }
  return cles
}

function fichiersSource(dir: string): string[] {
  const resultats: string[] = []
  for (const entree of readdirSync(dir)) {
    const chemin = join(dir, entree)
    if (statSync(chemin).isDirectory()) {
      resultats.push(...fichiersSource(chemin))
    } else if (/\.(vue|ts)$/.test(entree) && !entree.endsWith('.test.ts')) {
      resultats.push(chemin)
    }
  }
  return resultats
}

// Clés appelées via `t('...')` (gabarit) ou `t.value('...')` (script) avec une
// clé littérale. LIMITE CONNUE, assumée : une clé construite dynamiquement
// échapperait à cette expression. Cette page n'en a aucune — toutes ses clés
// sont des littéraux, y compris dans les branches ternaires.
function clesAppelsLitteraux(fichiers: string[]): Set<string> {
  const cles = new Set<string>()
  const motif = /\bt(?:\.value)?\(\s*['"]([A-Za-z0-9_]+)['"]/g
  for (const fichier of fichiers) {
    for (const m of readFileSync(fichier, 'utf8').matchAll(motif)) if (m[1]) cles.add(m[1])
  }
  return cles
}

describe('clés i18n utilisées par le plugin files', () => {
  const catalogue = new Set([
    ...clesToml(resolve(RACINE_PAQUET, '../src/locales/en.toml')),
    ...clesToml(resolve(RACINE_PAQUET, '../../ritornello-i18n/src/locales/common_en.toml')),
  ])

  it('n’en introduit aucune hors du catalogue et hors de la liste en attente', () => {
    const utilisees = clesAppelsLitteraux(fichiersSource(join(RACINE_PAQUET, 'src')))
    const attendues = new Set(EN_ATTENTE)
    const manquantes = [...utilisees].filter((c) => !catalogue.has(c) && !attendues.has(c)).sort()
    expect(manquantes).toEqual([])
  })

  it('ne garde en attente que des clés réellement absentes du catalogue', () => {
    // Sans cette moitié, `EN_ATTENTE` deviendrait un trou permanent : une clé
    // supprimée du code ou ajoutée au catalogue y resterait, et masquerait la
    // prochaine vraie absence portant le même nom.
    const arrivees = EN_ATTENTE.filter((c) => catalogue.has(c))
    expect(arrivees).toEqual([])
  })
})
