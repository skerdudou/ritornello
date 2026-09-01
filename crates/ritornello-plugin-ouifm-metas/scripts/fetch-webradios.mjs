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
// Note: the generated file's header (`header` below) is committed as
// src/webradios.toml and locked by the --verifier comparison.
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const PAGE = 'https://www.ouifm.fr/player'
const TARGET = join(dirname(fileURLToPath(import.meta.url)), '..', 'src', 'webradios.toml')

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
const LEGACY_MOUNTS = {
  2174546520932614531n: ['ouifm-high'], // OUI FM (inferred, see above)
  3134161803443976382n: ['ouifm2.'], // icy-name: OUI FM Alternatif
  3134161803443976427n: ['ouifm3.'], // icy-name: OUI FM Classic Rock
  3134161803443976526n: ['ouifm5.'], // icy-name: OUI FM Rock Indé
}

/** Extracts the JSON object assigned to `apidata` in the page source. */
function apidata(html) {
  const m = /apidata\s*=\s*(\{.*?\})\s*(?:;|<\/script>)/s.exec(html)
  if (!m) throw new Error('`apidata` variable not found: the page changed shape')
  return JSON.parse(m[1])
}

function toml(streams, date) {
  const header = [
    '# OUI FM webradios: mapping between the stream identifier (present in the',
    '# audio stream\'s URL) and the identifier expected by the metadata stream.',
    '#',
    '# MEASURED, NOT GUESSED. Source: the `apidata` JavaScript variable from',
    `# ${PAGE}, whose \`id\` (stream identifier) and`,
    '# `idMds` (metadata identifier) fields are carried over here verbatim.',
    '# `scripts/fetch-webradios.mjs` regenerates this file from that same source.',
    '#',
    '# Checked by hand against `?id=` on the metadata stream: the `metas`',
    '# identifier returns artist and title, the stream identifier returns an',
    '# empty frame.',
    '#',
    '# Each `urls` entry is looked up as a substring of the stream URL: the',
    '# broadcast URL carries a signed token and a variable format, but always',
    '# the stream identifier. The `ouifmN.` fragments recognize the historical',
    '# Icecast mounts (Infomaniak broadcast), which are the long-published',
    '# URLs — hence the ones a directory references and a user copies. See',
    '# `scripts/fetch-webradios.mjs` for how they were measured.',
    '#',
    `# Measured on ${date}. ${streams.length} streams.`,
    '',
  ]
  const body = streams.flatMap((f) => {
    const fragments = [f.id, ...(LEGACY_MOUNTS[BigInt(f.idMds)] ?? [])]
    return [
      '[[webradio]]',
      `label = "${f.label.replace(/"/g, '\\"')}"`,
      `urls = [${fragments.map((u) => `"${u}"`).join(', ')}]`,
      `metas = "${f.idMds}"`,
      '',
    ]
  })
  return [...header, ...body].join('\n')
}

const html = await fetch(PAGE, {
  headers: { 'user-agent': 'Mozilla/5.0 (X11; Linux x86_64) Chrome/120' },
}).then((r) => {
  if (!r.ok) throw new Error(`HTTP ${r.status} on ${PAGE}`)
  return r.text()
})

const data = apidata(html)
const streams = [...(data.radiostreams ?? []), ...(data.webradios ?? [])]
const missing = streams.filter((f) => !f.id || !f.idMds)
if (!streams.length) throw new Error('no streams in `apidata`')
if (missing.length) {
  throw new Error(`${missing.length} streams without id or idMds: unexpected source, nothing written`)
}
// A historical mount tied to a vanished `idMds` would be lost silently:
// better to say so — it is the sign OUI FM removed or renumbered a webradio.
const known = new Set(streams.map((f) => BigInt(f.idMds)))
for (const id of Object.keys(LEGACY_MOUNTS).map(BigInt)) {
  if (!known.has(id)) {
    console.warn(`warning: historical mount tied to ${id}, absent from apidata`)
  }
}

// The date is not re-read from the existing file: it dates the survey,
// not the file.
const rendered = toml(streams, new Date().toISOString().slice(0, 10))

// Line endings, neutralized for comparison. The checkout is a Windows one
// (the project is developed under Windows + WSL), so git hands this file back
// with CRLF while this script composes with LF: comparing them raw reported a
// drift on every run after a checkout or a rebase — a false alarm on the one
// check meant to catch a real one.
const sameLineEndings = (t) => t.replace(/\r\n/g, '\n')
const existing = readFileSync(TARGET, 'utf8')

if (process.argv.includes('--verifier')) {
  // Comparison excluding the date line: only the entries' content counts.
  const withoutDate = (t) => sameLineEndings(t).replace(/^# Measured on .*$/m, '')
  if (withoutDate(existing) !== withoutDate(rendered)) {
    console.error(`${TARGET} differs from the source (${streams.length} streams live)`)
    process.exit(1)
  }
  console.log(`table up to date (${streams.length} streams)`)
} else {
  // Written back with the endings the file already uses, so a Windows checkout
  // does not show the whole file as modified.
  writeFileSync(TARGET, existing.includes('\r\n') ? rendered.replace(/\n/g, '\r\n') : rendered)
  console.log(`${TARGET}: ${streams.length} streams written`)
}
