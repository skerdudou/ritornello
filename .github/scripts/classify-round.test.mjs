import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { classifyRound, DEPENDABOT_EMAIL, GITHUB_COMMITTER_EMAIL, TRUNCATION_CAP } from './classify-round.mjs'

const OURS = '41898282+github-actions[bot]@users.noreply.github.com'
const HUMAN = 'steven.kerdudou@kleegroup.com'

// The two pairs the workflow ever produces, measured on #12, #17, #18, #24,
// #25 and #26.
const dependabot = { author: DEPENDABOT_EMAIL, committer: GITHUB_COMMITTER_EMAIL }
const ours = { author: OURS, committer: OURS }

const round = (...commits) => classifyRound({ commits, ourEmail: OURS }).round

test('a pull request that is only ours is round 2', () => {
  assert.equal(round(ours), '2')
})

test("a pull request that is only Dependabot's is round 1", () => {
  assert.equal(round(dependabot), '1')
})

test('several Dependabot commits are still round 1', () => {
  // A rebase, or a grouped update that lands as more than one commit.
  assert.equal(round(dependabot, dependabot, dependabot), '1')
})

// Why the predicate has three states rather than two. A two-state version --
// ours, else round 1 -- answers `1` to every case below, and the step that acts
// on a `merge` verdict would merge a branch nobody reviewed. Proven by mutation
// against a VALID two-state predicate, not a broken one.
test('a hand commit on a Dependabot branch is a refusal, not a first round', () => {
  // #20 is this case: the TypeScript 6 configuration changes were carried onto
  // a Dependabot branch by hand.
  assert.equal(round(dependabot, { author: HUMAN, committer: HUMAN }), 'hand')
})

test('an empty identity refuses rather than defaulting to round 1', () => {
  // What an API failure leaves in the output it was meant to fill.
  assert.equal(round({ author: '', committer: '' }), 'hand')
})

test('an address that merely looks like Dependabot is refused', () => {
  // Same shape, different account id. String equality against the measured
  // literal is the test, not a pattern a lookalike could satisfy.
  assert.equal(round({ author: '1+dependabot[bot]@users.noreply.github.com', committer: GITHUB_COMMITTER_EMAIL }), 'hand')
})

// Why it reads EVERY commit rather than the head alone, and why a stranger
// outranks a round-2 state. The first is a pull request whose head is
// Dependabot's while a commit of ours sits underneath. The two after it are the
// Critical that came of getting the precedence the other way round.
test('our commit under a later Dependabot commit is still round 2', () => {
  // Our fix failed, round 2 stopped, and someone then commented
  // `@dependabot rebase`.
  assert.equal(round(dependabot, ours, dependabot), '2')
})

test('A HAND COMMIT OUTRANKS OURS, and this order is the safety property', () => {
  // This assertion was the other way round for one commit, and it was a
  // Critical. The reasoning then was that round 2 is "the stricter refusal" --
  // true while round 2 only refused, false the moment it could MERGE on a
  // green CI. The sequence it opened: our fix fails, the round-2 comment
  // invites the owner to adapt it, they push a commit, CI goes green, and
  // their unreviewed work is merged under them because our commit underneath
  // still said round 2.
  assert.equal(round(dependabot, ours, { author: HUMAN, committer: HUMAN }), 'hand')
})

test('a hand commit under our own fix is still a refusal', () => {
  // Order within the list must not matter either.
  assert.equal(round(dependabot, { author: HUMAN, committer: HUMAN }, ours), 'hand')
})

// And why the COMMITTER is read as well as the author.
test('OUR COMMIT AMENDED BY A HUMAN IS NO LONGER OURS', () => {
  // `git commit --amend` and `git rebase` preserve the author and rewrite the
  // committer. Reading the author alone, this is still {Dependabot, ours} --
  // round 2, green CI, merged, with the owner's unreviewed content in it. The
  // round-2 comment invites exactly this ("adapt the fix"), so it is the
  // likely case rather than an exotic one.
  assert.equal(round(dependabot, { author: OURS, committer: HUMAN }), 'hand')
})

test("a Dependabot commit amended by a human is no longer Dependabot's", () => {
  // The same route through the other identity: amend the bump commit itself
  // and the pull request would read as a pristine round 1, be analysed, and --
  // green CI, `merge` verdict, nothing written -- be merged.
  assert.equal(round({ author: DEPENDABOT_EMAIL, committer: HUMAN }), 'hand')
})

test('a commit authored by a human but committed by GitHub is a stranger', () => {
  // The web editor's shape: edit a file on the branch through the GitHub UI
  // and the commit carries your address as author and `noreply@github.com` as
  // committer. The committer alone must not be what admits a commit.
  assert.equal(round(dependabot, { author: HUMAN, committer: GITHUB_COMMITTER_EMAIL }), 'hand')
})

test('our identity on only one of the two fields is not ours', () => {
  assert.equal(round({ author: OURS, committer: GITHUB_COMMITTER_EMAIL }), 'hand')
  assert.equal(round({ author: DEPENDABOT_EMAIL, committer: OURS }), 'hand')
})

