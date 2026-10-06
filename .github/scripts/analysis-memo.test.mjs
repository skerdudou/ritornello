import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { marker, memoKey, verdictComment } from './analysis-memo.mjs'

const SCRIPT = fileURLToPath(new URL('./analysis-memo.mjs', import.meta.url))
const WORKFLOW = '.github/workflows/dependabot-analysis.yml'

// The summary `read-bump-metadata.mjs` wrote for #44, the pull request that
// was analysed three times for one bump.
const VUEUSE = '@vueuse/core to 15.0.0 (major)'

test('the same bump against the same CI outcome is the same question', () => {
  assert.equal(memoKey({ summary: VUEUSE, conclusion: 'failure' }), memoKey({ summary: VUEUSE, conclusion: 'failure' }))
  assert.match(memoKey({ summary: VUEUSE, conclusion: 'failure' }), /^[0-9a-f]{16}$/)
})

test('a CI that turns green is a new question', () => {
  assert.notEqual(memoKey({ summary: VUEUSE, conclusion: 'failure' }), memoKey({ summary: VUEUSE, conclusion: 'success' }))
})

test('a new arrival version is a new question', () => {
  assert.notEqual(
    memoKey({ summary: VUEUSE, conclusion: 'failure' }),
    memoKey({ summary: '@vueuse/core to 15.0.1 (major)', conclusion: 'failure' }),
  )
})

test('an empty summary has no key, or every unreadable bump would share one', () => {
  assert.throws(() => memoKey({ summary: '', conclusion: 'failure' }), /empty/)
  assert.throws(() => memoKey({ summary: '   ', conclusion: 'failure' }), /empty/)
  assert.throws(() => memoKey({ summary: undefined, conclusion: 'failure' }), /empty/)
})

test('a conclusion that is not a verdict has no key', () => {
  assert.throws(() => memoKey({ summary: VUEUSE, conclusion: 'cancelled' }), /not a verdict/)
  assert.throws(() => memoKey({ summary: VUEUSE, conclusion: '' }), /not a verdict/)
})

const KEY = memoKey({ summary: VUEUSE, conclusion: 'failure' })

test('a refusal carries its marker, so the next rebase finds it', () => {
  const body = verdictComment({ decision: 'read-it', key: KEY, conclusion: 'failure', why: 'Two copies.', report: 'r' })
  assert.ok(body.startsWith(marker(KEY)))
})

test('a merge verdict carries no marker: a moved head must be judged again or it never merges', () => {
  const body = verdictComment({ decision: 'merge', key: KEY, conclusion: 'success', why: 'Fine.', report: 'r' })
  assert.ok(!body.includes('dependabot-analysis: read-it'))
  assert.ok(!body.includes('will not be analysed again'))
})

test('the reason comes first and the report is folded', () => {
  const body = verdictComment({ decision: 'read-it', key: KEY, conclusion: 'failure', why: 'Wait for reka-ui.', report: 'LONG REPORT' })
  const reason = body.indexOf('Wait for reka-ui.')
  const fold = body.indexOf('<details>')
  assert.ok(reason !== -1 && fold !== -1 && reason < fold)
  assert.ok(body.indexOf('LONG REPORT') > fold)
  assert.ok(body.trimEnd().endsWith('</details>'))
})

test('a missing reason is said, not left blank', () => {
  const body = verdictComment({ decision: 'read-it', key: KEY, conclusion: 'failure', why: '  ', report: 'r' })
  assert.match(body, /gave no short reason/)
})

test('a huge report is cut to fit the comment API, marker and fold intact', () => {
  // Multi-byte characters, so a cut on a character count would overshoot.
  const report = 'é'.repeat(70000)
  const body = verdictComment({ decision: 'read-it', key: KEY, conclusion: 'failure', why: 'w', report })
  assert.ok(Buffer.byteLength(body) <= 60000, `${Buffer.byteLength(body)} bytes`)
  assert.ok(body.startsWith(marker(KEY)))
  assert.match(body, /truncated/)
  assert.ok(body.trimEnd().endsWith('</details>'))
  assert.ok(!body.includes('�'))
})

