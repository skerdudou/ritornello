// Regenerates src/stations.toml from the NRJ group's own on-air endpoints.
//
// One source, four hosts: every brand of the group publishes /onair.json,
// which lists ALL of its webradios with, in the same object, the three stream
// URLs and the station identifier the metadata endpoint expects. The pairing
// is therefore written by NRJ, not reconstructed by us.
//
// A host only serves its OWN stations: asking rireetchansons.fr for NRJ's id
// 158 returns nothing. That is why the brand travels in the table next to the
// identifier — it is what tells the plugin which host to query.
//
// cheriefm.fr answers 403 without a full browser User-Agent. Measured: it is
// the header, not the request rate — with `curl`.
//
// That qualifier matters: measured the same minute, on the same machine,
// Node's own HTTP stack (`fetch`, and raw `https.request` underneath it)
// gets a 403 from cheriefm.fr REGARDLESS of headers, while the other three
// hosts answer it 200 without complaint. `curl` with the same full browser
// User-Agent gets 200 from all four, cheriefm included. So there are two
// independent facts, not one: cheriefm additionally wants a real
// User-Agent (true for both clients), AND it separately refuses Node's HTTP
// stack outright (true regardless of headers) — most likely a TLS/HTTP
// client fingerprint its Cloudflare front end blocks, since PowerShell's
// .NET-based client is refused the same way. This is why this script shells
// out to `curl` instead of using `fetch`: it is not a style choice, it is
// the only one of the two clients tried that this endpoint accepts at all.
// A future "modernization" back to `fetch` would silently drop a quarter of
// the table (cheriefm) with no error, since the other three brands would
// keep working.
//
// This does NOT extend to the plugin's own runtime path. Measured
// separately: `reqwest` + `rustls`, with the plugin's exact runtime
// User-Agent, gets 200 from all four brands, cheriefm included, and all
// four bodies parsed — so `live::follows` keeps using `reqwest` rather than
// also shelling out to `curl`. Only this generation script needs the
// external binary; record this here so a later reader does not re-open the
// question against the wrong client.
//
// Why this table is embedded rather than fetched at boot: a device that starts
// unattended must not depend on a third party's page to recognize its
// stations, and such a failure would be silent.
//
// It writes TWO files from the same fetch: src/stations.toml, which the binary
// embeds, and the station list in README.md, between its two markers. They are
// generated together so they cannot disagree — a README listing stations the
// table does not know would be worse than no list at all.
//
// Usage: node scripts/fetch-stations.mjs
//        node scripts/fetch-stations.mjs --verifier   (writes nothing, exits
//        nonzero if either the bundled table or the README's list differs from
//        the sources)
//
// Requires `curl` on PATH. Development-machine-only script (never runs on
// the appliance), and curl ships with Windows 10+ and every mainstream Linux
// distribution, so this dependency is acceptable here where it would not be
// in the plugin itself.