test('no commits at all is a refusal, not a first round', () => {
  // The listing failed, or returned nothing. Inventing a state from an
  // absence is the fail-open direction.
  assert.equal(classifyRound({ commits: [], ourEmail: OURS }).round, 'hand')
})

test('round 2 explains itself and round 1 has nothing to say', () => {
  assert.match(classifyRound({ commits: [ours], ourEmail: OURS }).note, /a fix has already been attempted/)
  assert.equal(classifyRound({ commits: [dependabot], ourEmail: OURS }).note, null)
})

test('the refusal names every stranger pair it refused, once each', () => {
  const { note } = classifyRound({
    commits: [
      dependabot,
      { author: 'a@example.com', committer: 'a@example.com' },
      { author: 'b@example.com', committer: 'b@example.com' },
      { author: 'a@example.com', committer: 'a@example.com' },
    ],
    ourEmail: OURS,
  })
  assert.match(note, /a@example\.com/)
  assert.match(note, /b@example\.com/)
  assert.equal(note.match(/a@example\.com \/ a@example\.com/g).length, 1, 'a repeated pair must not be listed twice')
  assert.match(note, /pushed to this branch by hand, or rewrote a commit/)
})

test('a missing ourEmail throws instead of silently matching nothing', () => {
  // Without this, an unset BOT_EMAIL in the workflow would make every commit
  // -- ours included -- read as `hand`, and a round-2 fix would be analysed
  // from scratch for ever. A loud failure beats a silent misclassification.
  assert.throws(() => classifyRound({ commits: [ours], ourEmail: undefined }), /ourEmail/)
})

test('a listing at the API cap refuses, since a stranger could be beyond it', () => {
  const many = Array.from({ length: TRUNCATION_CAP }, () => dependabot)
  assert.equal(classifyRound({ commits: many, ourEmail: OURS }).round, 'hand')
  assert.equal(classifyRound({ commits: many.slice(0, TRUNCATION_CAP - 1), ourEmail: OURS }).round, '1')
})

test('a non-array commits throws instead of being coerced', () => {
  assert.throws(() => classifyRound({ commits: 'nope', ourEmail: OURS }), /commits/)
})

// --- The entry point ---------------------------------------------------------
//
// The exported function was covered and the script around it was not: the
// `COMMITS` parse, the `?? ''` fallback and the `GITHUB_OUTPUT` write are what
// the workflow actually executes, and a mistake in any of them is invisible to
// every test above.

const SCRIPT = fileURLToPath(new URL('./classify-round.mjs', import.meta.url))
const pair = (c) => `${c.author}|${c.committer}`

function runEntryPoint(env) {
  const dir = mkdtempSync(join(tmpdir(), 'classify-round-'))
  const outputFile = join(dir, 'output.txt')
  try {
    const stdout = execFileSync(process.execPath, [SCRIPT], {
      encoding: 'utf8',
      env: { ...process.env, GITHUB_OUTPUT: outputFile, COMMITS: '', BOT_EMAIL: OURS, ...env },
    })
    return { stdout, output: readFileSync(outputFile, 'utf8') }
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

test('entry point: one Dependabot pair writes round=1 and prints nothing', () => {
  const { stdout, output } = runEntryPoint({ COMMITS: pair(dependabot) })
  assert.equal(output, 'round=1\n')
  assert.equal(stdout, '')
})

test('entry point: the trailing space the workflow leaves is not a commit', () => {
  const { output } = runEntryPoint({ COMMITS: `${pair(dependabot)} ` })
  assert.equal(output, 'round=1\n')
})

test('entry point: several pairs on one line are split', () => {
  const { stdout, output } = runEntryPoint({ COMMITS: `${pair(dependabot)} ${pair(ours)} ` })
  assert.equal(output, 'round=2\n')
  assert.match(stdout, /a fix has already been attempted/)
})

test('entry point: a stranger anywhere in the line refuses', () => {
  const { stdout, output } = runEntryPoint({ COMMITS: `${pair(dependabot)} ${pair(ours)} ${HUMAN}|${HUMAN}` })
  assert.equal(output, 'round=hand\n')
  assert.match(stdout, /pushed to this branch by hand/)
})

test('entry point: an amended commit of ours refuses', () => {
  // The amend route, through the parse the workflow actually uses.
  const { output } = runEntryPoint({ COMMITS: `${pair(dependabot)} ${OURS}|${HUMAN}` })
  assert.equal(output, 'round=hand\n')
})

test('entry point: an entry with no pipe is a stranger, not a half-match', () => {
  // A malformed entry must not become a pair that happens to match something.
  // It matches nothing, which is `hand`.
  const { output } = runEntryPoint({ COMMITS: DEPENDABOT_EMAIL })
  assert.equal(output, 'round=hand\n')
})

test('entry point: an unset COMMITS refuses rather than defaulting to round 1', () => {
  const { output } = runEntryPoint({ COMMITS: '' })
  assert.equal(output, 'round=hand\n')
})

test('entry point: an unset BOT_EMAIL fails loudly instead of misclassifying', () => {
  assert.throws(() => runEntryPoint({ COMMITS: pair(dependabot), BOT_EMAIL: '' }), /ourEmail|Command failed/)
})
