import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readBumpMetadata } from './read-bump-metadata.mjs'

// The `updated-dependencies:` block below is byte-identical to the one in
// commit e3fcd9e of this repository -- verified by comparison, not by memory.
// The prose around it is abridged: the parser stops at the block's `...`
// terminator, so nothing outside the block is input, and pretending otherwise
// would invite an assertion on text this fixture does not faithfully carry.
const SINGLE = `chore(deps): bump sha2 from 0.10.9 to 0.11.0 (#24)

Bumps [sha2](https://github.com/RustCrypto/hashes) from 0.10.9 to 0.11.0.
- [Commits](https://github.com/RustCrypto/hashes/compare/sha2-v0.10.9...sha2-v0.11.0)

---
updated-dependencies:
- dependency-name: sha2
  dependency-version: 0.11.0
  dependency-type: direct:production
  update-type: version-update:semver-minor
...

Signed-off-by: dependabot[bot] <support@github.com>
`

// Same provenance, from commit 7360da5: the block is byte-identical, the
// prose abridged to the one line that shows a version carrying build metadata.
const GROUP = `chore(deps): bump the cargo-minor-and-patch group with 2 updates (#23)

Updates \`toml\` from 1.1.4+spec-1.1.0 to 1.1.5+spec-1.1.0

---
updated-dependencies:
- dependency-name: toml
  dependency-version: 1.1.5+spec-1.1.0
  dependency-type: direct:production
  update-type: version-update:semver-patch
  dependency-group: cargo-minor-and-patch
- dependency-name: flate2
  dependency-version: 1.1.10
  dependency-type: direct:production
  update-type: version-update:semver-patch
  dependency-group: cargo-minor-and-patch
...

Signed-off-by: dependabot[bot] <support@github.com>
`

const OURS = 'fix(deps): adapt to the vitest 5 runner API\n'

test('a single-dependency block yields the shape classifyBump consumes', () => {
  const read = readBumpMetadata([SINGLE])
  assert.deepEqual(read.updates, [
    { dependencyName: 'sha2', newVersion: '0.11.0', updateType: 'version-update:semver-minor' },
  ])
  assert.equal(read.group, null)
})

test('every member of a group is read, not just the first', () => {
  const read = readBumpMetadata([GROUP])
  assert.equal(read.updates.length, 2)
  assert.deepEqual(read.updates.map((u) => u.dependencyName), ['toml', 'flate2'])
  assert.equal(read.updates[0].newVersion, '1.1.5+spec-1.1.0')
  assert.equal(read.group, 'cargo-minor-and-patch')
})

test('our own fix commit on top does not hide the block underneath', () => {
  const read = readBumpMetadata([SINGLE, OURS])
  assert.equal(read.updates[0].dependencyName, 'sha2')
})

test('the oldest block wins when the head carries none', () => {
  const read = readBumpMetadata([GROUP, OURS, OURS])
  assert.equal(read.updates.length, 2)
})

test('no block at all returns null, which the caller treats as unreadable', () => {
  assert.equal(readBumpMetadata([OURS]), null)
  assert.equal(readBumpMetadata([]), null)
  assert.equal(readBumpMetadata(undefined), null)
})

test('an entry missing update-type is refused, not silently dropped', () => {
  const truncated = `x\n\n---\nupdated-dependencies:\n- dependency-name: sha2\n  dependency-version: 0.11.0\n...\n`
  assert.equal(readBumpMetadata([truncated]), null)
})

test('an entry missing dependency-version is refused', () => {
  const truncated = `x\n\n---\nupdated-dependencies:\n- dependency-name: sha2\n  update-type: version-update:semver-minor\n...\n`
  assert.equal(readBumpMetadata([truncated]), null)
})

test('an entry missing dependency-name is refused', () => {
  const truncated = `x\n\n---\nupdated-dependencies:\n- dependency-version: 0.11.0\n  update-type: version-update:semver-minor\n...\n`
  assert.equal(readBumpMetadata([truncated]), null)
})

test('a value outside the accepted character class is refused', () => {
  // A dependency name travels into a comment and into a prompt. Anything
  // that is not a package name is a reason to stop, not to sanitise.
  const hostile = `x\n\n---\nupdated-dependencies:\n- dependency-name: "a\`b; rm -rf /"\n  dependency-version: 1.0.0\n  update-type: version-update:semver-patch\n...\n`
  assert.equal(readBumpMetadata([hostile]), null)
})

test('a line that is neither an item nor a key inside the block is refused', () => {
  const malformed = `x\n\n---\nupdated-dependencies:\n- dependency-name: sha2\n  dependency-version: 0.11.0\n  update-type: version-update:semver-minor\nsomething-else\n...\n`
  assert.equal(readBumpMetadata([malformed]), null)
})

test('an empty block is refused rather than read as an empty list', () => {
  const empty = `x\n\n---\nupdated-dependencies:\n...\n`
  assert.equal(readBumpMetadata([empty]), null)
})

test('CRLF line endings are read the same as LF', () => {
  const read = readBumpMetadata([SINGLE.replace(/\n/g, '\r\n')])
  assert.equal(read.updates[0].dependencyName, 'sha2')
})

test('a scoped npm name is accepted', () => {
  const scoped = `x\n\n---\nupdated-dependencies:\n- dependency-name: "@vitejs/plugin-vue"\n  dependency-version: 6.0.1\n  update-type: version-update:semver-minor\n...\n`
  assert.equal(readBumpMetadata([scoped]).updates[0].dependencyName, '@vitejs/plugin-vue')
})
