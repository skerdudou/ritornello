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
// Why this table is embedded rather than fetched at boot: a device that starts
// unattended must not depend on a third party's page to recognize its
// stations, and such a failure would be silent.
//
// Usage: node scripts/fetch-stations.mjs
//        node scripts/fetch-stations.mjs --verifier   (writes nothing, exits
//        nonzero if the bundled table differs from the sources)
//
// Requires `curl` on PATH. Development-machine-only script (never runs on
// the appliance), and curl ships with Windows 10+ and every mainstream Linux
// distribution, so this dependency is acceptable here where it would not be
// in the plugin itself.

import { execFileSync } from 'node:child_process'
import { writeFileSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const BRANDS = [
  { brand: 'nrj', host: 'www.nrj.fr' },
  { brand: 'nostalgie', host: 'www.nostalgie.fr' },
  { brand: 'cheriefm', host: 'www.cheriefm.fr' },
  { brand: 'rireetchansons', host: 'www.rireetchansons.fr' },
]

const UA =
  'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ' +
  '(KHTML, like Gecko) Chrome/128.0 Safari/537.36'

const URL_FIELDS = ['url_128k_mp3', 'url_64k_aac', 'url_hd_aac']
const TOKEN = /^[a-z0-9]{12}$/

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

const here = dirname(fileURLToPath(import.meta.url))
const out = join(here, '..', 'src', 'stations.toml')

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
if (process.argv.includes('--verifier')) {
  const current = readFileSync(out, 'utf8')
  // The header carries the generation date, which changes on every run: the
  // comparison is on the entries alone.
  const entries = (s) => s.slice(s.indexOf('[[station]]'))
  if (entries(current) !== entries(text)) {
    console.error('stations.toml differs from the sources')
    process.exit(1)
  }
  console.log(`stations.toml matches the sources (${stations.length} stations)`)
} else {
  writeFileSync(out, text)
  console.log(`${out}: ${stations.length} stations`)
}
