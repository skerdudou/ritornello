// Regenerates src/stations.toml from Radio France's own published sources.
//
// Two sources, in this order of authority:
//
//  1. The Open API documentation (developers.radiofrance.fr). Its example
//     responses list, for every brand and every "ici" local station and FIP
//     webradio, both `liveStream` (hence the Icecast mount) and `playerUrl`
//     (which carries `id_station=<n>`). The two fields sit in the SAME object,
//     so the mount/identifier pairing is written by Radio France rather than
//     reconstructed by us. This covers 61 of the 74 stations.
//
//  2. The site's own webradio cards (radiofrance.fr/fip and /francemusique) for
//     the 13 stations the documentation does not list: the 11 France Musique
//     webradios, plus FIP Sacré français and FIP Cultes. Each card carries the
//     brand slug (`data-brand`) and the station identifier (`cardtitle-<n>`) on
//     the same card.
//
// The mounts of those 13 are NOT derivable from their slug — `francemusique_
// classique_easy` is served as `francemusiqueeasyclassique`, and
// `francemusique_evenementielle` ("Films") as `francemusiquelabo`, i.e. "la
// B.O.". They are listed in MOUNTS_HORS_DOC below and each one is verified
// against icecast.radiofrance.fr on every run: a mount that stopped answering
// is reported rather than written out.
//
// Why this table is embedded rather than fetched at boot: a device that starts
// unattended must not depend on a third party's page to recognize its stations,
// and such a failure would be silent. This script exists so that the table's
// provenance is executable rather than merely narrated, and to refresh it
// without manual work the day Radio France adds or removes a station.
//
// Usage: node scripts/fetch-stations.mjs
//        node scripts/fetch-stations.mjs --verifier   (writes nothing, exits
//        nonzero if the bundled table differs from the sources — useful in
//        review)
//
// Note: the generated file's header (`entete` below) stays in French on
// purpose — it is committed as src/stations.toml and locked by the --verifier
// comparison.
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const UA = 'Mozilla/5.0 (X11; Linux x86_64) Chrome/120'
const DOC = 'https://developers.radiofrance.fr/doc/tutorial-by-example'
const DOC_MARQUES = `${DOC}/list-brands`
const DOC_LOCALES = `${DOC}/list-locals-and-webradios`
const SITE_FIP = 'https://www.radiofrance.fr/fip'
const SITE_FM = 'https://www.radiofrance.fr/francemusique'
const RACINE = join(dirname(fileURLToPath(import.meta.url)), '..')
const CIBLE = join(RACINE, 'src', 'stations.toml')
// The plugin's README lists the same stations. Regenerating it from the same
// run is what stops the two from drifting: a list kept by hand would be right
// on the day it was written and quietly wrong afterwards.
const LISEZMOI = join(RACINE, 'README.md')
const DEBUT = '<!-- stations:auto:début — généré par scripts/fetch-stations.mjs, ne pas éditer à la main -->'
const FIN = '<!-- stations:auto:fin -->'

// The 13 stations absent from the documentation: their mount, keyed by the
// brand slug the site's cards carry. Collected by hand and re-verified against
// icecast.radiofrance.fr at every run (see verifieMounts). None of them can be
// derived from the slug, which is exactly why they are written down.
const MOUNTS_HORS_DOC = {
  fip_sacre_francais: 'fipsacrefrancais',
  fip_cultes: 'fipcultes',
  francemusique_classique_easy: 'francemusiqueeasyclassique',
  francemusique_classique_plus: 'francemusiqueclassiqueplus',
  francemusique_concert_rf: 'francemusiqueconcertsradiofrance',
  francemusique_ocora_monde: 'francemusiqueocoramonde',
  francemusique_la_jazz: 'francemusiquelajazz',
  francemusique_la_contemporaine: 'francemusiquelacontemporaine',
  // "la B.O." — the film-music webradio, which the site labels "Films".
  francemusique_evenementielle: 'francemusiquelabo',
  francemusique_baroque: 'francemusiquebaroque',
  francemusique_opera: 'francemusiqueopera',
  francemusique_piano_zen: 'francemusiquepianozen',
  francemusique_classique_love: 'francemusiqueclassiquelove',
}

// Rendering profile requested per station (last segment of the live URL). The
// choice is NOT cosmetic: the server shapes its answer according to it, and the
// wrong one makes the plugin silent. Measured on Mouv' at one instant:
// `webrf_fip_player` answers "Le direct" / "Mouv'" (the station's baseline),
// while `webrf_mouv_player` answers "La Playlist" / "SOOLKING - Bye Bye (feat.
// TAYC)", which was what actually aired.
//
//  - MORCEAU (`webrf_fip_player`): the answer is the SONG object — title and
//    artist already separated, and the window is the song's. Only the stations
//    below answer this way: FIP, its webradios, and France Musique's.
//  - EMISSION (`webrf_mouv_player`): the answer is the PROGRAMME object, with
//    what is playing inside it as a single "ARTIST - Title" string. It is the
//    only profile that surfaces the current song on Mouv', France Musique and
//    the 45 local stations; on purely spoken stations it returns the same
//    programme/detail pair as the brand's own profile (checked on France
//    Inter, franceinfo and France Culture). Its name is incidental — it is a
//    server-side profile, not a Mouv' endpoint.
const MORCEAU = 'webrf_fip_player'
const EMISSION = 'webrf_mouv_player'

