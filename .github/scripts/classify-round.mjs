import { appendFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

// Decide which round of the analysis a pull request is in, from the identities
// that authored its commits.
//
// Three answers, and the third one is the reason this is a file rather than
// three lines of shell: **"not ours" is not the same claim as "Dependabot's".**
// A two-state test -- ours, else round 1 -- reads a commit pushed BY HAND onto
// a Dependabot branch as a pristine first round, and the step that acts on a
// `merge` verdict would then merge a human's unreviewed work under them. That
// is not hypothetical in this repository: #20 exists because the TypeScript 6
// configuration changes were hand-carried onto exactly such a branch.
//
// `hand` is a refusal, and every caller spells its condition `round == '1'`,
// so a third value refuses all of them without any further edit. Nothing is
// analysed, nothing is commented, nothing is merged: whoever pushed that
// commit has write access here and can merge it themselves.
//
// An author this function does not recognise -- including the empty string an
// API failure would leave behind -- is `hand`. Refusing an unknown is the
// fail-closed direction; guessing round 1 is not.
//
// **EVERY author on the pull request, not just the head's.** This is what
// bounds the loop, and reading the head alone did not deliver it: we push a
// fix, CI goes red, round 2 stops for good -- and then one `@dependabot
// rebase` puts a Dependabot commit on top, the head reads as Dependabot's,
// round 1 returns, and a second analysis writes a second fix. The budget the
// bound exists to enforce would be spent twice. Any commit of ours anywhere on
// the pull request means we have already had our attempt.
//
// The honest statement of the property, which is weaker than "one fix per pull
// request, ever": ONE FIX PER PULL REQUEST FOR AS LONG AS THAT FIX IS ON IT.
// If Dependabot force-pushes and our commit disappears, no author is ours and
// round 1 returns -- correct, since our fix is gone and the tree is new, but
// not the same claim.

// Dependabot's own commit identity. Measured, not assumed: every commit on
// #18 (npm_and_yarn) and #24 (cargo) carries this exact address, so it is the
// bot's account identity and not an ecosystem-specific one.
export const DEPENDABOT_EMAIL = '49699333+dependabot[bot]@users.noreply.github.com'

export function classifyRound({ authors, ourEmail, dependabotEmail = DEPENDABOT_EMAIL }) {
  // `ourEmail` is a required input rather than a constant here on purpose: the
  // workflow declares that literal at job level because its push step needs
  // the same one, and a second copy in this file is how the two drift apart.
  if (!ourEmail) {
    throw new Error('classifyRound needs ourEmail: the identity our own fix commits are authored under')
  }
  if (!Array.isArray(authors)) {
    throw new Error('classifyRound needs authors: every commit author on the pull request, as an array')
  }
  // No commits is not a first round. It means the listing failed or returned
  // nothing, and inventing a state from an absence is the fail-open direction.
  if (authors.length === 0) {
    return {
      round: 'hand',
      note: 'No commit authors could be read for this pull request, so there is nothing to decide automatically.',
    }
  }

  if (authors.includes(ourEmail)) {
    return {
      round: '2',
      note: 'Round 2: one of the commits here is ours, so a fix has already been attempted.',
    }
  }

  const strangers = [...new Set(authors.filter((author) => author !== dependabotEmail))]
  if (strangers.length === 0) {
    return { round: '1', note: null }
  }

  return {
    round: 'hand',
    note:
      `Commits here are authored by ${strangers.map((s) => `\`${s}\``).join(', ')}, which is neither ` +
      "Dependabot's address nor ours. Someone pushed to this branch by hand, so there is nothing here " +
      'to decide automatically.',
  }
}

// Entry point. Reads BOT_EMAIL and a whitespace-separated AUTHORS from the
// environment, writes `round=` to GITHUB_OUTPUT, and prints its note for the
// caller to tee into the step summary. A round with nothing to say prints
// nothing.
if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const { round, note } = classifyRound({
    // Split on any whitespace, and the caller passes one line. A multi-line
    // step output would need a delimiter chosen so the values cannot close it,
    // and an email address cannot contain a space -- so one line is both
    // simpler and the shape with no delimiter to guess.
    //
    // An address that somehow did contain whitespace splits into two entries,
    // neither of which matches anything, and the verdict is `hand`. That is
    // the fail-closed direction, which is why it is left to happen rather
    // than guarded against.
    authors: (process.env.AUTHORS ?? '').split(/\s+/).filter(Boolean),
    ourEmail: process.env.BOT_EMAIL,
  })

  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `round=${round}\n`)
  }
  if (note) {
    console.log(note)
  }
}
