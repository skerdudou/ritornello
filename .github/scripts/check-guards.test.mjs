import { test } from 'node:test'
import assert from 'node:assert/strict'
import { checkGuards } from './check-guards.mjs'

// A minimal unified diff. `lines` are given with their leading +/- / space.
const diff = (path, lines) =>
  [`diff --git a/${path} b/${path}`, 'index 1111111..2222222 100644', `--- a/${path}`, `+++ b/${path}`, '@@ -1,3 +1,3 @@', ...lines].join('\n')

// Deduplicated on purpose. Guard 2 can fire twice on one diff -- once
// because the declaration count dropped, once because a test was silenced --
// and which of the two fired is not what most of these cases are about. The
// case that *is* about it asserts on `failures` directly.
const failedGuards = (text) => [...new Set(checkGuards(text).failures.map((f) => f.guard))].sort()

test('an ordinary source adaptation passes all three', () => {
  const text = diff('web/kit/src/lib/api.ts', ['-  const r = await get(url)', '+  const r = await get(url, {})'])
  assert.equal(checkGuards(text).ok, true)
})

test('an empty diff passes: nothing written is nothing to refuse', () => {
  assert.equal(checkGuards('').ok, true)
})

test('guard 1 refuses a change under .github', () => {
  const text = diff('.github/workflows/ci.yml', ['-        run: npm test', '+        run: true'])
  assert.deepEqual(failedGuards(text), [1])
})

test('guard 1 refuses a change to dependabot.yml, which is policy and not adaptation', () => {
  // The owner capped TypeScript below 7 by hand in .github/dependabot.yml.
  // That is a decision, not a mechanical adaptation, and it stays theirs.
  const text = diff('.github/dependabot.yml', ['+      - dependency-name: vitest'])
  assert.deepEqual(failedGuards(text), [1])
})

test('guard 2 refuses a diff that removes more tests than it adds', () => {
  const text = diff('web/kit/src/lib/api.test.ts', [
    "-  it('rejects an empty url', () => {})",
    "-  it('follows a redirect', () => {})",
    "+  it('rejects an empty url', () => {})",
  ])
  assert.deepEqual(failedGuards(text), [2])
})

test('guard 2 accepts a diff that rewrites a test without removing it', () => {
  const text = diff('web/kit/src/lib/api.test.ts', [
    "-  it('sends a click', () => { el.click() })",
    "+  it('sends a click', () => { el.dispatchEvent(new PointerEvent('click')) })",
  ])
  assert.equal(checkGuards(text).ok, true)
})

test('guard 2 accepts a diff that adds tests', () => {
  const text = diff('web/kit/src/lib/api.test.ts', ["+  it('handles the new option', () => {})"])
  assert.equal(checkGuards(text).ok, true)
})

test('guard 2 refuses a newly skipped test even when the count holds', () => {
  const text = diff('web/kit/src/lib/api.test.ts', [
    "-  it('follows a redirect', () => {})",
    "+  it.skip('follows a redirect', () => {})",
  ])
  assert.deepEqual(failedGuards(text), [2])
})

test('a silenced test that also drops the count is reported twice, once per reason', () => {
  // Found by running the suite rather than by reading it: replacing `it(` with
  // `it.skip(` trips guard 2 on both counts, because `\bit\s*\(` does not
  // match `it.skip(`. Two reasons is the honest report, and the comment on the
  // pull request is better for saying both.
  const text = diff('web/kit/src/lib/api.test.ts', [
    "-  it('follows a redirect', () => {})",
    "+  it.skip('follows a redirect', () => {})",
  ])
  const failures = checkGuards(text).failures
  assert.equal(failures.length, 2)
  assert.deepEqual(failures.map((f) => f.guard), [2, 2])
  assert.match(failures[0].detail, /removes 1 more test declaration/)
  assert.match(failures[1].detail, /silenced test/)
})

test('guard 2 refuses a newly ignored Rust test', () => {
  const text = diff('crates/ritornello-core/src/status/mod.rs', ['+#[ignore]', ' #[test]'])
  assert.deepEqual(failedGuards(text), [2])
})

test('guard 2 refuses a newly added .only, which silences every sibling', () => {
  const text = diff('web/app/src/views/Home.test.ts', ["+  it.only('one case', () => {})"])
  assert.deepEqual(failedGuards(text), [2])
})

test('guard 2 counts Rust tests as well as web ones', () => {
  const text = diff('crates/ritornello-core/src/status/mod.rs', ['-    #[test]', '-    fn reports_idle() {}'])
  assert.deepEqual(failedGuards(text), [2])
})

test('guard 2 does not mistake submit( for a test', () => {
  // Without the word boundary, `\bit\s*\(` fires inside `submit(`, and
  // deleting an ordinary line from a form reads as a test being removed.
  //
  // The diff has to be **asymmetric** to prove that. A first version of this
  // case removed and added a line that both contained `submit(`: the count
  // stayed equal, the guard stayed quiet, and the case passed with or without
  // the boundary. It survived the mutation, which is how it was found.
  const text = diff('web/app/src/views/Form.ts', ['-  submit(form)', '+  send(form)'])
  assert.equal(checkGuards(text).ok, true)
})

test('guard 3 refuses a package that has just gained an install script', () => {
  const text = diff('package-lock.json', ['+      "hasInstallScript": true,'])
  assert.deepEqual(failedGuards(text), [3])
})

test('guard 3 ignores an install script that was already declared', () => {
  const text = diff('package-lock.json', ['      "hasInstallScript": true,', '-      "version": "0.24.2"', '+      "version": "0.25.0"'])
  assert.equal(checkGuards(text).ok, true)
})

test('guard 3 ignores hasInstallScript false', () => {
  const text = diff('package-lock.json', ['+      "hasInstallScript": false,'])
  assert.equal(checkGuards(text).ok, true)
})

test('guard 3 only looks at lockfiles', () => {
  const text = diff('docs/installation.md', ['+A package may declare "hasInstallScript": true.'])
  assert.equal(checkGuards(text).ok, true)
})

test('all three can fail at once, and each is reported', () => {
  const text = [
    diff('.github/workflows/ci.yml', ['+        run: true']),
    diff('web/kit/src/lib/api.test.ts', ["-  it('a', () => {})"]),
    diff('package-lock.json', ['+      "hasInstallScript": true,']),
  ].join('\n')
  assert.deepEqual(failedGuards(text), [1, 2, 3])
})

test('a failure names the file, so the comment can say where', () => {
  const text = diff('.github/workflows/ci.yml', ['+        run: true'])
  assert.match(checkGuards(text).failures[0].detail, /\.github\/workflows\/ci\.yml/)
})

test('CRLF line endings do not make guard 3 blind', () => {
  // Guard 3 matches the path with `endsWith`, so a trailing carriage return
  // on the `+++` line is enough to silence it -- and silently, which is the
  // worst kind. Guards 1 and 2 would not notice: `startsWith` and the
  // content patterns are all unanchored, so an earlier version of this case
  // asserted on guard 1 and proved nothing.
  const text = diff('package-lock.json', ['+      "hasInstallScript": true,']).replace(/\n/g, '\r\n')
  assert.deepEqual(failedGuards(text), [3])
})
