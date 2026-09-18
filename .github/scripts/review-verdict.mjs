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
export function decide(messages, postedComments, { workflowChanged = false } = {}) {
  const usable = Array.isArray(messages) && messages.length > 0

  // **The one refusal that is not a failure.** `claude-code-action` validates
  // server-side that the running workflow file is byte-identical to the
  // default branch's copy, and skips without invoking a model when it is not
  // -- so a pull request that edits this very workflow can never be reviewed
  // by it. Measured on PR #36, the one that introduced this file:
  // "Skipping action due to workflow validation".
  //
  // Reddening that would be the guard crying wolf on the one case where the
  // refusal is expected, documented and unavoidable, and a guard that cries
  // wolf gets switched off. It is still reported -- silence is what this file
  // exists to end -- but it does not claim a fault.
  //
  // It is deliberately narrow: only when there is no usable log at all. A
  // review that DID run is judged on what it did, whatever the workflow diff
  // says, so this cannot become a way to land an unreviewed change by
  // touching the file.
  //
  // Since the trigger became a comment the validated file is the default
  // branch's own copy, so this may no longer be reachable at all. The caller
  // now answers the question by listing the pull request's files rather than
  // by diffing a workspace that no longer holds it; the branch stays because
  // a guess about someone else's server is not worth a comment telling the
  // author their change went unreviewed when it did not.
  if (workflowChanged && !usable) {
    return {
      verdict: 'skipped',
      reason: 'this pull request changes the review workflow itself, which the action refuses to run; it will be reviewed once this lands on the default branch',
      stats: null,
    }
  }

  // Fail closed on a shape this does not recognise. An unreadable log is not
  // evidence of success, and treating it as one would restore exactly the
  // silence this file was written to end.
  if (!usable) {
    return failed('the execution log is missing, empty, or not the array of SDK messages this reads')
  }

  const result = messages.find((m) => m && typeof m === 'object' && m.type === 'result')
  if (!result) {
    return failed('the run produced no `result` record, so it did not finish')
  }

  // `byTool` is reported and never judged. It is what turns "the review said
  // nothing" into an answer -- whether it read the diff, whether it tried to
  // comment -- without exposing a line of the model's output, which is why
  // the action hides that output in the first place.
  const byTool = toolHistogram(messages)
  const stats = {
    turns: numberOr(result.num_turns, 0),
    costUsd: numberOr(result.total_cost_usd, 0),
    denials: numberOr(result.permission_denials_count, 0),
    toolCalls: Object.values(byTool).reduce((a, b) => a + b, 0),
    byTool,
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

  // **Nothing was examined.** An assistant that called no tool at all read no
  // diff, no file and no history, whatever it then said -- that is true of
  // any reviewer, under any plugin, and it is what attempt 1 looked like.
  //
  // This rule replaces a count of `Task` tool calls, which was wrong twice
  // over and measured wrong on the first real run after it shipped: the
  // subagent tool is named `Agent` in current versions, not `Task`, and a
  // review that legitimately works without subagents at all is not a failed
  // one. That run spent $1.60 over 9 turns across three models -- Sonnet,
  // Haiku and Opus, so subagents plainly ran -- and was reported as never
  // having started.
  //
  // The lesson is in the shape of the rule, not just its threshold: a guard
  // keyed on the NAME of someone else's tool is keyed on something that
  // changes without telling us. This one is keyed on the existence of any
  // tool call at all. The per-tool breakdown below is reported, never judged.
  if (stats.toolCalls === 0) {
    return failed('the review called no tool at all, so it examined nothing', stats)
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

// Count `tool_use` blocks per tool name across assistant messages.
//
// Written defensively: a message whose `content` is a bare string rather than
// an array is valid SDK output and must not throw here, because a crash in
// this file would redden the job for a reason unrelated to the review. A
// nameless block is counted under `(unnamed)` rather than dropped -- a tool
// call this cannot identify is still a tool call, and silently discarding it
// is how `toolCalls === 0` would become wrong.
function toolHistogram(messages) {
  const counts = {}
  for (const message of messages) {
    if (!message || typeof message !== 'object' || message.type !== 'assistant') { continue }
    const content = message.message?.content
    if (!Array.isArray(content)) { continue }
    for (const block of content) {
      if (!block || typeof block !== 'object' || block.type !== 'tool_use') { continue }
      const name = typeof block.name === 'string' && block.name ? block.name : '(unnamed)'
      counts[name] = (counts[name] ?? 0) + 1
    }
  }
  return counts
}

/** The Markdown the workflow posts on the pull request, one per verdict. */
export function comment(decision) {
  const lines = ['<!-- claude-code-review-verdict -->', '## Code review', '']
  if (decision.verdict === 'clean') {
    lines.push('Reviewed this change and found nothing to raise.')
  } else if (decision.verdict === 'findings') {
    lines.push(`Reviewed this change and left ${decision.stats.postedComments} comment(s) above.`)
  } else if (decision.verdict === 'skipped') {
    lines.push('Not reviewed: this pull request changes the review workflow itself.')
    lines.push('')
    lines.push('The action refuses to run a workflow file that differs from the default branch, so no review is possible here. It will run again on the next pull request once this lands.')
  } else {
    lines.push('**This review did not run to completion, so this pull request has not been reviewed.**')
    lines.push('')
    lines.push(`Reason: ${decision.reason}`)
  }
  if (decision.stats) {
    lines.push('')
    lines.push(
      `<sub>${decision.stats.turns} turns, ${decision.stats.toolCalls} tool calls, `
        + `$${decision.stats.costUsd.toFixed(2)}.</sub>`,
    )
  }
  return lines.join('\n')
}

/**
 * The per-tool breakdown, for the job summary rather than the pull request.
 *
 * It answers the question the hidden model output otherwise leaves open --
 * did the review read the diff, did it try to comment -- and it is what would
 * have caught the `Task`/`Agent` naming mistake before it shipped instead of
 * on the first real run. Sorted by count so the shape is readable at a
 * glance, and it names no file and quotes no output.
 */
export function toolBreakdown(decision) {
  if (!decision.stats) { return 'No execution log, so no tool calls to report.' }
  const entries = Object.entries(decision.stats.byTool).sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
  if (entries.length === 0) { return 'The review called no tool at all.' }
  return ['| Tool | Calls |', '| --- | --- |', ...entries.map(([name, n]) => `| \`${name}\` | ${n} |`)].join('\n')
}

// CLI: `node review-verdict.mjs <execution-file> <posted-comment-count>`.
// Prints the comment body on stdout, writes `verdict` and `reason` to
// GITHUB_OUTPUT, and exits 1 only on `failed` -- which is what reddens the job.
if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const [file, countArg, workflowChangedArg] = process.argv.slice(2)

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

  // Only the literal `true` enables the skip. Anything else -- an empty
  // argument, a typo, a shell that expanded nothing -- must leave the strict
  // path in force, because this flag is the one input that can turn a red
  // verdict green.
  const decision = decide(messages, count, { workflowChanged: workflowChangedArg === 'true' })
  console.log(comment(decision))
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `verdict=${decision.verdict}\n`)
    appendFileSync(process.env.GITHUB_OUTPUT, `reason=${decision.reason}\n`)
  }
  // The breakdown goes to the run summary and not to the pull request: it is
  // for whoever is asking why a verdict reads the way it does, which is not
  // every reader of every pull request. Written here rather than in the YAML
  // so that what it contains is covered by this file's tests.
  if (process.env.GITHUB_STEP_SUMMARY) {
    appendFileSync(
      process.env.GITHUB_STEP_SUMMARY,
      `${comment(decision)}\n\n<details><summary>Tool calls</summary>\n\n${toolBreakdown(decision)}\n</details>\n`,
    )
  }
  process.exit(decision.verdict === 'failed' ? 1 : 0)
}
