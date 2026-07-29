// Regenerates src/webradios.toml from OUI FM's source of truth.
//
// The list of webradios and their two identifiers lives in an `apidata`
// JavaScript variable of the player page: each entry carries `id` (stream
// identifier, present in the audio stream URL) and `idMds` (identifier
// expected by the metadata feed). No public API provides this list — the
// site's GraphQL endpoint exposes no known query for the streams —, so
// the table is embedded in the plugin rather than re-read at startup: a
// regex extraction from an HTML page is too fragile for a device that
// must boot unattended, and its failure would be silent.
//
// This script exists so that the table's provenance is executable rather
// than merely narrated, and to refresh it without manual work the day
// OUI FM adds or removes a webradio.
//
// Usage: node scripts/fetch-webradios.mjs
//        node scripts/fetch-webradios.mjs --verifier   (writes nothing,
//        exits nonzero if the bundled table differs from the source —
//        useful in review)
//
// Note: the generated file's header (`entete` below) and the diagnostic
// messages stay in French on purpose — the header is committed as
// src/webradios.toml and locked by the --verifier comparison.
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const PAGE = 'https://www.ouifm.fr/player'
const CIBLE = join(dirname(fileURLToPath(import.meta.url)), '..', 'src', 'webradios.toml')

// Historical Icecast mounts (Infomaniak distribution), **absent from
// `apidata`** which only knows the `streams.lesindesradios.fr` URLs. Yet
// these are the ones met in practice: they are the long-published URLs,
// hence the ones a directory like Radio Browser references and a user
// copies. Without them, an OUI FM station added the normal way was
// recognized by no table entry.
//
// Collected from each stream's `icy-name` header, and cross-checked: the
// ICY title of `ouifm5` and Rock Indé's metadata feed announced the same
// track at the same instant ("Made Up - TAHITI 80" / "TAHITI 80 / MADE
// UP"). Only the main entry is **inferred** rather than cross-checked:
// host and mount without a number, and it is the only stream whose ICY
// carries nothing but filler text — which is precisely the metadata
// feed's reason to exist.
//
// The ouifm4 and ouifm6 through ouifm9 hosts answer without `icy-name`:
// nothing ties them to a webradio, so they are not carried over.
//
// Each fragment is matched as a substring of the URL; the trailing dot of
// `ouifmN.` makes it match the host (`ouifm3.ice...`) as well as the
// mount (`.../ouifm3.mp3`), whatever format is served.
const MOUNTS_HERITES = {
  2174546520932614531n: ['ouifm-high'], // OUI FM (inferred, see above)
  3134161803443976382n: ['ouifm2.'], // icy-name: OUI FM Alternatif
  3134161803443976427n: ['ouifm3.'], // icy-name: OUI FM Classic Rock
  3134161803443976526n: ['ouifm5.'], // icy-name: OUI FM Rock Indé
}

/** Extracts the JSON object assigned to `apidata` in the page source. */
function apidata(html) {
  const m = /apidata\s*=\s*(\{.*?\})\s*(?:;|<\/script>)/s.exec(html)
  if (!m) throw new Error('variable `apidata` introuvable : la page a change de forme')
  return JSON.parse(m[1])
}

function toml(flux, date) {
  const entete = [
    '# Webradios OUI FM : correspondance entre l’identifiant de flux (présent dans',
    '# l’URL du flux audio) et l’identifiant attendu par le flux de métadonnées.',
    '#',
    '# RELEVÉ, PAS DEVINÉ. Source : la variable JavaScript `apidata` de',
    `# ${PAGE}, dont les champs \`id\` (identifiant de flux) et`,
    '# `idMds` (identifiant de métadonnées) sont repris ici tels quels.',
    '# `scripts/fetch-webradios.mjs` régénère ce fichier depuis cette même source.',
    '#',
    '# Vérifié à la main sur `?id=` du flux de métadonnées : l’identifiant `metas`',
    '# renvoie artiste et titre, l’identifiant de flux renvoie une trame vide.',
    '#',
    '# Chaque entrée de `urls` est cherchée comme sous-chaîne de l’URL du flux :',
    '# l’URL de diffusion porte un jeton signé et un format variables, mais',
    '# toujours l’identifiant de flux. Les fragments `ouifmN.` reconnaissent les',
    '# mounts Icecast historiques (diffusion Infomaniak), qui sont les URL',
    '# publiées de longue date — donc celles qu’un annuaire référence et qu’un',
    '# utilisateur copie. Voir `scripts/fetch-webradios.mjs` pour leur relevé.',
    '#',
    `# Relevé le ${date}. ${flux.length} flux.`,
    '',
  ]
  const corps = flux.flatMap((f) => {
    const fragments = [f.id, ...(MOUNTS_HERITES[BigInt(f.idMds)] ?? [])]
    return [
      '[[webradio]]',
      `label = "${f.label.replace(/"/g, '\\"')}"`,
      `urls = [${fragments.map((u) => `"${u}"`).join(', ')}]`,
      `metas = "${f.idMds}"`,
      '',
    ]
  })
  return [...entete, ...corps].join('\n')
}

const html = await fetch(PAGE, {
  headers: { 'user-agent': 'Mozilla/5.0 (X11; Linux x86_64) Chrome/120' },
}).then((r) => {
  if (!r.ok) throw new Error(`HTTP ${r.status} sur ${PAGE}`)
  return r.text()
})

const data = apidata(html)
const flux = [...(data.radiostreams ?? []), ...(data.webradios ?? [])]
const manquants = flux.filter((f) => !f.id || !f.idMds)
if (!flux.length) throw new Error('aucun flux dans `apidata`')
if (manquants.length) {
  throw new Error(`${manquants.length} flux sans id ou idMds : source inattendue, rien n'est ecrit`)
}
// A historical mount tied to a vanished `idMds` would be lost silently:
// better to say so — it is the sign OUI FM removed or renumbered a webradio.
const connus = new Set(flux.map((f) => BigInt(f.idMds)))
for (const id of Object.keys(MOUNTS_HERITES).map(BigInt)) {
  if (!connus.has(id)) {
    console.warn(`avertissement: mount historique rattache a ${id}, absent d'apidata`)
  }
}

// The date is not re-read from the existing file: it dates the survey,
// not the file.
const rendu = toml(flux, new Date().toISOString().slice(0, 10))

if (process.argv.includes('--verifier')) {
  // Comparison excluding the date line: only the entries' content counts.
  const sansDate = (t) => t.replace(/^# Relevé le .*$/m, '')
  const livre = readFileSync(CIBLE, 'utf8')
  if (sansDate(livre) !== sansDate(rendu)) {
    console.error(`${CIBLE} differe de la source (${flux.length} flux en ligne)`)
    process.exit(1)
  }
  console.log(`table a jour (${flux.length} flux)`)
} else {
  writeFileSync(CIBLE, rendu)
  console.log(`${CIBLE} : ${flux.length} flux ecrits`)
}
