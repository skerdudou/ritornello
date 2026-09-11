import { test } from 'node:test'
import assert from 'node:assert/strict'
import { classifyRound, DEPENDABOT_EMAIL } from './classify-round.mjs'

const OURS = '41898282+github-actions[bot]@users.noreply.github.com'
const HUMAN = 'steven.kerdudou@kleegroup.com'
const round = (...authors) => classifyRound({ authors, ourEmail: OURS }).round

test('a pull request that is only ours is round 2', () => {
  assert.equal(round(OURS), '2')
})

test("a pull request that is only Dependabot's is round 1", () => {
  assert.equal(round(DEPENDABOT_EMAIL), '1')
})

test('several Dependabot commits are still round 1', () => {
  // A rebase, or a grouped update that lands as more than one commit.
  assert.equal(round(DEPENDABOT_EMAIL, DEPENDABOT_EMAIL, DEPENDABOT_EMAIL), '1')
})

// The three cases below are why this predicate has three states. A two-state
// version -- ours, else round 1 -- answers `1` to every one of them, and the
// step that acts on a `merge` verdict would merge a branch nobody reviewed.
// Proven by mutation, and the figure depends on which mutant: a valid
// two-state predicate that keeps a refusal branch fails exactly these three;
// one that deletes the branch outright also fails the note test below.
test('a hand commit on a Dependabot branch is a refusal, not a first round', () => {
  // #20 is this case: the TypeScript 6 configuration changes were carried onto
  // a Dependabot branch by hand.
  assert.equal(round(DEPENDABOT_EMAIL, HUMAN), 'hand')
})

test('an empty author refuses rather than defaulting to round 1', () => {
  // What an API failure leaves in the output it was meant to fill. An unknown
  // identity must fail closed.
  assert.equal(round(''), 'hand')
})

test('an address that merely looks like Dependabot is refused', () => {
  // Same shape, different account id. String equality against the measured
  // literal is the test, not a pattern a lookalike could satisfy.
  assert.equal(round('1+dependabot[bot]@users.noreply.github.com'), 'hand')
})

// And these are why it reads EVERY author rather than the head's alone. Each
// one is a pull request where the head is Dependabot's while a commit of ours
// sits underneath -- reading the head would answer `1` and spend a second
// analysis and a second fix on a bump we have already failed to adapt.
test('our commit under a later Dependabot commit is still round 2', () => {
  // The realistic shape: our fix failed, round 2 stopped, and someone then
  // commented `@dependabot rebase`.
  assert.equal(round(DEPENDABOT_EMAIL, OURS, DEPENDABOT_EMAIL), '2')
})

test('our commit outranks a hand commit too', () => {
  // Both are present. Round 2 wins, because it is the stricter refusal: it
  // stops without analysing, which is what having already tried means.
  assert.equal(round(DEPENDABOT_EMAIL, OURS, HUMAN), '2')
})

test('no commits at all is a refusal, not a first round', () => {
  // The listing failed, or returned nothing. Inventing a state from an
  // absence is the fail-open direction.
  assert.equal(classifyRound({ authors: [], ourEmail: OURS }).round, 'hand')
})

test('round 2 explains itself and round 1 has nothing to say', () => {
  assert.match(classifyRound({ authors: [OURS], ourEmail: OURS }).note, /a fix has already been attempted/)
  assert.equal(classifyRound({ authors: [DEPENDABOT_EMAIL], ourEmail: OURS }).note, null)
})

test('the refusal names every stranger it refused, once each', () => {
  const { note } = classifyRound({ authors: [DEPENDABOT_EMAIL, 'a@example.com', 'b@example.com', 'a@example.com'], ourEmail: OURS })
  assert.match(note, /a@example\.com/)
  assert.match(note, /b@example\.com/)
  assert.equal(note.match(/a@example\.com/g).length, 1, 'a repeated author must not be listed twice')
  assert.match(note, /pushed to this branch by hand/)
})

test('a missing ourEmail throws instead of silently matching nothing', () => {
  // Without this, an unset BOT_EMAIL in the workflow would make every commit
  // -- ours included -- read as `hand`, and a round-2 fix would be analysed
  // from scratch for ever. A loud failure beats a silent misclassification.
  assert.throws(() => classifyRound({ authors: [OURS], ourEmail: undefined }), /ourEmail/)
})

test('a non-array authors throws instead of being coerced', () => {
  // The entry point splits a string into an array. If a caller ever passes the
  // string straight through, `includes` on a string would match a SUBSTRING --
  // so a stranger whose address merely contains ours would read as round 2.
  assert.throws(() => classifyRound({ authors: OURS, ourEmail: OURS }), /authors/)
})
