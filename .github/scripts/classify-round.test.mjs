import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { classifyRound, DEPENDABOT_EMAIL, TRUNCATION_CAP } from './classify-round.mjs'

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

// The cases below are why this predicate has three states rather than two.
// A two-state version -- ours, else round 1 -- answers `1` to every one of
// them, and the step that acts on a `merge` verdict would merge a branch
// nobody reviewed. Proven by mutation against a valid two-state predicate,
// not a broken one; the count of tests it turns red depends on which mutant,
// so it is not quoted here.
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

// And these are why it reads EVERY author rather than the head's alone, and
// why a stranger outranks everything. The first is a pull request whose head
// is Dependabot's while a commit of ours sits underneath: reading the head
// would answer `1` and spend a second analysis and a second fix on a bump we
// have already failed to adapt. The two after it are the Critical that came
// of getting the precedence the other way round.
test('our commit under a later Dependabot commit is still round 2', () => {
  // The realistic shape: our fix failed, round 2 stopped, and someone then
  // commented `@dependabot rebase`.
  assert.equal(round(DEPENDABOT_EMAIL, OURS, DEPENDABOT_EMAIL), '2')
})

test('A HAND COMMIT OUTRANKS OURS, and this order is the safety property', () => {
  // This assertion was the other way round for one commit, and it was a
  // Critical. The reasoning then was that round 2 is "the stricter refusal" --
  // true while round 2 only refused, false the moment it could MERGE on a
  // green CI. The sequence it opened: our fix fails, the round-2 comment
  // invites the owner to adapt it, they push a commit, CI goes green, and
  // their unreviewed work is merged under them because our commit underneath
  // still said round 2. #20 is the precedent for a human hand-carrying a fix
  // onto a Dependabot branch.
  assert.equal(round(DEPENDABOT_EMAIL, OURS, HUMAN), 'hand')
})

test('a hand commit under our own fix is still a refusal', () => {
  // Order within the list must not matter either.
  assert.equal(round(DEPENDABOT_EMAIL, HUMAN, OURS), 'hand')
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

test('a listing at the API cap refuses, since a stranger could be beyond it', () => {
  // `pulls/{n}/commits` returns at most 250 entries even with `--paginate`.
  // At that size the absence of a stranger is not evidence of absence, and the
  // precedence this module rests on turns on exactly that absence.
  const many = Array.from({ length: TRUNCATION_CAP }, () => DEPENDABOT_EMAIL)
  assert.equal(classifyRound({ authors: many, ourEmail: OURS }).round, 'hand')
  assert.equal(classifyRound({ authors: many.slice(0, TRUNCATION_CAP - 1), ourEmail: OURS }).round, '1')
})

test('a non-array authors throws instead of being coerced', () => {
  // The entry point splits a string into an array. If a caller ever passes the
  // string straight through, `includes` on a string would match a SUBSTRING --
  // so a stranger whose address merely contains ours would read as round 2.
  assert.throws(() => classifyRound({ authors: OURS, ourEmail: OURS }), /authors/)
})

// --- The entry point ---------------------------------------------------------
//
// The exported function was covered and the script around it was not: the
// `AUTHORS` split, the `?? ''` fallback and the `GITHUB_OUTPUT` write are what
// the workflow actually executes, and a mistake in any of them is invisible to
// every test above.

const SCRIPT = fileURLToPath(new URL('./classify-round.mjs', import.meta.url))

function runEntryPoint(env) {
  const dir = mkdtempSync(join(tmpdir(), 'classify-round-'))
  const outputFile = join(dir, 'output.txt')
  try {
    const stdout = execFileSync(process.execPath, [SCRIPT], {
      encoding: 'utf8',
      env: { ...process.env, GITHUB_OUTPUT: outputFile, AUTHORS: '', BOT_EMAIL: OURS, ...env },
    })
    return { stdout, output: readFileSync(outputFile, 'utf8') }
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

test('entry point: one Dependabot author writes round=1 and prints nothing', () => {
  const { stdout, output } = runEntryPoint({ AUTHORS: DEPENDABOT_EMAIL })
  assert.equal(output, 'round=1\n')
  assert.equal(stdout, '')
})

test('entry point: the workflow passes a trailing space, which must not become an author', () => {
  // `tr` leaves exactly that, and an empty entry would read as a stranger and
  // refuse every pull request.
  const { output } = runEntryPoint({ AUTHORS: `${DEPENDABOT_EMAIL} ` })
  assert.equal(output, 'round=1\n')
})

test('entry point: several authors on one line are split', () => {
  const { stdout, output } = runEntryPoint({ AUTHORS: `${DEPENDABOT_EMAIL} ${OURS} ` })
  assert.equal(output, 'round=2\n')
  assert.match(stdout, /a fix has already been attempted/)
})

test('entry point: a stranger anywhere in the line refuses', () => {
  const { stdout, output } = runEntryPoint({ AUTHORS: `${DEPENDABOT_EMAIL} ${OURS} ${HUMAN}` })
  assert.equal(output, 'round=hand\n')
  assert.match(stdout, /pushed to this branch by hand/)
})

test('entry point: an unset AUTHORS refuses rather than defaulting to round 1', () => {
  // What an API failure leaves behind. The fallback splits to nothing, and no
  // authors is a refusal.
  const { output } = runEntryPoint({ AUTHORS: '' })
  assert.equal(output, 'round=hand\n')
})

test('entry point: an unset BOT_EMAIL fails loudly instead of misclassifying', () => {
  assert.throws(() => runEntryPoint({ AUTHORS: DEPENDABOT_EMAIL, BOT_EMAIL: '' }), /ourEmail|Command failed/)
})
