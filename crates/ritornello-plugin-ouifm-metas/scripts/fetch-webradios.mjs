// Regenere src/webradios.toml depuis la source de verite d'OUI FM.
//
// La liste des webradios et leurs deux identifiants vivent dans une variable
// JavaScript `apidata` de la page du lecteur : chaque entree porte `id`
// (identifiant de flux, present dans l'URL du flux audio) et `idMds`
// (identifiant attendu par le flux de metadonnees). Aucune API publique ne
// donne cette liste — l'endpoint GraphQL du site n'expose pas de requete
// connue pour les flux —, donc la table est embarquee dans le plugin plutot
// que relue au demarrage : une extraction par expression reguliere sur une
// page HTML est trop fragile pour un appareil qui doit demarrer sans
// surveillance, et son echec serait silencieux.
//
// Ce script existe pour que la provenance de la table soit executable et non
// pas seulement racontee, et pour la rafraichir sans travail manuel le jour ou
// OUI FM ajoutera ou retirera une webradio.
//
// Usage : node scripts/fetch-webradios.mjs
//         node scripts/fetch-webradios.mjs --verifier   (n'ecrit rien, sort en
//         erreur si la table livree differe de la source — utile en revue)
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const PAGE = 'https://www.ouifm.fr/player'
const CIBLE = join(dirname(fileURLToPath(import.meta.url)), '..', 'src', 'webradios.toml')

// Mounts Icecast historiques (diffusion Infomaniak), **absents d'`apidata`** qui
// ne connait que les URL `streams.lesindesradios.fr`. Ce sont pourtant celles
// qu'on rencontre en pratique : ce sont les URL publiees de longue date, donc
// celles qu'un annuaire comme Radio Browser reference et qu'un utilisateur
// copie. Sans elles, une station OUI FM ajoutee normalement n'etait reconnue
// par aucune entree de la table.
//
// Releves par l'en-tete `icy-name` de chaque flux, et verifies par recoupement :
// le titre ICY d'`ouifm5` et le flux de metadonnees de Rock Inde annoncaient le
// meme morceau au meme instant (« Made Up - TAHITI 80 » / « TAHITI 80 / MADE
// UP »). Seule l'entree principale est **deduite** et non recoupee : hote et
// mount sans numero, et c'est le seul flux dont l'ICY ne porte qu'un texte de
// remplissage — ce qui est precisement la raison d'etre du flux de metadonnees.
//
// Les hotes ouifm4 et ouifm6 a ouifm9 repondent sans `icy-name` : rien ne permet
// de les rattacher a une webradio, ils ne sont donc pas repris.
//
// Chaque fragment est cherche comme sous-chaine de l'URL ; le point final de
// `ouifmN.` fait qu'il reconnait aussi bien l'hote (`ouifm3.ice...`) que le
// mount (`.../ouifm3.mp3`), quel que soit le format servi.
const MOUNTS_HERITES = {
  2174546520932614531n: ['ouifm-high'], // OUI FM (deduit, voir ci-dessus)
  3134161803443976382n: ['ouifm2.'], // icy-name: OUI FM Alternatif
  3134161803443976427n: ['ouifm3.'], // icy-name: OUI FM Classic Rock
  3134161803443976526n: ['ouifm5.'], // icy-name: OUI FM Rock Indé
}

/** Extrait l'objet JSON affecte a `apidata` dans le source de la page. */
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
// Un mount historique rattache a un `idMds` disparu serait perdu en silence :
// autant le dire, c'est le signe qu'OUI FM a retire ou renumerote une webradio.
const connus = new Set(flux.map((f) => BigInt(f.idMds)))
for (const id of Object.keys(MOUNTS_HERITES).map(BigInt)) {
  if (!connus.has(id)) {
    console.warn(`avertissement: mount historique rattache a ${id}, absent d'apidata`)
  }
}

// La date n'est pas relue de l'existant : elle date le releve, pas le fichier.
const rendu = toml(flux, new Date().toISOString().slice(0, 10))

if (process.argv.includes('--verifier')) {
  // Comparaison hors ligne de datation : seul le contenu des entrees compte.
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