// Commentary attached to a station in the generated file, where the pairing
// deserves an explanation the reader would otherwise have to rediscover.
const NOTES = {
  407: [
    '# Mount « labo » = « la B.O. » (bandes originales), la webradio des musiques de',
    '# films — que le site appelle « Films » et dont le slug de marque est',
    '# `francemusique_evenementielle`. C’est la seule entrée dont le rapprochement',
    '# repose sur l’élimination : après attribution des dix autres webradios France',
    '# Musique, il restait exactement un mount et exactement une station.',
  ],
}

const texte = (u) =>
  fetch(u, { headers: { 'user-agent': UA } }).then((r) => {
    if (!r.ok) throw new Error(`HTTP ${r.status} sur ${u}`)
    return r.text()
  })

/**
 * Line endings, neutralized for comparison.
 *
 * The checkout is a Windows one (the project is developed under Windows +
 * WSL), so git hands these files back with CRLF while this script composes
 * with LF. Comparing them raw made `--verifier` report a drift on every run
 * after a checkout or a rebase — a false alarm on the one check meant to
 * catch a real one.
 */
const memeFins = (t) => t.replace(/\r\n/g, '\n')

/** Rewrites `rendu` with the line endings the file on disk already uses. */
const commeSurDisque = (rendu, actuel) => (actuel.includes('\r\n') ? rendu.replace(/\n/g, '\r\n') : rendu)

/**
 * Strips the HTML while preserving the spaces INSIDE the JSON string literals
 * of a highlighted code block: each token sits in its own element, on its own
 * line, so trimming per line and joining without a separator rebuilds the JSON
 * exactly. Collapsing all whitespace instead would turn "France Bleu Alsace"
 * into "FranceBleuAlsace".
 */
function jsonDeLaPage(html) {
  return html
    .replace(/<[^>]+>/g, '\n')
    .replace(/&quot;/g, '"')
    .replace(/&#x27;/g, "'")
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .split('\n')
    .map((l) => l.trim())
    .join('')
}

/** Every {id, title, playerUrl, liveStream} object of a documentation page. */
function stationsDoc(plat) {
  const out = []
  for (const m of plat.matchAll(/\{"id":"([A-Z_0-9]+)","title":"([^"]*)"[^{}]*?\}/g)) {
    const bloc = m[0]
    const id = /id_station=(\d+)/.exec(bloc)?.[1]
    const mount = /icecast\.radiofrance\.fr\/([a-z0-9]+)-(?:midfi|lofi|hifi)/.exec(bloc)?.[1]
    // FIP à Bordeaux/Nantes/Strasbourg are listed with playerUrl and liveStream
    // both null: they no longer broadcast, so there is nothing to recognize.
    if (!id || !mount) continue
    out.push({ id: Number(id), mount, label: m[2].trim(), source: m[1] })
  }
  return out
}

/**
 * Webradio cards of a site page: brand slug and station identifier sit on the
 * same card. The regex is "tempered" — no other data-brand may come between the
 * two — without which a card would borrow the next one's identifier.
 */
function cartesDuSite(html) {
  const re =
    /data-brand="([a-z_0-9]+)"((?:(?!data-brand=)[\s\S]){0,4000}?)id="cardtitle-(\d+)"((?:(?!data-brand=)[\s\S]){0,1200}?)aria-label="([^"]*)"/g
  const vues = new Map()
  for (const m of html.matchAll(re)) if (!vues.has(m[1])) vues.set(m[1], { id: Number(m[3]), label: m[5].trim() })
  return vues
}

/** Human-readable label for a station taken from a site card. */
function libelle(slug, court) {
  const marque = slug.startsWith('fip_') ? 'FIP' : slug.startsWith('francemusique_') ? 'France Musique' : ''
  return marque ? `${marque} ${court}` : court
}

/** Checks that a mount still answers, so a stale one is reported, not written. */
async function verifieMounts(mounts) {
  const morts = []
  for (const m of mounts) {
    try {
      const c = new AbortController()
      const t = setTimeout(() => c.abort(), 10000)
      const r = await fetch(`https://icecast.radiofrance.fr/${m}-midfi.mp3`, {
        headers: { 'user-agent': UA },
        signal: c.signal,
      })
      clearTimeout(t)
      r.body?.cancel()
      if (r.status !== 200) morts.push(`${m} (HTTP ${r.status})`)
    } catch (e) {
      morts.push(`${m} (${e.message})`)
    }
  }
  return morts
}

