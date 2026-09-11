import { test } from 'node:test'
import assert from 'node:assert/strict'
import { classifyRound, DEPENDABOT_EMAIL } from './classify-round.mjs'

const OURS = '41898282+github-actions[bot]@users.noreply.github.com'
const round = (author) => classifyRound({ author, ourEmail: OURS }).round

test('a commit we pushed ourselves is round 2', () => {
  assert.equal(round(OURS), '2')
})

test("Dependabot's own commit is round 1", () => {
  assert.equal(round(DEPENDABOT_EMAIL), '1')
})

// The three cases below are the whole reason this predicate has three states.
// A two-state version -- ours, else round 1 -- answers `1` to every one of
// them, and the step that acts on a `merge` verdict would merge a branch
// nobody reviewed. Proven by mutation, and the figure depends on which
// mutant: a valid two-state predicate that keeps a refusal branch passes the
// two tests above and fails exactly these three; one that deletes the branch
// outright also fails the note test below, for four. Either way the two
// recognised-identity tests pass, so they are not what proves this.
test('a hand commit on a Dependabot branch is a refusal, not a first round', () => {
  // #20 is this case: the TypeScript 6 configuration changes were carried onto
  // a Dependabot branch by hand.
  assert.equal(round('steven.kerdudou@kleegroup.com'), 'hand')
})

test('an empty author refuses rather than defaulting to round 1', () => {
  // What an API failure leaves in the output it was meant to fill. An unknown
  // identity must fail closed.
  assert.equal(round(''), 'hand')
})

test('an address that merely looks like Dependabot is refused', () => {
  // Same shape, different account id. String equality against the measured
  // literal is the test, not a pattern that a lookalike could satisfy.
  assert.equal(round('1+dependabot[bot]@users.noreply.github.com'), 'hand')
})

test('round 2 explains itself and round 1 has nothing to say', () => {
  assert.match(classifyRound({ author: OURS, ourEmail: OURS }).note, /a fix has already been attempted/)
  assert.equal(classifyRound({ author: DEPENDABOT_EMAIL, ourEmail: OURS }).note, null)
})

test('the refusal names the author it refused', () => {
  const { note } = classifyRound({ author: 'someone@example.com', ourEmail: OURS })
  assert.match(note, /someone@example\.com/)
  assert.match(note, /pushed to this branch by hand/)
})

test('a missing ourEmail throws instead of silently matching nothing', () => {
  // Without this, an unset BOT_EMAIL in the workflow would make every commit
  // -- ours included -- read as `hand`, and a round-2 fix would be analysed
  // from scratch for ever. A loud failure beats a silent misclassification.
  assert.throws(() => classifyRound({ author: OURS, ourEmail: undefined }), /ourEmail/)
})