test('a short report is left whole', () => {
  const body = verdictComment({ decision: 'read-it', key: KEY, conclusion: 'failure', why: 'w', report: 'x'.repeat(1000) })
  assert.ok(body.includes('x'.repeat(1000)))
  assert.ok(!body.includes('truncated'))
})

test('the command line refuses to post a refusal without a key', () => {
  const dir = mkdtempSync(join(tmpdir(), 'memo-'))
  writeFileSync(join(dir, 'report.md'), 'r')
  assert.throws(() =>
    execFileSync('node', [SCRIPT, 'comment', 'read-it', '', 'failure', join(dir, 'why.txt'), join(dir, 'report.md')], {
      stdio: 'pipe',
    }),
  )
  const ok = execFileSync('node', [SCRIPT, 'comment', 'read-it', KEY, 'failure', join(dir, 'why.txt'), join(dir, 'report.md')])
  assert.ok(String(ok).startsWith(marker(KEY)))
})

test('the command line writes the key the verdict step will mark with', () => {
  const dir = mkdtempSync(join(tmpdir(), 'memo-'))
  const out = join(dir, 'out')
  writeFileSync(out, '')
  execFileSync('node', [SCRIPT, 'key'], { env: { ...process.env, SUMMARY: VUEUSE, CONCLUSION: 'failure', GITHUB_OUTPUT: out } })
  assert.equal(readFileSync(out, 'utf8'), `key=${KEY}\n`)
})

// --- The workflow agrees with this file ------------------------------------
//
// A text test, like `review-triggers.test.mjs` and for the same reason: no
// YAML parser is installed here.

const workflow = readFileSync(WORKFLOW, 'utf8')

/** Each step's `if:` text, single-line or block. */
function stepConditions() {
  const lines = workflow.split(/\r?\n/)
  const steps = []
  for (let i = 0; i < lines.length; i++) {
    const name = /^ {6}- name: (.*)$/.exec(lines[i])
    if (name) steps.push({ name: name[1], cond: '' })
    const single = /^ {8}if: (?!\|)(.*)$/.exec(lines[i])
    if (single && steps.length) steps.at(-1).cond = single[1]
    if (/^ {8}if: \|\s*$/.test(lines[i]) && steps.length) {
      const body = []
      for (let j = i + 1; j < lines.length && /^ {10}/.test(lines[j]); j++) body.push(lines[j].trim())
      steps.at(-1).cond = body.join(' ')
    }
  }
  return steps
}

test('every step of the analysis path stands down when the refusal was already explained', () => {
  const gated = stepConditions().filter(
    (s) => s.cond.includes("steps.bump.outputs.compatible != 'true'") && !s.name.startsWith('Skip a refusal already explained'),
  )
  // Checkout, lay-out, analysis, tooling clear, tooling fetch, verdict.
  assert.equal(gated.length, 6, gated.map((s) => s.name).join('\n'))
  for (const step of gated) {
    assert.ok(step.cond.includes("steps.memo.outputs.seen != 'true'"), `\`${step.name}\` would still run`)
  }
})

const memoStep = workflow.slice(workflow.indexOf('id: memo'), workflow.indexOf('- name: Check out the pull request head'))

test('a manual dispatch never consults the memo', () => {
  assert.match(memoStep, /GITHUB_EVENT_NAME" = "workflow_dispatch"/)
})

test('the workflow looks for exactly the marker this file posts', () => {
  // The step greps a literal; if it drifted from `marker()`, every refusal
  // would be found by nothing and analysed again, silently.
  assert.ok(memoStep.includes(`grep -qF "${marker('$key')}" memo-comments.txt`))
})

test('the verdict step posts through this file, marked with the memo key', () => {
  const verdict = workflow.slice(workflow.indexOf('id: verdict'), workflow.indexOf('- name: Refuse a fix written over a green CI'))
  assert.match(verdict, /tools\/\.github\/scripts\/analysis-memo\.mjs comment "\$decision" "\$MEMO_KEY"/)
  assert.match(verdict, /MEMO_KEY: \$\{\{ steps\.memo\.outputs\.key \}\}/)
})
