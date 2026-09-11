import { appendFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

// Decide which round of the analysis a pull request is in, from the identities
// on its commits.
//
// Three answers, and the third one is why this is a file rather than three
// lines of shell: **"not ours" is not the same claim as "Dependabot's".** A
// two-state test -- ours, else round 1 -- reads a commit pushed BY HAND onto a
// Dependabot branch as a pristine first round, and the step that acts on a
// `merge` verdict would then merge a human's unreviewed work under them. That
// is not hypothetical here: #20 exists because the TypeScript 6 configuration
// changes were hand-carried onto exactly such a branch.
//
// `hand` is a refusal, and every caller spells its condition `round == '1'` or
// `round == '2'`, so a third value refuses all of them. An identity this
// function does not recognise -- including the empty string an API failure
// leaves behind -- is `hand`. Refusing an unknown is fail-closed; guessing
// round 1 is not.
//
// **EVERY commit, not just the head.** Reading the head alone did not bound
// the loop: we push a fix, CI goes red, round 2 stops for good, and then one
// `@dependabot rebase` puts a Dependabot commit on top, the head reads as
// round 1, and a second analysis writes a second fix.
//
// **AUTHOR AND COMMITTER, not the author alone.** `git commit --amend` and
// `git rebase` PRESERVE the author and rewrite the committer, so an owner who
// adapts our failed fix by amending it -- which the round-2 comment invites in
// so many words -- would keep the list at {Dependabot, ours}, land in round 2,
// and have their unreviewed work merged the moment CI went green. Reading the
// pair closes that: their amend rewrites the committer, the pair stops
// matching, and the verdict is `hand`.
//
// The risk of reading the pair is the opposite one -- a legitimate shape whose
// pair matches neither literal would turn a working pull request into `hand`.
// Swept against every Dependabot pull request this repository has ever had,
// open or closed: 17 commits, and not one carries a pair this refuses.
//
// Measured on #12, #17, #18, #24, #25 and #26, every commit identical:
// Dependabot's carry author `49699333+dependabot[bot]@…` with committer
// `noreply@github.com` (GitHub signs what it creates through the API). Ours
// are written by `git commit` on the runner with `-c user.email`, which sets
// both fields to the same address. Anything else is a stranger.
//
// **What this does NOT check is the signature**, and `dependabot/fetch-metadata`
// does. Both fields are plain text a local commit can carry, so a forged pair
// is possible -- but only from an actor with write access to this repository,
// who can merge the pull request themselves, so it is not an escalation. What
// the pair does close is the ACCIDENT, which is the realistic case: an amend, a
// rebase, an "Update branch" click.
//
// The honest statement of the property, weaker than "one fix per pull request,
// ever": ONE FIX PER PULL REQUEST FOR AS LONG AS THAT FIX IS ON IT UNCHANGED.

// Dependabot's identity, and GitHub's own committer on anything created
// through the API. Measured, not assumed, and not ecosystem-specific: #18 is
// npm_and_yarn, #24 is cargo, and the pair is the same.
export const DEPENDABOT_EMAIL = '49699333+dependabot[bot]@users.noreply.github.com'
export const GITHUB_COMMITTER_EMAIL = 'noreply@github.com'

// `GET /repos/{o}/{r}/pulls/{n}/commits` returns at most 250 entries, and
// `--paginate` does not lift that.
export const TRUNCATION_CAP = 250

export function classifyRound({
  commits,
  ourEmail,
  dependabotEmail = DEPENDABOT_EMAIL,
  githubCommitterEmail = GITHUB_COMMITTER_EMAIL,
}) {
  // `ourEmail` is a required input rather than a constant here on purpose: the
  // workflow declares that literal at job level because its push step needs
  // the same one, and a second copy in this file is how the two drift apart.
  if (!ourEmail) {
    throw new Error('classifyRound needs ourEmail: the identity our own fix commits are authored under')
  }
  if (!Array.isArray(commits)) {
    throw new Error('classifyRound needs commits: every commit on the pull request, as an array of { author, committer }')
  }

  // No commits is not a first round. It means the listing failed or returned
  // nothing, and inventing a state from an absence is the fail-open direction.
  if (commits.length === 0) {
    return {
      round: 'hand',
      note: 'No commits could be read for this pull request, so there is nothing to decide automatically.',
    }
  }

  // A listing that may be truncated cannot answer "is there a stranger here",
  // and the precedence below turns on exactly that absence.
  if (commits.length >= TRUNCATION_CAP) {
    return {
      round: 'hand',
      note: `This pull request lists ${commits.length} commits, at or beyond the ${TRUNCATION_CAP} the API returns, so the listing may be truncated and the absence of a hand-written commit cannot be established.`,
    }
  }

  const isDependabot = (c) => c.author === dependabotEmail && c.committer === githubCommitterEmail
  const isOurs = (c) => c.author === ourEmail && c.committer === ourEmail

  // **A stranger anywhere wins, and the order of these two tests is the whole
  // safety property.** It was the other way round for one commit, on the
  // reasoning that round 2 is "the stricter refusal". True while round 2 only
  // refused; false the moment round 2 could MERGE on a green CI.
  const strangers = [...new Set(
    commits.filter((c) => !isDependabot(c) && !isOurs(c)).map((c) => `${c.author} / ${c.committer}`),
  )]
  if (strangers.length > 0) {
    return {
      round: 'hand',
      note:
        `Commits here carry ${strangers.map((s) => `\`${s}\``).join(', ')} as author / committer, which is ` +
        "neither Dependabot's pair nor ours. Someone pushed to this branch by hand, or rewrote a commit that " +
        'was on it, so there is nothing here to decide automatically.',
    }
  }

  if (commits.some(isOurs)) {
    return {
      round: '2',
      note: 'Round 2: one of the commits here is ours, so a fix has already been attempted.',
    }
  }

  return { round: '1', note: null }
}

// Entry point. Reads BOT_EMAIL and COMMITS from the environment, writes
// `round=` to GITHUB_OUTPUT, and prints its note for the caller to tee into
// the step summary. A round with nothing to say prints nothing.
if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  // One line, space-separated, each entry `author|committer`. A multi-line
  // step output would need a delimiter chosen so the values cannot close it;
  // this needs none.
  //
  // These are git identity fields, which whoever pushes sets and which CAN
  // contain a space or a pipe. That is not a claim the parse is faithful: an
  // address carrying either splits into pieces that match nothing, and the
  // verdict is `hand`. Fail-closed, so it is left to happen.
  const commits = (process.env.COMMITS ?? '')
    .split(/\s+/)
    .filter(Boolean)
    .map((pair) => {
      const [author = '', committer = ''] = pair.split('|')
      return { author, committer }
    })

  const { round, note } = classifyRound({ commits, ourEmail: process.env.BOT_EMAIL })

  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `round=${round}\n`)
  }
  if (note) {
    console.log(note)
  }
}