import { execFileSync } from 'node:child_process'
import { writeFileSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

// `title` is only ever displayed — it heads the brand's group in the README
// table. `brand` is the key the plugin matches on and must stay as the site
// spells it.
const BRANDS = [
  { brand: 'nrj', host: 'www.nrj.fr', title: 'NRJ' },
  { brand: 'nostalgie', host: 'www.nostalgie.fr', title: 'Nostalgie' },
  { brand: 'cheriefm', host: 'www.cheriefm.fr', title: 'Chérie FM' },
  { brand: 'rireetchansons', host: 'www.rireetchansons.fr', title: 'Rire & Chansons' },
]

const UA =
  'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ' +
  '(KHTML, like Gecko) Chrome/128.0 Safari/537.36'

const URL_FIELDS = ['url_128k_mp3', 'url_64k_aac', 'url_hd_aac']
const TOKEN = /^[a-z0-9]{12}$/

/**
 * Line endings, neutralized for comparison.
 *
 * The checkout is a Windows one (the project is developed under Windows +
 * WSL), so git hands `stations.toml` back with CRLF while this script
 * composes with LF. Comparing them raw made `--verifier` report a drift on
 * every run after a checkout or a rebase — a false alarm on the one check
 * meant to catch a real one.
 */
const sameLineEndings = (t) => t.replace(/\r\n/g, '\n')

/** Rewrites `rendered` with the line endings the file on disk already uses. */
const asOnDisk = (rendered, current) => (current.includes('\r\n') ? rendered.replace(/\n/g, '\r\n') : rendered)

/**
 * GETs `url` through `curl`, full browser headers included, and returns its
 * body and status code. `-w` appends the status code after the body on its
 * own line, which is how a plain `-o -` invocation lets us see it without a
 * second request.
 */
function curlGet(url) {
  const out = execFileSync(
    'curl',
    ['-s', '-A', UA, '-H', 'Accept-Language: fr-FR,fr;q=0.9', '-w', '\n%{http_code}', url],
    { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 },
  )
  const i = out.lastIndexOf('\n')
  return { body: out.slice(0, i), status: Number(out.slice(i + 1)) }
}

function fetchBrand({ brand, host }) {
  const { body, status } = curlGet(`https://${host}/onair.json`)
  if (status !== 200) throw new Error(`${host}: HTTP ${status}`)
  const list = JSON.parse(body)
  if (!Array.isArray(list) || list.length === 0) throw new Error(`${host}: empty list`)
  return list.map((s) => {
    const tokens = []
    for (const f of URL_FIELDS) {
      const url = s[f]
      if (!url) continue
      const token = url.replace(/\/+$/, '').split('/').pop()
      // A token that no longer looks like a token means the URL shape changed:
      // fail loudly rather than write a table that silently matches nothing.
      if (!TOKEN.test(token)) throw new Error(`${host} ${s.id}: unexpected token ${token}`)
      if (!tokens.includes(token)) tokens.push(token)
    }
    if (tokens.length === 0) throw new Error(`${host} ${s.id}: no stream URL`)
    return { label: s.name, tokens, brand, id: Number(s.id) }
  })
}

function render(stations) {
  const head = [
    '# NRJ group webradios: stream token -> brand and station identifier.',
    '#',
    '# MEASURED, NOT GUESSED. Source: the /onair.json of each of the four brand',
    '# sites, where every station carries its three stream URLs and its',
    '# identifier in the same object. `scripts/fetch-stations.mjs` regenerates',
    '# this file from those same sources.',
    '#',
    '# `brand` is not decoration: a host only serves its own stations, so it is',
    '# what tells the plugin which host to query.',
    '#',
    '# Each `tokens` entry is the last segment of a stream URL, looked up as a',
    '# whole token of the URL configured on the device — which usually carries a',
    '# query string (`?origine=fluxradios`) but always the token.',
    '#',
    `# Measured on ${new Date().toISOString().slice(0, 10)}. ${stations.length} stations.`,
    '',
  ].join('\n')
  const body = stations
    .map(
      (s) =>
        '[[station]]\n' +
        `label = ${JSON.stringify(s.label)}\n` +
        `tokens = [${s.tokens.map((t) => JSON.stringify(t)).join(', ')}]\n` +
        `brand = ${JSON.stringify(s.brand)}\n` +
        `id = ${s.id}\n`,
    )
    .join('\n')
  return `${head}\n${body}`
}

/**
 * The station table of the README, between its two markers.
 *
 * Every token of a station is listed, not just the first. A reader arrives
 * here with the URL configured on their device and searches the page for its
 * token; the same station is broadcast under three of them (mp3, aac, HD aac),
 * so printing one would fail two readers out of three.
 */
function markdown(stations) {
  const lines = ['', '| Station | Stream tokens | Id |', '|---|---|---|']
  for (const { brand, title } of BRANDS) {
    const group = stations.filter((s) => s.brand === brand)
    lines.push(`| **${title}** — ${group.length} stations | | |`)
    for (const s of group) {
      const tokens = s.tokens.map((t) => `\`${t}\``).join(' ')
      lines.push(`| ${s.label} | ${tokens} | ${s.id} |`)
    }
  }
  lines.push('')
  return lines.join('\n')
}

/** Replaces the marked section of the README, leaving the prose untouched. */
function renderedReadme(current, stations) {
  const i = current.indexOf(START)
  const j = current.indexOf(END)
  if (i < 0 || j < 0 || j < i) {
    throw new Error(`markers not found in ${readme}: nothing written`)
  }
  return current.slice(0, i + START.length) + markdown(stations) + current.slice(j)
}

const here = dirname(fileURLToPath(import.meta.url))
const out = join(here, '..', 'src', 'stations.toml')
const readme = join(here, '..', 'README.md')

// These two strings must match the README byte for byte, or the section is not
// found and nothing is written. Change them and the README together.
const START = '<!-- stations:auto:start — generated by scripts/fetch-stations.mjs, do not edit by hand -->'
const END = '<!-- stations:auto:end -->'

const stations = (await Promise.all(BRANDS.map(fetchBrand))).flat()

// Two invariants the plugin's tests also lock in, checked here so that a bad
// table is never written in the first place.
const ids = new Set()
const seen = new Set()
for (const s of stations) {
  if (ids.has(s.id)) throw new Error(`duplicate identifier ${s.id} (${s.label})`)
  ids.add(s.id)
  for (const t of s.tokens) {
    if (seen.has(t)) throw new Error(`duplicate token ${t} (${s.label})`)
    seen.add(t)
  }
}

const text = render(stations)
// Absent on a first generation (the plan's own step 3, run before this file
// exists): fall back to an empty string rather than letting ENOENT abort
// before either branch below runs. `asOnDisk(text, '')` then writes plain LF,
// which is correct for a brand-new file.
let current = ''
try {
  current = readFileSync(out, 'utf8')
} catch (e) {
  if (e.code !== 'ENOENT') throw e
}

// The README, unlike the table, must already exist: it carries the prose and
// the two markers. A missing one is a real error, so no ENOENT fallback here.
const currentReadme = readFileSync(readme, 'utf8')
const nextReadme = renderedReadme(sameLineEndings(currentReadme), stations)

if (process.argv.includes('--verifier')) {
  // The header carries the generation date, which changes on every run: the
  // comparison is on the entries alone. Line endings are normalized first, or
  // a fresh Windows checkout (CRLF on disk vs. this script's LF) would report
  // a drift that is not there.
  const entries = (s) => {
    const n = sameLineEndings(s)
    return n.slice(n.indexOf('[[station]]'))
  }
  const drifted = []
  if (entries(current) !== entries(text)) drifted.push(out)
  if (sameLineEndings(currentReadme) !== nextReadme) drifted.push(readme)
  if (drifted.length) {
    console.error(`differs from the sources: ${drifted.join(', ')}`)
    process.exit(1)
  }
  console.log(`table and README match the sources (${stations.length} stations)`)
} else {
  writeFileSync(out, asOnDisk(text, current))
  writeFileSync(readme, asOnDisk(nextReadme, currentReadme))
  console.log(`${out}: ${stations.length} stations`)
  console.log(`${readme}: station list updated`)
}