function toml(groupes, date, total) {
  const entete = [
    '# Stations Radio France : correspondance entre le mount de diffusion (présent',
    '# dans l’URL du flux audio) et l’identifiant attendu par le point d’entrée du',
    '# direct.',
    '#',
    '# RELEVÉ, PAS DEVINÉ. Source principale : la documentation publique de l’Open',
    '# API de Radio France (https://developers.radiofrance.fr/doc/tutorial-by-example',
    '# /list-brands et /list-locals-and-webradios), dont les réponses d’exemple',
    '# donnent pour chaque station son `liveStream` (donc son mount Icecast) et son',
    '# `playerUrl`, qui porte `id_station=<n>`. Les deux champs viennent de la même',
    '# réponse : la correspondance est écrite par Radio France, pas reconstituée.',
    '#',
    '# Les 13 stations absentes de cette documentation — les webradios France',
    '# Musique, plus FIP Sacré français et FIP Cultes — sont relevées des cartes de',
    '# https://www.radiofrance.fr/francemusique et /fip, où le slug de marque et',
    '# l’identifiant se lisent sur la même carte. Leur mount est vérifié station par',
    '# station contre icecast.radiofrance.fr. `scripts/fetch-stations.mjs` régénère',
    '# ce fichier depuis ces mêmes sources.',
    '#',
    '# Chaque mount est cherché comme **jeton** de l’URL du flux : bordé de part et',
    '# d’autre par un caractère non alphanumérique. C’est ce qui permet une seule',
    '# entrée par station là où la même station se diffuse sous plusieurs formes',
    '# (`icecast.radiofrance.fr/<mount>-midfi.mp3`, le nom historique',
    '# `direct.fipradio.fr/live/<mount>-midfi.mp3`, le HLS',
    '# `stream.radiofrance.fr/<mount>/<mount>.m3u8`, et les qualités `-lofi`,',
    '# `-hifi.aac`) — et ce qui empêche `fip` de capturer `fipgroove`.',
    '#',
    '# `rules` est le **profil de rendu** demandé au serveur, et son choix n’est pas',
    '# cosmétique : c’est lui qui décide si le plugin dit quelque chose. Mesuré au',
    '# même instant sur Mouv’, `webrf_fip_player` répond « Le direct » / « Mouv’ »',
    '# (le slogan) quand `webrf_mouv_player` répond « La Playlist » / « SOOLKING -',
    '# Bye Bye (feat. TAYC) », qui était bien ce qui passait à l’antenne.',
    '#   - `webrf_fip_player` : la réponse est l’objet MORCEAU, titre et artiste',
    '#     déjà séparés, bornes du morceau. Seules FIP, ses webradios et celles de',
    '#     France Musique répondent ainsi.',
    '#   - `webrf_mouv_player` : la réponse est l’objet ÉMISSION, avec ce qui s’y',
    '#     joue en une seule chaîne « ARTISTE - Titre ». Seul profil qui sorte le',
    '#     morceau sur Mouv’, France Musique et les 45 locales ; sur les stations',
    '#     parlées il rend la même paire que le profil propre à la marque (vérifié',
    '#     sur France Inter, franceinfo et France Culture). Son nom est fortuit :',
    '#     c’est un profil du serveur, pas un point d’entrée de Mouv’.',
    '#',
    `# Relevé le ${date}. ${total} stations.`,
    '',
  ]
  const corps = groupes.flatMap(({ titre, stations }) => [
    `# --- ${titre} ${'-'.repeat(Math.max(1, 76 - titre.length))}`,
    '',
    ...stations.flatMap((s) => [
      ...(NOTES[s.id] ?? []),
      '[[station]]',
      `label = "${s.label.replace(/"/g, '\\"')}"`,
      `mounts = ["${s.mount}"]`,
      `id = ${s.id}`,
      `rules = "${s.rules}"`,
      '',
    ]),
  ])
  return [...entete, ...corps].join('\n')
}

// ---------------------------------------------------------------- collecte

const [platMarques, platLocales, htmlFip, htmlFm] = await Promise.all([
  texte(DOC_MARQUES).then(jsonDeLaPage),
  texte(DOC_LOCALES).then(jsonDeLaPage),
  texte(SITE_FIP),
  texte(SITE_FM),
])

const marques = stationsDoc(platMarques)
const docLocales = stationsDoc(platLocales)
if (!marques.length || !docLocales.length) {
  throw new Error('la documentation a change de forme : aucune station extraite, rien n’est ecrit')
}
// The locals page carries both the "ici" network and FIP's webradios; the FIP
// ones are told apart by their mount, not by their position on the page.
const locales = docLocales.filter((s) => s.mount.startsWith('fb'))
const webradiosFipDoc = docLocales.filter((s) => !s.mount.startsWith('fb'))

