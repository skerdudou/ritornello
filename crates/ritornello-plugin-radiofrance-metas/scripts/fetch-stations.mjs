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
// B.O.". They are listed in MOUNTS_OUTSIDE_DOC below and each one is verified
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
// Note: the generated file's header (`header` below) is committed as
// src/stations.toml and locked by the --verifier comparison. The README
// markers below (`START`/`END`) must keep the exact text already committed
// in README.md, which this script does not otherwise translate.
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const UA = 'Mozilla/5.0 (X11; Linux x86_64) Chrome/120'
const DOC = 'https://developers.radiofrance.fr/doc/tutorial-by-example'
const DOC_BRANDS = `${DOC}/list-brands`
const DOC_LOCALS = `${DOC}/list-locals-and-webradios`
const SITE_FIP = 'https://www.radiofrance.fr/fip'
const SITE_FM = 'https://www.radiofrance.fr/francemusique'
const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..')
const TARGET = join(ROOT, 'src', 'stations.toml')
// The plugin's README lists the same stations. Regenerating it from the same
// run is what stops the two from drifting: a list kept by hand would be right
// on the day it was written and quietly wrong afterwards.
const README = join(ROOT, 'README.md')
const START = '<!-- stations:auto:début — généré par scripts/fetch-stations.mjs, ne pas éditer à la main -->'
const END = '<!-- stations:auto:fin -->'

