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
  // The count genuinely holds here: one declaration removed, one added, and
  // the skipped one is not counted at all, since `\bit\s*\(` does not match
  // `it.skip(`. So the silencer check is the only thing that can fire, which
  // is what this case is for -- asserted on `failures.length` so that a
  // count-drop sneaking back in would show up as a second failure.
  //
  // An earlier version used a diff where the count *also* dropped, which made
  // the name a false claim about the mechanism and proved nothing the next
  // case does not.
  const text = diff('web/kit/src/lib/api.test.ts', [
    "-  it('follows a redirect', () => {})",
    "+  it('follows a redirect', () => {})",
    "+  it.skip('handles a 500', () => {})",
  ])
  assert.deepEqual(failedGuards(text), [2])
  assert.equal(checkGuards(text).failures.length, 1)
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

test('guard 1 refuses a deletion under .github, as the first file in the diff', () => {
  // A deletion emits `+++ /dev/null`. The first parser kept the previous
  // file's path here -- or null, for the first file -- and dropped it, so
  // deleting the workflow that runs the tests was invisible. This is the
  // module's whole purpose, and it was open.
  const text = [
    'diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml',
    'deleted file mode 100644',
    'index 1111111..0000000',
    '--- a/.github/workflows/ci.yml',
    '+++ /dev/null',
    '@@ -1,2 +0,0 @@',
    '-name: CI',
    '-on: pull_request',
  ].join('\n')
  assert.deepEqual(failedGuards(text), [1])
})

test('guard 1 refuses a deletion under .github that follows an innocent file', () => {
  // The stale-path variant, which is the nastier of the two: without the
  // source path the removed lines are attributed to the *previous* file and
  // the deletion passes while looking accounted for.
  const text = [
    diff('web/kit/src/lib/api.ts', ['-  const a = 1', '+  const a = 2']),
    'diff --git a/.github/dependabot.yml b/.github/dependabot.yml',
    'deleted file mode 100644',
    '--- a/.github/dependabot.yml',
    '+++ /dev/null',
    '@@ -1,1 +0,0 @@',
    '-version: 2',
  ].join('\n')
  assert.deepEqual(failedGuards(text), [1])
})

test('guard 1 refuses moving a file out of .github and editing it at once', () => {
  // Renaming this very file to `scripts/` while weakening its predicate: the
  // destination is outside `.github/`, so only the `---` line and the
  // `rename from` line carry the fact that a control was touched.
  const text = [
    'diff --git a/.github/scripts/check-guards.mjs b/scripts/check-guards.mjs',
    'similarity index 90%',
    'rename from .github/scripts/check-guards.mjs',
    'rename to scripts/check-guards.mjs',
    '--- a/.github/scripts/check-guards.mjs',
    '+++ b/scripts/check-guards.mjs',
    '@@ -1,1 +1,1 @@',
    '-export function checkGuards(diffText) {',
    '+export function checkGuards() { return { ok: true, failures: [] } }',
  ].join('\n')
  assert.deepEqual(failedGuards(text), [1])
})

test('guard 1 refuses a rename out of .github that carries no hunk at all', () => {
  // A 100%-similarity rename emits no `---`, no `+++` and no `@@`. Only
  // `rename from` names the old path.
  const text = [
    'diff --git a/.github/workflows/ci.yml b/ci.yml',
    'similarity index 100%',
    'rename from .github/workflows/ci.yml',
    'rename to ci.yml',
  ].join('\n')
  assert.deepEqual(failedGuards(text), [1])
})

test('guard 1 refuses a mode-only change under .github, which emits no +++ line', () => {
  // Nothing but the `diff --git` header names the file here.
  const text = [
    'diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml',
    'old mode 100644',
    'new mode 100755',
  ].join('\n')
  assert.deepEqual(failedGuards(text), [1])
})

test('a removed line that looks like a diff header is content, not a header', () => {
  // A documentation file that quotes a diff. `-` prefixed onto
  // `-- a/.github/workflows/ci.yml` arrives here as `--- a/...`, which is
  // byte-for-byte a header. Recognising headers only before the first `@@`
  // is what stops this from inventing a touched file and **refusing a diff
  // that touches nothing of the sort**.
  //
  // The first version of this case used a harmless line and asserted `ok`.
  // That proved nothing: the mutation which recognises headers everywhere
  // also stops collecting content lines, so an empty result still reads as
  // `ok`. The assertion has to be that a phantom header causes a *false
  // refusal*, which is the failure that would actually be felt.
  const text = diff('docs/development.md', [
    '--- a/.github/workflows/ci.yml',
    '+-- a/.github/workflows/ci.yml',
  ])
  assert.equal(checkGuards(text).ok, true)
  assert.deepEqual(checkGuards(text).failures, [])
})

test('guard 2 does not count a test marker inside a comment-only added line', () => {
  // An added comment pads `after` and makes the guard more permissive, so a
  // real deletion could be offset by prose. With dense comments mandated in
  // this repository the collision is realistic, not adversarial.
  const text = diff('web/kit/src/lib/api.test.ts', [
    "-  it('follows a redirect', () => {})",
    '+  // dropped it (the 5.0 changelog explains why)',
  ])
  assert.deepEqual(failedGuards(text), [2])
})

test('guard 2 does not count a test marker inside a comment-only removed line', () => {
  // The other direction: a removed comment pads `before` and would refuse a
  // diff that loses no coverage at all.
  const text = diff('web/kit/src/lib/api.ts', ['-  // call it (once)', '+  callOnce()'])
  assert.equal(checkGuards(text).ok, true)
})

test('guard 2 still counts a Rust test attribute, whose line starts with #', () => {
  // The regression guard for the comment filter: `#` must never be a comment
  // prefix, or Rust tests stop being counted entirely.
  const text = diff('crates/ritornello-core/src/status/mod.rs', ['-    #[test]'])
  assert.deepEqual(failedGuards(text), [2])
})

test('guard 2 refuses a conditional Rust ignore, which changes no count', () => {
  // `#[cfg_attr(..., ignore)]` silences a test without matching `#[ignore]`
  // and without touching the `#[test]` line, so it cleared both halves.
  const text = diff('crates/ritornello-core/src/status/mod.rs', [
    '+#[cfg_attr(target_os = "windows", ignore)]',
    ' #[test]',
  ])
  assert.deepEqual(failedGuards(text), [2])
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
