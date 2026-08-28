// Garde-fou : la migration vers la SPA a supprime les constantes
// `PAGE_KEYS` qui garantissaient que toute cle utilisee par l'IHM existe
// dans le catalogue anglais embarque. Sans cette garantie, une cle absente
// s'displayed telle quelle a l'ecran au lieu d'fail au build ou aux tests
// (c'est exactement ce qui s'etait produit pour trois messages du shell,
// voir le commentaire dans PluginView.ts). Ce test recollecte les cles
// reellement appelees par le code source et verifie leur presence dans
// `crates/ritornello-core/src/locales/en.toml` + le vocabulaire commun
// `crates/ritornello-i18n/src/locales/common_en.toml` (le shell consomme
// les deux, la couche commune etant fusionnee par `Catalog::entries`).
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { describe, expect, it } from 'vitest'
import { LINK_LABEL } from './components/links'
import { REMOTE_COMMANDS } from './views/remoteCommands'

// Chemins resolus via `process.cwd()` (le repertoire du paquet, cf. le
// script npm `test`) plutot que via `import.meta.url` : sous vitest en
// environnement `jsdom`, une URL relative qui remonte hors de la root du
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
// dynamiquement (variable, template literal, propriete calculee, comme
// `t(c.key)` ou `t(erreur.value)`) echappe a cette regex. C'est pourquoi les
// cles ci-dessous qui ne sont jamais ecrites en dur dans un appel `t(...)`
// sont ajoutees explicitement plus loin.
function clesAppelsLitteraux(files: string[]): Set<string> {
  const cles = new Set<string>()
  const motif = /\bt(?:\.value)?\(\s*['"]([A-Za-z0-9_]+)['"]/g
  for (const fichier of files) {
    const contenu = readFileSync(fichier, 'utf8')
    for (const m of contenu.matchAll(motif)) if (m[1]) cles.add(m[1])
  }
  return cles
}

describe('cles i18n utilisees par le shell', () => {
  it('existent toutes dans le catalogue anglais embarque (core + commun)', () => {
    const catalogue = new Set([
      ...clesToml(resolve(RACINE_PAQUET, '../../crates/ritornello-core/src/locales/en.toml')),
      ...clesToml(
        resolve(RACINE_PAQUET, '../../crates/ritornello-i18n/src/locales/common_en.toml'),
      ),
    ])

    const utilisees = new Set([
      ...clesAppelsLitteraux(fichiersSource(join(RACINE_PAQUET, 'src'))),
      // views/remoteCommands.ts : indexees par `.key`, jamais ecrites en dur
      // dans un appel `t(...)` (HomeView.vue fait `t(c.key)`).
      ...REMOTE_COMMANDS.map((c) => c.key),
      // PluginView.ts : `erreur` est une ref typee `'plugin_unavailable' |
      // 'plugin_contract_mismatch' | null`, jamais un litteral passe a
      // `t(...)` (c'est `t(erreur.value)`).
      'plugin_unavailable',
      'plugin_contract_mismatch',
      // SystemView.vue : le message de succes de `waitForReturn` arrive en
      // parametre (`t.value(cleSucces)`), la cle etant choisie par l'action
      // confirmee. Les deux litteraux existent bien dans le fichier, mais au
      // point d'appel et non dans un `t(...)` — la regex ne les voit donc step.
      'system_restarted',
      'system_device_restarted',
      // components/links.ts : les libelles des plateformes d'ecoute sont
      // indexes par `platform` (`t(LINK_LABEL[lien.platform])`), donc jamais
      // ecrits en dur dans un appel `t(...)` — la regex ne les voit step, c'est
      // cet ajout explicite qui les couvre.
      ...Object.values(LINK_LABEL),
    ])

    const manquantes = [...utilisees].filter((cle) => !catalogue.has(cle)).sort()
    expect(manquantes).toEqual([])
  })
})
