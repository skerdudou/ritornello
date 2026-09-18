import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

// **Which comment starts which workflow, decided by running the conditions
// rather than by reading them.**
//
// `claude-code-review.yml` answers `/review` and `@claude-review`;
// `claude.yml` answers `@claude`. The trap is that **`@claude-review`
// contains `@claude`**, so the obvious pair of conditions starts BOTH on
// every request for a review -- a second session, in parallel, doing
// something nobody asked for and paying for it. Only an explicit exclusion in
// `claude.yml` keeps them apart, and nothing but this file would notice if it
// were dropped while tidying the expression.
//
// The conditions are lifted out of the YAML and evaluated, so these assert
// what the workflows will really do. A test that restated the logic in
// JavaScript would pass happily while the YAML said something else -- which
// is the whole failure mode here, since the two files have to agree and
// neither mentions the other in code.
//
// Deliberately a text test: no YAML parser is installed here, and the
// reasoning `check-workflow-permissions.test.mjs` records about not adding a
// dependency so a CI check can read two keys applies unchanged.

const REVIEW = '.github/workflows/claude-code-review.yml'
const CLAUDE = '.github/workflows/claude.yml'

/**
 * Pull a job's `if:` block out of a workflow file.
 *
 * Both are `if: |` blocks, so the body is every line indented past the key,
 * up to the next key at the same level.
 */
