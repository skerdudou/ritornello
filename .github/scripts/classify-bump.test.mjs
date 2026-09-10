import { test } from 'node:test'
import assert from 'node:assert/strict'
import { classifyBump, parseVersion, pinnedIndex } from './classify-bump.mjs'

const patch = (dependencyName, newVersion) => ({ dependencyName, newVersion, updateType: 'version-update:semver-patch' })
const minor = (dependencyName, newVersion) => ({ dependencyName, newVersion, updateType: 'version-update:semver-minor' })
const major = (dependencyName, newVersion) => ({ dependencyName, newVersion, updateType: 'version-update:semver-major' })

test('a minor bump above 1.0 leaves the pinned component alone', () => {
  assert.equal(classifyBump([minor('serde', '1.3.0')]).compatible, true)
})

test('a patch bump above 1.0 leaves the pinned component alone', () => {
  assert.equal(classifyBump([patch('flate2', '1.1.10')]).compatible, true)
})

test('build metadata is not a reason to refuse: the toml bump that held #23 back', () => {
  // `toml 1.1.4+spec-1.1.0 -> 1.1.5+spec-1.1.0` is a plain patch. The old
  // regexp rejected the `+spec-1.1.0` suffix and a human had to merge it.
  const verdict = classifyBump([patch('toml', '1.1.5+spec-1.1.0')])
  assert.equal(verdict.compatible, true, verdict.reasons.join('; '))
})

test('a minor bump below 1.0 moves the component a caret pins', () => {
  const verdict = classifyBump([minor('sha2', '0.11.0')])
  assert.equal(verdict.compatible, false)
  assert.match(verdict.reasons[0], /sha2/)
})

test('a patch bump on a 0.0.x version moves the component a caret pins', () => {
  assert.equal(classifyBump([patch('obscure', '0.0.4')]).compatible, false)
})

test('a patch bump on a 0.x version does not', () => {
  assert.equal(classifyBump([patch('evdev', '0.13.3')]).compatible, true)
})

test('a major is always refused', () => {
  assert.equal(classifyBump([major('vitest', '5.0.0')]).compatible, false)
})

test('a prerelease stays unreadable, and unreadable means refused', () => {
  const verdict = classifyBump([minor('typescript', '6.0.0-beta')])
  assert.equal(verdict.compatible, false)
  assert.match(verdict.reasons[0], /unreadable/)
})

test('an empty version string is refused, because Number("") is 0', () => {
  // `''.split('.').map(Number)` yields `[0]`, which reads as an ordinary
  // version. The shape has to be checked before splitting.
  assert.equal(classifyBump([patch('ghost', '')]).compatible, false)
})

test('an unknown update type is refused rather than ignored', () => {
  const odd = { dependencyName: 'x', newVersion: '1.2.3', updateType: 'version-update:semver-unknown' }
  assert.equal(classifyBump([odd]).compatible, false)
})

test('a group is judged over all its members, not the first', () => {
  const verdict = classifyBump([patch('flate2', '1.1.10'), minor('tower', '0.5.3')])
  assert.equal(verdict.compatible, false)
  assert.equal(verdict.reasons.length, 1)
  assert.match(verdict.reasons[0], /tower/)
})

test('no metadata at all is refused, not waved through', () => {
  assert.equal(classifyBump([]).compatible, false)
  assert.equal(classifyBump(undefined).compatible, false)
})

test('parseVersion keeps the numeric core and drops build metadata', () => {
  assert.deepEqual(parseVersion('1.1.5+spec-1.1.0'), [1, 1, 5])
  assert.equal(parseVersion('1.0.0-rc.1'), null)
  assert.equal(parseVersion(''), null)
  assert.equal(parseVersion(undefined), null)
})

test('pinnedIndex finds the leftmost non-zero component', () => {
  assert.equal(pinnedIndex([1, 2, 3]), 0)
  assert.equal(pinnedIndex([0, 4, 13]), 1)
  assert.equal(pinnedIndex([0, 0, 3]), 2)
  assert.equal(pinnedIndex([0, 0, 0]), -1)
})