// The 13 stations absent from the documentation: their mount, keyed by the
// brand slug the site's cards carry. Collected by hand and re-verified against
// icecast.radiofrance.fr at every run (see checkMounts). None of them can be
// derived from the slug, which is exactly why they are written down.
const MOUNTS_OUTSIDE_DOC = {
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
//  - TRACK (`webrf_fip_player`): the answer is the SONG object — title and
//    artist already separated, and the window is the song's. Only the stations
//    below answer this way: FIP, its webradios, and France Musique's.
//  - SHOW (`webrf_mouv_player`): the answer is the PROGRAMME object, with
//    what is playing inside it as a single "ARTIST - Title" string. It is the
//    only profile that surfaces the current song on Mouv', France Musique and
//    the 45 local stations; on purely spoken stations it returns the same
//    programme/detail pair as the brand's own profile (checked on France
//    Inter, franceinfo and France Culture). Its name is incidental — it is a
//    server-side profile, not a Mouv' endpoint.
const TRACK = 'webrf_fip_player'
const SHOW = 'webrf_mouv_player'

// Commentary attached to a station in the generated file, where the pairing
// deserves an explanation the reader would otherwise have to rediscover.
const NOTES = {
  407: [
    '# Mount "labo" = "la B.O." (original soundtracks), the webradio for film',
    '# music — which the site calls "Films" and whose brand slug is',
    '# `francemusique_evenementielle`. This is the only entry whose matching',
    '# rests on elimination: after assigning the ten other France Musique',
    '# webradios, exactly one mount and exactly one station remained.',
  ],
}

const text = (u) =>
  fetch(u, { headers: { 'user-agent': UA } }).then((r) => {
    if (!r.ok) throw new Error(`HTTP ${r.status} on ${u}`)
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
const sameLineEndings = (t) => t.replace(/\r\n/g, '\n')

/** Rewrites `rendered` with the line endings the file on disk already uses. */
const asOnDisk = (rendered, current) => (current.includes('\r\n') ? rendered.replace(/\n/g, '\r\n') : rendered)

/**
 * Strips the HTML while preserving the spaces INSIDE the JSON string literals
 * of a highlighted code block: each token sits in its own element, on its own
 * line, so trimming per line and joining without a separator rebuilds the JSON
 * exactly. Collapsing all whitespace instead would turn "France Bleu Alsace"
 * into "FranceBleuAlsace".
 */
function jsonFromPage(html) {
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
function stationsFromDoc(flat) {
  const out = []
  for (const m of flat.matchAll(/\{"id":"([A-Z_0-9]+)","title":"([^"]*)"[^{}]*?\}/g)) {
    const block = m[0]
    const id = /id_station=(\d+)/.exec(block)?.[1]
    const mount = /icecast\.radiofrance\.fr\/([a-z0-9]+)-(?:midfi|lofi|hifi)/.exec(block)?.[1]
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
function cardsFromSite(html) {
  const re =
    /data-brand="([a-z_0-9]+)"((?:(?!data-brand=)[\s\S]){0,4000}?)id="cardtitle-(\d+)"((?:(?!data-brand=)[\s\S]){0,1200}?)aria-label="([^"]*)"/g
  const seen = new Map()
  for (const m of html.matchAll(re)) if (!seen.has(m[1])) seen.set(m[1], { id: Number(m[3]), label: m[5].trim() })
  return seen
}

/** Human-readable label for a station taken from a site card. */
function label(slug, short) {
  const brand = slug.startsWith('fip_') ? 'FIP' : slug.startsWith('francemusique_') ? 'France Musique' : ''
  return brand ? `${brand} ${short}` : short
}

/** Checks that a mount still answers, so a stale one is reported, not written. */
async function checkMounts(mounts) {
  const dead = []
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
      if (r.status !== 200) dead.push(`${m} (HTTP ${r.status})`)
    } catch (e) {
      dead.push(`${m} (${e.message})`)
    }
  }
  return dead
}

function toml(groups, date, total) {
  const header = [
    '# Radio France stations: mapping between the broadcast mount (present in the',
    '# audio stream\'s URL) and the identifier expected by the live entry point.',
    '#',
    '# MEASURED, NOT GUESSED. Primary source: Radio France\'s public Open API',
    '# documentation (https://developers.radiofrance.fr/doc/tutorial-by-example',
    '# /list-brands and /list-locals-and-webradios), whose sample responses give',
    '# each station\'s `liveStream` (hence its Icecast mount) and its',
    '# `playerUrl`, which carries `id_station=<n>`. Both fields come from the',
    '# same response: the mapping is written by Radio France, not reconstructed.',
    '#',
    '# The 13 stations absent from that documentation — the France Musique',
    '# webradios, plus FIP Sacré français and FIP Cultes — are gathered from the',
    '# cards at https://www.radiofrance.fr/francemusique and /fip, where the',
    '# brand slug and the identifier are read off the same card. Their mount is',
    '# checked station by station against icecast.radiofrance.fr.',
    '# `scripts/fetch-stations.mjs` regenerates this file from those same',
    '# sources.',
    '#',
    '# Each mount is looked up as a **token** of the stream URL: bordered on',
    '# both sides by a non-alphanumeric character. That is what allows a single',
    '# entry per station where the same station is broadcast in several forms',
    '# (`icecast.radiofrance.fr/<mount>-midfi.mp3`, the historical name',
    '# `direct.fipradio.fr/live/<mount>-midfi.mp3`, the HLS',
    '# `stream.radiofrance.fr/<mount>/<mount>.m3u8`, and the `-lofi`,',
    '# `-hifi.aac` qualities) — and what stops `fip` from capturing `fipgroove`.',
    '#',
    '# `rules` is the **rendering profile** requested from the server, and its',
    '# choice is not cosmetic: it is what decides whether the plugin says',
    '# anything at all. Measured at the same instant on Mouv\', `webrf_fip_player`',
    '# answers "Le direct" / "Mouv\'" (the slogan) while `webrf_mouv_player`',
    '# answers "La Playlist" / "SOOLKING - Bye Bye (feat. TAYC)", which was',
    '# indeed what was on air.',
    '#   - `webrf_fip_player`: the response is the TRACK object, title and',
    '#     artist already separated, track boundaries. Only FIP, its webradios',
    '#     and France Musique\'s answer this way.',
    '#   - `webrf_mouv_player`: the response is the SHOW object, with whatever',
    '#     is playing in a single "ARTIST - Title" string. The only profile that',
    '#     yields the track on Mouv\', France Musique and the 45 local stations;',
    '#     on the talk stations it renders the same pair as the brand\'s own',
    '#     profile (checked on France Inter, franceinfo and France Culture). Its',
    '#     name is incidental: it is a server profile, not a Mouv\' endpoint.',
    '#',
    `# Measured on ${date}. ${total} stations.`,
    '',
  ]
  const body = groups.flatMap(({ title, stations }) => [
    `# --- ${title} ${'-'.repeat(Math.max(1, 76 - title.length))}`,
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
  return [...header, ...body].join('\n')
}

// ---------------------------------------------------------------- collection

const [flatBrands, flatLocals, htmlFip, htmlFm] = await Promise.all([
  text(DOC_BRANDS).then(jsonFromPage),
  text(DOC_LOCALS).then(jsonFromPage),
  text(SITE_FIP),
  text(SITE_FM),
])

const brands = stationsFromDoc(flatBrands)
const docLocals = stationsFromDoc(flatLocals)
if (!brands.length || !docLocals.length) {
  throw new Error('documentation changed shape: no station extracted, nothing written')
}
// The locals page carries both the "ici" network and FIP's webradios; the FIP
// ones are told apart by their mount, not by their position on the page.
const locals = docLocals.filter((s) => s.mount.startsWith('fb'))
const webradiosFipDoc = docLocals.filter((s) => !s.mount.startsWith('fb'))

const cards = new Map([...cardsFromSite(htmlFip), ...cardsFromSite(htmlFm)])
const outsideDoc = []
for (const [slug, mount] of Object.entries(MOUNTS_OUTSIDE_DOC)) {
  const card = cards.get(slug)
  if (!card) {
    throw new Error(`card not found for ${slug}: the site changed shape, nothing written`)
  }
  outsideDoc.push({ id: card.id, mount, label: label(slug, card.label), source: slug })
}

const dead = await checkMounts(outsideDoc.map((s) => s.mount))
if (dead.length) {
  throw new Error(`mount(s) outside the documentation with no response: ${dead.join(', ')} — nothing written`)
}

const byId = (a, b) => a.id - b.id
const webradiosFip = [...webradiosFipDoc, ...outsideDoc.filter((s) => s.source.startsWith('fip_'))].sort(byId)
const webradiosFm = outsideDoc.filter((s) => s.source.startsWith('francemusique_')).sort(byId)
// FIP itself answers with the song object; the other five national brands do
// not, so they take the programme profile like the local stations.
const withProfile = (s, fallback) => ({ ...s, rules: s.mount === 'fip' ? TRACK : fallback })
const groups = [
  { title: 'The six national brands', stations: brands.sort(byId).map((s) => withProfile(s, SHOW)) },
  { title: 'Webradios FIP', stations: webradiosFip.map((s) => withProfile(s, TRACK)) },
  { title: 'Webradios France Musique', stations: webradiosFm.map((s) => withProfile(s, TRACK)) },
  { title: `The ${locals.length} "ici" locals (formerly France Bleu)`, stations: locals.sort(byId).map((s) => withProfile(s, SHOW)) },
]

const total = groups.reduce((n, g) => n + g.stations.length, 0)
// A duplicated identifier or mount would make one station shadow another, and
// the shadowed one would simply never be recognized — silently.
for (const field of ['id', 'mount']) {
  const seen = new Set()
  for (const g of groups) {
    for (const s of g.stations) {
      if (seen.has(s[field])) throw new Error(`duplicate ${field} ${s[field]} (${s.label}): nothing written`)
      seen.add(s[field])
    }
  }
}

/** The station table of the README, between its two markers. */
function markdown(groups) {
  const lines = ['', '| Station | Mount | Id | Profile |', '|---|---|---|---|']
  for (const { title, stations } of groups) {
    lines.push(`| **${title}** | | | |`)
    for (const s of stations) lines.push(`| ${s.label} | \`${s.mount}\` | ${s.id} | \`${s.rules}\` |`)
  }
  lines.push('')
  return lines.join('\n')
}

/** Replaces the marked section of the README, leaving the prose untouched. */
function renderedReadme(current, groups) {
  const i = current.indexOf(START)
  const j = current.indexOf(END)
  if (i < 0 || j < 0 || j < i) {
    throw new Error(`markers not found in ${README}: nothing written`)
  }
  return current.slice(0, i + START.length) + markdown(groups) + current.slice(j)
}

// The date is not re-read from the existing file: it dates the survey, not the
// file.
const rendered = toml(groups, new Date().toISOString().slice(0, 10), total)
const currentToml = readFileSync(TARGET, 'utf8')
const currentReadme = readFileSync(README, 'utf8')
const readme = renderedReadme(sameLineEndings(currentReadme), groups)

if (process.argv.includes('--verifier')) {
  // Comparison excluding the date line: only the entries' content counts.
  const withoutDate = (t) => sameLineEndings(t).replace(/^# Measured on .*$/m, '')
  const diffs = []
  if (withoutDate(currentToml) !== withoutDate(rendered)) diffs.push(TARGET)
  if (sameLineEndings(currentReadme) !== readme) diffs.push(README)
  if (diffs.length) {
    console.error(`differs from the sources (${total} stations live): ${diffs.join(', ')}`)
    process.exit(1)
  }
  console.log(`table and README up to date (${total} stations)`)
} else {
  writeFileSync(TARGET, asOnDisk(rendered, currentToml))
  writeFileSync(README, asOnDisk(readme, currentReadme))
  console.log(`${TARGET}: ${total} stations written`)
  console.log(`${README}: table updated`)
}
