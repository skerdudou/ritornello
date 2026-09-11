import { appendFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

// Decide which round of the analysis a pull request head belongs to, from the
// identity that authored its head commit.
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

// Dependabot's own commit identity. Measured, not assumed: every commit on
// #18 (npm_and_yarn) and #24 (cargo) carries this exact address, so it is the
// bot's account identity and not an ecosystem-specific one.
export const DEPENDABOT_EMAIL = '49699333+dependabot[bot]@users.noreply.github.com'

export function classifyRound({ author, ourEmail, dependabotEmail = DEPENDABOT_EMAIL }) {
  // `ourEmail` is a required input rather than a constant here on purpose: the
  // workflow declares that literal at job level because its push step needs
  // the same one, and a second copy in this file is how the two drift apart.
  if (!ourEmail) {
    throw new Error('classifyRound needs ourEmail: the identity our own fix commits are authored under')
  }

  if (author === ourEmail) {
    return {
      round: '2',
      note: 'Round 2: the head commit is ours, so a fix has already been attempted.',
    }
  }

  if (author === dependabotEmail) {
    return { round: '1', note: null }
  }

  return {
    round: 'hand',
    note:
      `The head commit is authored by \`${author}\`, which is neither Dependabot's address nor ours. ` +
      'Someone pushed to this branch by hand, so there is nothing here to decide automatically.',
  }
}

// Entry point. Reads AUTHOR and BOT_EMAIL from the environment, writes
// `round=` to GITHUB_OUTPUT, and prints its note for the caller to tee into
// the step summary. A round with nothing to say prints nothing.
if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const { round, note } = classifyRound({
    author: process.env.AUTHOR ?? '',
    ourEmail: process.env.BOT_EMAIL,
  })

  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `round=${round}\n`)
  }
  if (note) {
    console.log(note)
  }
}
