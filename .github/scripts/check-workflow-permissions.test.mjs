import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

// This file exists because of one regression the merge job introduced.
//
// Before it, `dependabot-analysis.yml` declared `contents: read` at workflow
// level and no job could write a ref at all -- structurally, not by
// convention. `gh pr merge --auto` needs `contents: write`, so the ceiling had
// to rise, and the property is now held by each job narrowing itself back.
//
// A job added later without its own `permissions:` block would silently
// inherit `contents: write`, including the job that runs a model and holds a
// checkout of the pull request head. Nothing but a comment stopped that. This
// is the guard that does.
//
// Deliberately a text test and not a YAML one: no YAML parser is installed
// here, and adding a dependency to the root manifest so a CI check can read
// two keys is the wrong trade -- the same reasoning `read-bump-metadata.mjs`
// records for Dependabot's commit block. The shape being read is our own
// file with a fixed two-space indent, and every assertion below fails loudly
// rather than vacuously if that shape changes.

const WORKFLOW = '.github/workflows/dependabot-analysis.yml'
const source = readFileSync(WORKFLOW, 'utf8')

// Split on top-level job headers: exactly two spaces, a name, a colon, end of
// line. The `jobs:` key itself is at column 0 and does not match.
function jobBlocks(text) {
  const lines = text.split(/\r?\n/)
  const start = lines.findIndex((l) => l === 'jobs:')
  assert.notEqual(start, -1, `${WORKFLOW} has no top-level \`jobs:\` key; this test is reading the wrong shape`)

  const blocks = new Map()
  let current = null
  for (const line of lines.slice(start + 1)) {
    const header = /^ {2}([A-Za-z0-9_-]+):\s*$/.exec(line)
    if (header) {
      current = header[1]
      blocks.set(current, [])
    } else if (current) {
      blocks.get(current).push(line)
    }
  }
  return blocks
}

// A job's own keys sit at four spaces. Anything deeper belongs to a step and
// must not be mistaken for the job's declaration -- a step-level `permissions:`
// does not exist, but a `with:` block could contain the word.
function declaresAtJobLevel(body, key) {
  return body.some((l) => new RegExp(`^ {4}${key}:\\s*$`).test(l))
}

function jobLevelScope(body, scope) {
  // The scopes sit at six spaces, inside the job's own `permissions:` block.
  // Read only that block, so a `with:` key of the same name cannot be read as
  // a permission.
  let inside = false
  for (const line of body) {
    if (/^ {4}permissions:\s*$/.test(line)) { inside = true; continue }
    if (inside && /^ {4}\S/.test(line)) { break }
    if (!inside) { continue }
    const m = new RegExp(`^ {6}${scope}:\\s*(\\S+)\\s*$`).exec(line)
    if (m) { return m[1] }
  }
  return null
}

const jobs = jobBlocks(source)

test('the test is reading the jobs it thinks it is', () => {
  // Without this the whole file could pass vacuously after a rename.
  assert.deepEqual([...jobs.keys()].sort(), ['analyse', 'merge'])
})

test('every job declares its own permissions block', () => {
  for (const [name, body] of jobs) {
    assert.ok(
      declaresAtJobLevel(body, 'permissions'),
      `job \`${name}\` has no \`permissions:\` of its own, so it inherits the workflow ceiling -- which includes \`contents: write\``,
    )
  }
})

test('only the merge job may write contents', () => {
  for (const [name, body] of jobs) {
    const contents = jobLevelScope(body, 'contents')
    assert.ok(contents, `job \`${name}\` declares no \`contents:\` scope`)
    if (name === 'merge') {
      assert.equal(contents, 'write', 'the merge job needs `contents: write` for `gh pr merge --auto`')
    } else {
      assert.equal(
        contents,
        'read',
        `job \`${name}\` must not be able to write a ref: it runs a model and holds a checkout of the pull request head`,
      )
    }
  }
})

test('the workflow ceiling is not quietly lowered below what the merge job needs', () => {
  // The other direction of the same guard: if someone puts the ceiling back to
  // `contents: read` to "tidy up", the merge job silently stops working -- and
  // the failure would look like a GitHub problem, not an edit.
  const ceiling = /^permissions:\s*$([\s\S]*?)^\S/m.exec(source + '\nX')
  assert.ok(ceiling, 'the workflow-level `permissions:` block could not be found')
  assert.match(ceiling[1], /^ {2}contents: write\s*$/m)
})

test('the analyse job keeps exactly the scopes it needs and no others', () => {
  const body = jobs.get('analyse')
  assert.equal(jobLevelScope(body, 'pull-requests'), 'write', 'it posts the reasoning as a comment')
  assert.equal(jobLevelScope(body, 'actions'), 'read', 'it reads the failing CI run log')
  assert.equal(jobLevelScope(body, 'id-token'), 'write', "claude-code-action's OIDC exchange needs it")
})