function condition(path, job) {
  const lines = readFileSync(path, 'utf8').split(/\r?\n/)
  const start = lines.findIndex((l) => l === `  ${job}:`)
  assert.notEqual(start, -1, `${path} has no job \`${job}\`; this test is reading the wrong shape`)

  const open = lines.slice(start).findIndex((l) => /^ {4}if: \|\s*$/.test(l))
  assert.notEqual(open, -1, `job \`${job}\` has no \`if: |\` block`)

  const rest = lines.slice(start + open + 1)
  const end = rest.findIndex((l) => /^ {4}\S/.test(l))
  const body = rest.slice(0, end === -1 ? undefined : end).filter((l) => !/^\s*#/.test(l))
  const text = body.join('\n').trim()
  assert.ok(text.length > 40, `read only ${text.length} characters as \`${job}\`'s condition`)
  return text
}

/**
 * Evaluate a GitHub Actions `if` expression against a fake event.
 *
 * Only the handful of constructs these two conditions use is translated --
 * `contains`, `startsWith`, the boolean operators and `github.event*` paths.
 * Anything else reaching this would be a silent mistranslation, so the
 * leftover-`github.` assertion below refuses it rather than guessing.
 */
function evaluate(expression, event) {
  const js = expression
    .replace(/contains\(/g, '__contains(')
    .replace(/startsWith\(/g, '__startsWith(')
    .replace(/github\.event_name/g, 'ev.event_name')
    .replace(/github\.event\./g, 'ev.')
  assert.doesNotMatch(js, /github\./, `this expression uses a construct the evaluator does not translate: ${expression}`)

  const contains = (haystack, needle) => String(haystack ?? '').includes(needle)
  const startsWith = (haystack, needle) => String(haystack ?? '').startsWith(needle)
  // eslint-disable-next-line no-new-func
  return Boolean(new Function('ev', '__contains', '__startsWith', `return (${js})`)(event, contains, startsWith))
}

/** A comment event, of the shape both conditions read. */
function comment(body, { bot = false, onPullRequest = true, event_name = 'issue_comment' } = {}) {
  return {
    event_name,
    comment: { body, user: { type: bot ? 'Bot' : 'User' } },
    issue: { pull_request: onPullRequest ? { url: 'https://api.github.com/…' } : undefined, body: '', title: '' },
    review: { body: '' },
  }
}

const reviewIf = () => condition(REVIEW, 'claude-review')
const claudeIf = () => condition(CLAUDE, 'claude')

const startsReview = (body, opts) => evaluate(reviewIf(), comment(body, opts))
const startsClaude = (body, opts) => evaluate(claudeIf(), comment(body, opts))

test('the evaluator is faithful enough to be trusted', () => {
  // Without this every assertion below could pass against a broken
  // translation that simply returns false for everything.
  assert.equal(evaluate("contains(github.event.comment.body, 'x')", comment('axb')), true)
  assert.equal(evaluate("contains(github.event.comment.body, 'x')", comment('abc')), false)
  assert.equal(evaluate("startsWith(github.event.comment.body, 'x')", comment('xyz')), true)
  assert.equal(evaluate("startsWith(github.event.comment.body, 'x')", comment('yxz')), false)
  assert.equal(evaluate("!contains(github.event.comment.body, 'x')", comment('abc')), true)
  assert.equal(evaluate("github.event_name == 'issue_comment'", comment('a')), true)
  assert.throws(() => evaluate('github.token', comment('a')), /does not translate/)
})

// --- Exactly one workflow answers each comment -------------------------------

test('both spellings start the review', () => {
  assert.ok(startsReview('/review'), '`/review` on its own')
  assert.ok(startsReview('/review please, the CSS part worries me'), '`/review` with a request after it')
  assert.ok(startsReview('@claude-review'), 'the mention on its own')
  assert.ok(startsReview('Could you take a look? @claude-review'), 'the mention mid-sentence, as a mention is written')
})

test('and neither of them also starts claude.yml', () => {
  // **The reason this file exists.** `@claude-review` contains `@claude`.
  for (const body of ['/review', '@claude-review', 'please @claude-review this one']) {
    assert.equal(startsClaude(body), false, `\`${body}\` must not start a second session`)
  }
})

test('a plain @claude still starts claude.yml, and only that', () => {
  assert.ok(startsClaude('@claude why is the CI red?'))
  assert.equal(startsReview('@claude why is the CI red?'), false)
})

test('a comment asking for both gets one review, not one of each', () => {
  // Someone writing `@claude` out of habit and `/review` for the command.
  assert.ok(startsReview('/review — and @claude, check the lockfile too'))
  assert.equal(startsClaude('/review — and @claude, check the lockfile too'), false)
})

// --- What must NOT start a review --------------------------------------------

test('a URL that merely contains /review does not start one', () => {
  // The reason `/review` is matched at the start and not anywhere: links to
  // GitHub's own review endpoints carry the word, and a review nobody asked
  // for is exactly what this trigger exists to stop.
  assert.equal(startsReview('see https://github.com/skerdudou/ritornello/pull/41/reviews'), false)
  assert.equal(startsReview('the file lives in docs/review.md'), false)
})

test('a bot cannot start either workflow', () => {
  // The counterpart of `bypassPermissions`: an automated comment quoting a
  // diff must not start a session that can run anything.
  assert.equal(startsReview('/review', { bot: true }), false)
  assert.equal(startsReview('@claude-review', { bot: true }), false)
  assert.equal(startsClaude('@claude please', { bot: true }), false)
})

test('a comment on an issue does not start a review', () => {
  // `issue_comment` fires on issues too, where there is no diff to read.
  assert.equal(startsReview('/review', { onPullRequest: false }), false)
})

test('an unrelated comment starts nothing', () => {
  assert.equal(startsReview('merging this tomorrow'), false)
  assert.equal(startsClaude('merging this tomorrow'), false)
})

// --- The property, stated once ----------------------------------------------

test('no comment ever starts both workflows', () => {
  // The four assertions above are the explanation; this one is the guard, and
  // it covers the spellings nobody has thought of yet.
  const bodies = [
    '/review',
    '/review now',
    '@claude-review',
    'x @claude-review',
    '@claude',
    '@claude do a code review',
    '@claude-review and @claude',
    '/review @claude',
    'nothing here',
    'https://github.com/o/r/pull/1/reviews',
  ]
  for (const body of bodies) {
    const both = startsReview(body) && startsClaude(body)
    assert.equal(both, false, `\`${body}\` starts both workflows`)
  }
})
