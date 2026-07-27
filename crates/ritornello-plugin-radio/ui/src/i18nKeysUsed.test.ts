// Garde-fou : la migration vers la SPA a supprime les constantes
// `PAGE_KEYS` qui garantissaient que toute cle utilisee par l'IHM existe
// dans le catalogue anglais embarque. Sans cette garantie, une cle absente
// s'affiche telle quelle a l'ecran au lieu d'echouer au build ou aux tests.
// Ce test recollecte les cles reellement appelees par le code source de ce
// plugin et verifie leur presence dans
// `crates/ritornello-plugin-radio/src/locales/en.toml` + le vocabulaire
// commun `crates/ritornello-i18n/src/locales/common_en.toml` (le plugin
// consomme les deux, la couche commune etant fusionnee par
// `Catalog::entries`).
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

// Chemins resolus via `process.cwd()` (le repertoire du paquet, cf. le
// script npm `test`) plutot que via `import.meta.url` : sous vitest en
// environnement `jsdom`, une URL relative qui remonte hors de la racine du
// projet vite est reecrite en `http://localhost/@fs/...` au lieu de rester
// un `file://` — `fileURLToPath` leve alors `The URL must be of scheme
// file`. `process.cwd()` est un chemin OS brut, insensible a cette
// reecriture.
const RACINE_PAQUET = process.cwd()

// Lecteur TOML plat ecrit a la main : le format des catalogues embarques
// est une simple suite de lignes `cle = "valeur"`, jamais de table
// `[[...]]`. Suffisant ici, inutile d'ajouter une dependance TOML.
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

// Cles appelees via `t('...')` (template) ou `t.value('...')` (script) avec
// une cle litterale. LIMITE CONNUE, assumee : une cle construite
// dynamiquement (variable, template literal, propriete calculee) echapperait
// a cette regex. Ce plugin n'en a aucune a ce jour (pas de liste `ACTIONS`
// ni equivalent, contrairement a generic-input) : toutes les cles de
// `RadioAdmin.vue` sont des litteraux directs.
function clesAppelsLitteraux(fichiers: string[]): Set<string> {
  const cles = new Set<string>()
  const motif = /\bt(?:\.value)?\(\s*['"]([A-Za-z0-9_]+)['"]/g
  for (const fichier of fichiers) {
    const contenu = readFileSync(fichier, 'utf8')
    for (const m of contenu.matchAll(motif)) if (m[1]) cles.add(m[1])
  }
  return cles
}

describe('cles i18n utilisees par le plugin radio', () => {
  it('existent toutes dans le catalogue anglais embarque (plugin + commun)', () => {
    const catalogue = new Set([
      ...clesToml(resolve(RACINE_PAQUET, '../src/locales/en.toml')),
      ...clesToml(resolve(RACINE_PAQUET, '../../ritornello-i18n/src/locales/common_en.toml')),
    ])

    const utilisees = clesAppelsLitteraux(fichiersSource(join(RACINE_PAQUET, 'src')))

    const manquantes = [...utilisees].filter((cle) => !catalogue.has(cle)).sort()
    expect(manquantes).toEqual([])
  })
})
