import { createHash } from 'node:crypto'
import { appendFileSync, readFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

// **What stops a refusal from being paid for, and posted, once per rebase.**
//
// A `read-it` verdict leaves no commit of ours on the pull request, so it stays
// in round 1, and every later CI completion convoked a fresh analysis: each
// time Dependabot rebuilt its branch because `main` moved, and each re-run.
// Measured on #44 (`@vueuse/core` 14 -> 15): three analyses in a week, three
// long comments saying the same thing, and the only things that had changed
// were the lockfile entries of *other* packages, merged on `main` meanwhile.
//
// What the analysis judges is the bump -- which dependencies, to which
// versions -- against whether CI passed. A rebase changes neither, so those
// two make the key, and a refusal already explained under the same key is not
// explained again. A green CI after a red one is a new question and gets a new
// analysis; so does a new arrival version, since Dependabot rewrites the
// summary. Anything else the owner wants reconsidered -- a guard fixed on
// `main`, a dependency of ours that moved -- is one manual dispatch away, and
// the dispatch path never consults the memo.
//
// Only `read-it` is remembered. A `merge` verdict whose head moved before the
// merge was queued must be judged again on the new head, or it would never
// merge at all.

const CONCLUSIONS = new Set(['success', 'failure'])

/**
 * The identity of the question the analysis answered.
 *
 * Refuses an empty summary rather than hashing it: every unreadable bump would
 * otherwise share one key, and the first refusal would silence all the others.
 */
export function memoKey({ summary, conclusion }) {
  if (typeof summary !== 'string' || summary.trim() === '') {
    throw new Error('memoKey: the bump summary is empty; there is no question to remember')
  }
  if (!CONCLUSIONS.has(conclusion)) {
    throw new Error(`memoKey: CI conclusion \`${conclusion}\` is not a verdict`)
  }
  return createHash('sha256').update(`${summary.trim()}\n${conclusion}`).digest('hex').slice(0, 16)
}

/** An HTML comment, so it is invisible in the rendered body. */
export const marker = (key) => `<!-- dependabot-analysis: read-it ${key} -->`

// The comment API refuses a body above 65536 characters; the margin covers
// the header and the closing tags, which are not counted below.
const BODY_BUDGET = 60000

/**
 * The pull request comment: the verdict and its reason up front, where the
 * owner reads them, and the full report folded underneath for whoever needs
 * it. Three unfolded copies of a two-page report was the other half of the
 * #44 complaint.
 */
export function verdictComment({ decision, key, conclusion, why, report }) {
  const head = []
  if (decision === 'read-it') head.push(marker(key))
  const reason = String(why ?? '').trim() || 'The analysis gave no short reason; the report below has its reasoning.'
  head.push(`**Dependabot analysis: \`${decision}\`.** ${reason}`)
  if (decision === 'read-it') {
    head.push(
      '',
      `This will not be analysed again while the same versions meet a CI that ${conclusion === 'success' ? 'passes' : 'fails'}: ` +
        'a rebase by Dependabot changes neither. Running the *Dependabot analysis* workflow by hand forces a new look.',
    )
  }
  const open = ['', '<details><summary>Full report</summary>', '', '']
  const close = ['', '', '</details>', '']

  const fixed = [...head, ...open, ...close].join('\n')
  let body = String(report ?? '')
  const room = BODY_BUDGET - Buffer.byteLength(fixed)
  if (Buffer.byteLength(body) > room) {
    const note = '\n\n*(truncated; the run summary carries the full report)*'
    // Cut on bytes, then drop a code point the cut may have split.
    body = Buffer.from(body).subarray(0, room - Buffer.byteLength(note)).toString('utf8').replace(/�+$/, '') + note
  }
  return [...head, ...open].join('\n') + body + close.join('\n')
}

// --- Command line -----------------------------------------------------------
//
// `node analysis-memo.mjs key` reads SUMMARY and CONCLUSION from the
// environment and writes `key=` to GITHUB_OUTPUT.
//
// `node analysis-memo.mjs comment <decision> <key> <conclusion> <why-file> <report-file>`
// prints the comment body. A missing why file reads as no reason given.
if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const [command, ...args] = process.argv.slice(2)
  const readOr = (path, fallback) => {
    try {
      return readFileSync(path, 'utf8')
    } catch {
      return fallback
    }
  }

  if (command === 'key') {
    const key = memoKey({ summary: process.env.SUMMARY, conclusion: process.env.CONCLUSION })
    if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `key=${key}\n`)
    console.log(key)
  } else if (command === 'comment') {
    const [decision, key, conclusion, whyPath, reportPath] = args
    if (decision === 'read-it' && !/^[0-9a-f]{16}$/.test(key ?? '')) {
      // A refusal posted without its key would be posted again next time,
      // which is the very thing this file exists to stop -- say so loudly.
      throw new Error(`comment: \`${key}\` is not a memo key`)
    }
    process.stdout.write(
      verdictComment({ decision, key, conclusion, why: readOr(whyPath, ''), report: readFileSync(reportPath, 'utf8') }),
    )
  } else {
    throw new Error(`analysis-memo: unknown command \`${command}\``)
  }
}
