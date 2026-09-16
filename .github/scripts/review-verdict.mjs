// **What the code review actually did, decided from evidence rather than from
// the step's exit code.**
//
// `claude-code-action` reports `outcome=success` whenever the SDK returns at
// all, so a review that examined nothing and a review that found nothing look
// identical from the outside -- and the `code-review` plugin says nothing on a
// clean pull request by design ("if there are no issues that meet this
// criteria, do not proceed"). The two silences were indistinguishable, which
// is the whole reason this file exists.
//
// Measured on three runs of this repository's own workflow, all green:
//
//   PR #35, attempt 1:  3 turns,  12.7s, $0.10,  0 denials, nothing posted
//   PR #35, attempt 2: 24 turns,  95s,   $0.55, 11 denials, nothing posted
//   PR #22:            54 turns, 319s,   $2.07, 12 denials, comment posted
//
// The first two are failures. Only the third did the job. Nothing in the
// GitHub UI distinguished them.
//
// The input is `execution_file` from the action: a JSON array of Claude Code
// SDK messages, `JSON.stringify(messages, null, 2)` (see
// base-action/src/execution-file.ts upstream). The last entry is normally the
// `result` record carrying the run's totals.

import { readFileSync, appendFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

/**
 * The turn count is deliberately NOT a criterion.
 *
 * It is the most tempting one -- 3 turns versus 54 separates the measured
 * failures from the measured success cleanly -- and it is the one that would
 * fire wrongly. The plugin's first step legitimately stops early when the pull
 * request is closed, is a draft, or already carries its review, and a threshold
 * cannot tell that apart from an abandoned run. A guard that cries wolf on a
 * legitimate skip gets switched off, and then it guards nothing.
 *
 * Every rule below is instead something that cannot be true of a review that
 * did its job.
 */
export function decide(messages, postedComments) {
  // Fail closed on a shape this does not recognise. An unreadable log is not
  // evidence of success, and treating it as one would restore exactly the
  // silence this file was written to end.
  if (!Array.isArray(messages) || messages.length === 0) {
    return failed('the execution log is missing, empty, or not the array of SDK messages this reads')
  }

  const result = messages.find((m) => m && typeof m === 'object' && m.type === 'result')
  if (!result) {
    return failed('the run produced no `result` record, so it did not finish')
  }

  const stats = {
    turns: numberOr(result.num_turns, 0),
    costUsd: numberOr(result.total_cost_usd, 0),
    denials: numberOr(result.permission_denials_count, 0),
    toolCalls: countToolUses(messages),
    subagents: countToolUses(messages, 'Task'),
    postedComments,
  }

  if (result.is_error === true || result.subtype !== 'success') {
    return failed(`the SDK reported \`${result.subtype ?? 'no subtype'}\``, stats)
  }

  // **A denial means a review that was prevented, not a review that was
  // clean.** Both measured failures above were drowning in them: the workflow
  // passed `--allowedTools` naming a single MCP tool, so every `gh pr diff`,
  // every `git blame` and every subagent the plugin wanted was refused. With
  // `--permission-mode bypassPermissions` this should now be zero, and any
  // denial reappearing here is a real regression rather than noise to absorb.
  if (stats.denials > 0) {
    return failed(`${stats.denials} tool call(s) were denied, so the review was prevented from reading the change`, stats)
  }

  // **The one coupling to the plugin, and it is deliberately loose.** The
  // plugin's procedure is subagents from its very first step onwards -- an
  // eligibility check, a CLAUDE.md lookup, a summary, then five reviewers in
  // parallel. Zero subagents means the procedure never started, which is
  // precisely attempt 1 above. A legitimate early stop still launches the
  // eligibility agent, so it lands above this line, not below it.
  if (stats.subagents === 0) {
    return failed('no review subagent was launched, so the review procedure never started', stats)
  }

  return {
    verdict: postedComments > 0 ? 'findings' : 'clean',
    reason:
      postedComments > 0
        ? `the review ran and left ${postedComments} comment(s) on the pull request`
        : 'the review ran to completion and raised nothing',
    stats,
  }
}

function failed(reason, stats = null) {
  return { verdict: 'failed', reason, stats }
}

function numberOr(value, fallback) {
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback
}

// Count `tool_use` blocks across assistant messages, optionally for one tool.
// Written defensively: a message whose `content` is a bare string rather than
// an array is valid SDK output and must not throw here, because a crash in
// this file would redden the job for a reason unrelated to the review.
function countToolUses(messages, name = null) {
  let count = 0
  for (const message of messages) {
    if (!message || typeof message !== 'object' || message.type !== 'assistant') { continue }
    const content = message.message?.content
    if (!Array.isArray(content)) { continue }
    for (const block of content) {
      if (!block || typeof block !== 'object' || block.type !== 'tool_use') { continue }
      if (name === null || block.name === name) { count += 1 }
    }
  }
  return count
}

/** The Markdown the workflow posts on the pull request, one per verdict. */
export function comment(decision) {
  const lines = ['<!-- claude-code-review-verdict -->', '## Code review', '']
  if (decision.verdict === 'clean') {
    lines.push('Reviewed this change and found nothing to raise.')
  } else if (decision.verdict === 'findings') {
    lines.push(`Reviewed this change and left ${decision.stats.postedComments} comment(s) above.`)
  } else {
    lines.push('**This review did not run to completion, so this pull request has not been reviewed.**')
    lines.push('')
    lines.push(`Reason: ${decision.reason}`)
  }
  if (decision.stats) {
    lines.push('')
    lines.push(
      `<sub>${decision.stats.turns} turns, ${decision.stats.subagents} subagents, `
        + `$${decision.stats.costUsd.toFixed(2)}.</sub>`,
    )
  }
  return lines.join('\n')
}

// CLI: `node review-verdict.mjs <execution-file> <posted-comment-count>`.
// Prints the comment body on stdout, writes `verdict` and `reason` to
// GITHUB_OUTPUT, and exits 1 only on `failed` -- which is what reddens the job.
if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const [file, countArg] = process.argv.slice(2)

  let messages = null
  try {
    messages = JSON.parse(readFileSync(file, 'utf8'))
  } catch {
    // Left null on purpose: `decide` owns the fail-closed message, so a
    // missing file and a corrupt one take the same path and read the same way.
  }

  // `Number('')` is 0, not NaN -- the trap this repository already recorded
  // once in its version guard. An absent count must not silently become "no
  // comments", so an unparseable argument is a hard error here.
  const count = Number.parseInt(countArg, 10)
  if (!Number.isFinite(count) || count < 0) {
    console.error(`review-verdict: expected a comment count, got ${JSON.stringify(countArg)}`)
    process.exit(2)
  }

  const decision = decide(messages, count)
  console.log(comment(decision))
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `verdict=${decision.verdict}\n`)
    appendFileSync(process.env.GITHUB_OUTPUT, `reason=${decision.reason}\n`)
  }
  process.exit(decision.verdict === 'failed' ? 1 : 0)
}
