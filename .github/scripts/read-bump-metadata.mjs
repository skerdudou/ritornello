import { appendFileSync, readFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

// Read the bump metadata from Dependabot's own commit message.
//
// `dependabot/fetch-metadata` reads the context of a `pull_request` event and
// does not work from `workflow_run`, which is the event stage 1 runs on. The
// information is not lost though: Dependabot writes a machine-readable block
// into the message of the commit it pushes, and that block is what
// fetch-metadata parses as well.
//
// This is a reader for exactly that block, not a YAML implementation. No YAML
// parser is installed here, and adding a dependency to the root manifest so a
// CI script can read three known keys is the wrong trade. The block is
// machine-generated with a fixed shape, so anything that is not that shape is
// refused rather than guessed at.
//
// Nothing is ever evaluated. A dependency name read here travels into a pull
// request comment and into a model prompt, so values are matched against a
// conservative class and the whole block is refused if one strays -- refusing
// is safe, sanitising is a guess.

const BLOCK_START = 'updated-dependencies:'

// YAML document markers: Dependabot puts the block in its own document.
const TERMINATORS = new Set(['...', '---'])

// Package names (including npm scopes), versions with build metadata, and the
// `version-update:semver-*` / `direct:production` vocabularies. Anything else
// stops the read.
const SAFE_VALUE = /^[A-Za-z0-9._:@\/+-]+$/

const ITEM = /^- ([a-z-]+): (.+)$/
const KEY = /^ {2}([a-z-]+): (.+)$/

// Dependabot quotes a value when it needs to (a scoped npm name). Strip one
// layer of matching quotes and nothing more.
const unquote = (raw) => {
  const value = raw.trim()
  const quoted = /^"(.*)"$/.exec(value) || /^'(.*)'$/.exec(value)
  return quoted ? quoted[1] : value
}

function readOne(message) {
  const lines = String(message ?? '').split('\n').map((line) => line.replace(/\r$/, ''))
  const start = lines.findIndex((line) => line.trim() === BLOCK_START)
  if (start < 0) return null

  const entries = []
  for (const line of lines.slice(start + 1)) {
    if (line.trim() === '') continue
    if (TERMINATORS.has(line.trim())) break

    const item = ITEM.exec(line)
    if (item) {
      entries.push({})
    } else if (!KEY.exec(line)) {
      // Inside the block and neither an item nor one of its keys: the shape
      // is not what we accept, so nothing here is trustworthy.
      return null
    }

    const [, key, rawValue] = item || KEY.exec(line)
    if (entries.length === 0) return null

    const value = unquote(rawValue)
    if (!SAFE_VALUE.test(value)) return null
    entries[entries.length - 1][key] = value
  }

  if (entries.length === 0) return null

  const updates = []
  for (const entry of entries) {
    const dependencyName = entry['dependency-name']
    const newVersion = entry['dependency-version']
    const updateType = entry['update-type']
    // All three are required. A missing one is not an entry to skip: it means
    // the block is not the shape this reader was written against, and the
    // caller must treat the bump as unreadable.
    if (!dependencyName || !newVersion || !updateType) return null
    updates.push({ dependencyName, newVersion, updateType })
  }

  const groups = new Set(entries.map((entry) => entry['dependency-group'] ?? null))
  const group = groups.size === 1 ? [...groups][0] : null

  return { updates, group }
}

// Oldest first. After stage 2 pushes a fix, the head commit is ours and
// carries no block, so the head alone is not enough.
export function readBumpMetadata(commitMessages) {
  for (const message of Array.isArray(commitMessages) ? commitMessages : []) {
    const read = readOne(message)
    if (read) return read
  }
  return null
}

// --- Command line -----------------------------------------------------------
//
// `node read-bump-metadata.mjs <commits.json>`, where the file is a JSON array
// of commit messages. Writes `unreadable=true`, or `summary=`, `reasons=`,
// `compatible=` and `group=`, to GITHUB_OUTPUT.
//
// The caret rule is applied here rather than in the workflow so the two always
// travel together: reading the bump and judging it are one question for the
// caller, and splitting them across a YAML step is how they drift.
if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const out = (line) => process.env.GITHUB_OUTPUT && appendFileSync(process.env.GITHUB_OUTPUT, `${line}\n`)

  const read = readBumpMetadata(JSON.parse(readFileSync(process.argv[2], 'utf8')))
  if (!read) {
    // Unreadable metadata is not a reason to proceed on a guess.
    out('unreadable=true')
    console.log('No readable updated-dependencies block in this pull request.')
  } else {
    const { classifyBump } = await import('./classify-bump.mjs')
    const summary = read.updates
      .map((u) => `${u.dependencyName} to ${u.newVersion} (${u.updateType.replace('version-update:semver-', '')})`)
      .join(', ')
    const verdict = classifyBump(read.updates)

    out(`summary=${summary}`)
    out(`reasons=${verdict.reasons.join(' | ')}`)
    out(`compatible=${verdict.compatible}`)
    out(`group=${read.group ?? ''}`)

    console.log(`Bump: ${summary}`)
    console.log(`Caret rule: ${verdict.compatible ? 'compatible' : verdict.reasons.join('; ')}`)
  }
}