const cartes = new Map([...cartesDuSite(htmlFip), ...cartesDuSite(htmlFm)])
const horsDoc = []
for (const [slug, mount] of Object.entries(MOUNTS_HORS_DOC)) {
  const carte = cartes.get(slug)
  if (!carte) {
    throw new Error(`carte introuvable pour ${slug} : le site a change de forme, rien n’est ecrit`)
  }
  horsDoc.push({ id: carte.id, mount, label: libelle(slug, carte.label), source: slug })
}

const morts = await verifieMounts(horsDoc.map((s) => s.mount))
if (morts.length) {
  throw new Error(`mount(s) hors documentation sans reponse : ${morts.join(', ')} — rien n’est ecrit`)
}

const parId = (a, b) => a.id - b.id
const webradiosFip = [...webradiosFipDoc, ...horsDoc.filter((s) => s.source.startsWith('fip_'))].sort(parId)
const webradiosFm = horsDoc.filter((s) => s.source.startsWith('francemusique_')).sort(parId)
// FIP itself answers with the song object; the other five national brands do
// not, so they take the programme profile like the local stations.
const profil = (s, defaut) => ({ ...s, rules: s.mount === 'fip' ? MORCEAU : defaut })
const groupes = [
  { titre: 'Les six marques nationales', stations: marques.sort(parId).map((s) => profil(s, EMISSION)) },
  { titre: 'Webradios FIP', stations: webradiosFip.map((s) => profil(s, MORCEAU)) },
  { titre: 'Webradios France Musique', stations: webradiosFm.map((s) => profil(s, MORCEAU)) },
  { titre: `Les ${locales.length} locales ici (ex-France Bleu)`, stations: locales.sort(parId).map((s) => profil(s, EMISSION)) },
]

const total = groupes.reduce((n, g) => n + g.stations.length, 0)
// A duplicated identifier or mount would make one station shadow another, and
// the shadowed one would simply never be recognized — silently.
for (const champ of ['id', 'mount']) {
  const vus = new Set()
  for (const g of groupes) {
    for (const s of g.stations) {
      if (vus.has(s[champ])) throw new Error(`${champ} ${s[champ]} en double (${s.label}) : rien n’est ecrit`)
      vus.add(s[champ])
    }
  }
}

/** The station table of the README, between its two markers. */
function markdown(groupes) {
  const lignes = ['', '| Station | Mount | Id | Profile |', '|---|---|---|---|']
  for (const { titre, stations } of groupes) {
    lignes.push(`| **${titre}** | | | |`)
    for (const s of stations) lignes.push(`| ${s.label} | \`${s.mount}\` | ${s.id} | \`${s.rules}\` |`)
  }
  lignes.push('')
  return lignes.join('\n')
}

/** Replaces the marked section of the README, leaving the prose untouched. */
function lisezmoiRendu(actuel, groupes) {
  const i = actuel.indexOf(DEBUT)
  const j = actuel.indexOf(FIN)
  if (i < 0 || j < 0 || j < i) {
    throw new Error(`marqueurs introuvables dans ${LISEZMOI} : rien n’est ecrit`)
  }
  return actuel.slice(0, i + DEBUT.length) + markdown(groupes) + actuel.slice(j)
}

// The date is not re-read from the existing file: it dates the survey, not the
// file.
const rendu = toml(groupes, new Date().toISOString().slice(0, 10), total)
const tomlActuel = readFileSync(CIBLE, 'utf8')
const lisezmoiActuel = readFileSync(LISEZMOI, 'utf8')
const lisezmoi = lisezmoiRendu(memeFins(lisezmoiActuel), groupes)

if (process.argv.includes('--verifier')) {
  // Comparison excluding the date line: only the entries' content counts.
  const sansDate = (t) => memeFins(t).replace(/^# Relevé le .*$/m, '')
  const ecarts = []
  if (sansDate(tomlActuel) !== sansDate(rendu)) ecarts.push(CIBLE)
  if (memeFins(lisezmoiActuel) !== lisezmoi) ecarts.push(LISEZMOI)
  if (ecarts.length) {
    console.error(`differe des sources (${total} stations en ligne) : ${ecarts.join(', ')}`)
    process.exit(1)
  }
  console.log(`table et README a jour (${total} stations)`)
} else {
  writeFileSync(CIBLE, commeSurDisque(rendu, tomlActuel))
  writeFileSync(LISEZMOI, commeSurDisque(lisezmoi, lisezmoiActuel))
  console.log(`${CIBLE} : ${total} stations ecrites`)
  console.log(`${LISEZMOI} : tableau mis a jour`)
}
