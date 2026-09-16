import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { writeFileSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { decide, comment, toolBreakdown } from './review-verdict.mjs'

const SCRIPT = fileURLToPath(new URL('./review-verdict.mjs', import.meta.url))

// Every assertion below is written against ONE mutation of a log that is
// otherwise healthy, so a rule that stops applying is a failure here rather
// than a quieter green. The healthy log is asserted first, or the whole file
// could pass by rejecting everything.

/**
 * A log shaped like the action's `execution_file` for a review that worked.
 *
 * `tools` names the calls it made. The default deliberately mixes an `Agent`
 * and a `Bash`: the rule under test must not care which, and the previous
 * version of it counted only `Task` and so reported a real, working review as
 * one that never started.
 */
function healthyLog({
  turns = 54,
  denials = 0,
  tools = ['Agent', 'Bash', 'Read'],
  subtype = 'success',
  isError = false,
} = {}) {
  const messages = [{ type: 'system', subtype: 'init', message: 'Claude Code initialized' }]

  for (const name of tools) {
    messages.push({
      type: 'assistant',
      message: { content: [{ type: 'tool_use', name, input: { prompt: 'review something' } }] },
    })
    messages.push({ type: 'user', message: { content: [{ type: 'tool_result', content: 'done' }] } })
  }

  messages.push({
    type: 'result',
    subtype,
    is_error: isError,
    num_turns: turns,
    total_cost_usd: 2.0748557,
    permission_denials_count: denials,
  })
  return messages
}

test('the healthy log is healthy, or nothing below is discriminating', () => {
  const d = decide(healthyLog(), 0)
  assert.equal(d.verdict, 'clean', d.reason)
  assert.equal(d.stats.toolCalls, 3)
  assert.deepEqual(d.stats.byTool, { Agent: 1, Bash: 1, Read: 1 })
  assert.equal(d.stats.turns, 54)
  assert.equal(d.stats.denials, 0)
})

test('a completed review that raised nothing is clean, not failed', () => {
  // The case the whole file exists for: silence must be reported, not
  // mistaken for a crash and not mistaken for success without evidence.
  assert.equal(decide(healthyLog(), 0).verdict, 'clean')
})

test('a completed review that commented reports findings', () => {
  const d = decide(healthyLog(), 4)
  assert.equal(d.verdict, 'findings')
  assert.equal(d.stats.postedComments, 4)
})

// --- Fail-closed on a log this cannot read ----------------------------------

test('a missing log fails rather than passing as clean', () => {
  // `decide(null, ...)` is what the CLI hands over when the file is absent or
  // corrupt. Reading that as "no issues found" is the exact silence this
  // replaces, so it must be the loudest branch in the file.
  assert.equal(decide(null, 0).verdict, 'failed')
  assert.equal(decide(undefined, 0).verdict, 'failed')
  assert.equal(decide([], 0).verdict, 'failed')
  assert.equal(decide('not an array', 0).verdict, 'failed')
  assert.equal(decide({ type: 'result' }, 0).verdict, 'failed', 'an object is not the array of messages')
})

test('an empty log is reported as an unreadable log, not as a truncated one', () => {
  // The verdict alone does not pin this: with the length test removed, `[]`
  // still fails, one branch further down, as "no result record". Measured by
  // mutation -- that edit survived every other assertion in this file. The
  // reason is what someone reads on the pull request, and "the log is empty"
  // and "the run was killed mid-flight" send them to different places.
  assert.match(decide([], 0).reason, /missing, empty/)
})

test('a log with no result record fails', () => {
  // A run killed mid-flight writes its messages and never its totals.
  const truncated = healthyLog().filter((m) => m.type !== 'result')
  const d = decide(truncated, 0)
  assert.equal(d.verdict, 'failed')
  assert.match(d.reason, /no `result` record/)
})

// --- One mutation per operand of the error predicate ------------------------
//
// `is_error === true || subtype !== 'success'` has two operands, and pinning
// only one leaves the other free to rot.

test('is_error fails even when the subtype still says success', () => {
  const d = decide(healthyLog({ isError: true }), 0)
  assert.equal(d.verdict, 'failed')
})

test('a non-success subtype fails even when is_error is false', () => {
  // This is the turn-limit shape: `error_max_turns`, `is_error: false`.
  const d = decide(healthyLog({ subtype: 'error_max_turns' }), 0)
  assert.equal(d.verdict, 'failed')
  assert.match(d.reason, /error_max_turns/)
})

// --- The two measured failure modes -----------------------------------------

test('a denied tool call fails: the review was prevented, not clean', () => {
  // PR #35 attempt 2 and PR #22: 11 and 12 denials against a single-tool
  // allow-list. With `bypassPermissions` this should be zero for ever, so one
  // reappearing is a regression and not noise.
  const d = decide(healthyLog({ denials: 11 }), 0)
  assert.equal(d.verdict, 'failed')
  assert.match(d.reason, /11 tool call/)
})

test('one denial is enough; the threshold is not "a few are fine"', () => {
  assert.equal(decide(healthyLog({ denials: 1 }), 0).verdict, 'failed')
})

test('a run that called no tool at all fails', () => {
  // PR #35 attempt 1: 3 turns, no denials, nothing posted, green. Whatever it
  // said, it read no diff and no file, so it reviewed nothing.
  const d = decide(healthyLog({ tools: [], turns: 3 }), 0)
  assert.equal(d.verdict, 'failed')
  assert.match(d.reason, /examined nothing/)
})

test('a single tool call is enough, whatever the tool is called', () => {
  // **The regression this rule is a repair of.** The previous version counted
  // calls named `Task` and failed anything else. Measured on the first real
  // run after it shipped -- 9 turns, $1.60, three models, zero denials, and a
  // verdict of "the review procedure never started".
  //
  // The subagent tool is named `Agent` in current versions, and a review that
  // works without subagents at all is not a failed one. So each of these, on
  // its own, is a review that examined something:
  for (const tool of ['Agent', 'Task', 'Bash', 'Read', 'Grep', 'mcp__github_inline_comment__create_inline_comment']) {
    const d = decide(healthyLog({ tools: [tool], turns: 4 }), 0)
    assert.equal(d.verdict, 'clean', `a lone \`${tool}\` call should not read as a failure: ${d.reason}`)
  }
})

test('a tool call with no usable name still counts as a tool call', () => {
  // Dropping it would make `toolCalls === 0` wrong on a log this cannot fully
  // parse, which is the one number the rule above is keyed on.
  const log = healthyLog({ tools: [] })
  log.splice(1, 0, { type: 'assistant', message: { content: [{ type: 'tool_use' }] } })
  const d = decide(log, 0)
  assert.equal(d.verdict, 'clean')
  assert.equal(d.stats.toolCalls, 1)
  assert.deepEqual(d.stats.byTool, { '(unnamed)': 1 })
})

test('the breakdown counts each tool separately and is sorted by use', () => {
  const d = decide(healthyLog({ tools: ['Bash', 'Read', 'Bash', 'Agent', 'Bash', 'Read'] }), 0)
  assert.deepEqual(d.stats.byTool, { Bash: 3, Read: 2, Agent: 1 })
  assert.equal(d.stats.toolCalls, 6)
  const table = toolBreakdown(d)
  assert.match(table, /\| `Bash` \| 3 \|/)
  assert.ok(table.indexOf('`Bash`') < table.indexOf('`Read`'), 'sorted by count, most used first')
  assert.ok(table.indexOf('`Read`') < table.indexOf('`Agent`'))
})

test('the breakdown says so plainly when there is nothing to break down', () => {
  assert.match(toolBreakdown(decide(null, 0)), /No execution log/)
  assert.match(toolBreakdown(decide(healthyLog({ tools: [] }), 0)), /called no tool at all/)
})

// --- Shapes that must not throw ---------------------------------------------

test('a string content block does not crash the counter', () => {
  // Valid SDK output. A crash here would redden the job for a reason that has
  // nothing to do with the review, which is a worse failure than the one this
  // file prevents.
  const log = healthyLog()
  log.splice(1, 0, { type: 'assistant', message: { content: 'plain text reply' } })
  log.splice(1, 0, { type: 'assistant', message: {} })
  log.splice(1, 0, null)
  assert.equal(decide(log, 0).verdict, 'clean')
})

test('missing numeric totals read as zero rather than NaN', () => {
  const log = healthyLog()
  const result = log.find((m) => m.type === 'result')
  delete result.num_turns
  delete result.total_cost_usd
  delete result.permission_denials_count
  const d = decide(log, 0)
  assert.equal(d.stats.turns, 0)
  assert.equal(d.stats.costUsd, 0)
  assert.equal(d.verdict, 'clean', 'absent totals are not themselves a failure')
})

// --- The one refusal that is not a failure ----------------------------------

test('a pull request that edits the review workflow is skipped, not failed', () => {
  // The action validates the workflow file against the default branch and
  // skips without invoking a model, so there is no log at all. Measured on
  // PR #36.
  const d = decide(null, 0, { workflowChanged: true })
  assert.equal(d.verdict, 'skipped')
  assert.match(d.reason, /changes the review workflow itself/)
})

test('the same pull request without the flag still fails', () => {
  // The pair that proves the flag is what does the work, and not the empty
  // log reading as benign on its own.
  assert.equal(decide(null, 0).verdict, 'failed')
  assert.equal(decide(null, 0, { workflowChanged: false }).verdict, 'failed')
})

test('the skip cannot launder a review that ran badly', () => {
  // **The escape hatch this must not become.** If touching the workflow file
  // excused any verdict, an unreviewed change would land green by editing one
  // comment in the YAML. The skip is narrow on purpose: it applies only when
  // there is no usable log, so a run that DID produce one is judged on what
  // it did whatever the workflow diff says.
  const changed = { workflowChanged: true }
  assert.equal(decide(healthyLog({ denials: 4 }), 0, changed).verdict, 'failed')
  assert.equal(decide(healthyLog({ tools: [] }), 0, changed).verdict, 'failed')
  assert.equal(decide(healthyLog({ isError: true }), 0, changed).verdict, 'failed')
  assert.equal(decide(healthyLog().filter((m) => m.type !== 'result'), 0, changed).verdict, 'failed')
})

test('a skip is not a review, and says so rather than claiming a clean bill', () => {
  const body = comment(decide(null, 0, { workflowChanged: true }))
  assert.match(body, /Not reviewed/)
  assert.doesNotMatch(body, /found nothing to raise/)
  assert.doesNotMatch(body, /undefined/)
})

// --- The command line, because the exit code is what reddens the job --------
//
// `decide` being right is not enough: the workflow reads an exit code and a
// file, and the argument that turns a red verdict green is a shell variable
// that can arrive empty.

/** Run the script as the workflow does. Returns `{ status, stdout }`. */
function runCli(log, count, workflowChanged) {
  const dir = mkdtempSync(join(tmpdir(), 'review-verdict-'))
  let file = ''
  if (log !== null) {
    file = join(dir, 'execution.json')
    writeFileSync(file, JSON.stringify(log))
  }
  const args = [SCRIPT, file, String(count)]
  if (workflowChanged !== undefined) args.push(workflowChanged)
  try {
    return { status: 0, stdout: execFileSync('node', args, { encoding: 'utf8' }) }
  } catch (error) {
    return { status: error.status, stdout: error.stdout ?? '' }
  }
}

test('a clean review exits 0 and a broken one exits 1', () => {
  assert.equal(runCli(healthyLog(), 0).status, 0)
  assert.equal(runCli(healthyLog({ tools: [] }), 0).status, 1)
  assert.equal(runCli(null, 0).status, 1, 'a missing execution file reddens the job')
})

test('only the literal "true" enables the workflow-change skip', () => {
  // The flag reaches the script as `"${WORKFLOW_CHANGED:-false}"`, and an
  // unset step output, a typo or a shell that expanded nothing must all leave
  // the strict path in force. A loose test here -- `!== 'false'`, or a
  // truthiness check -- would turn every one of those into a green job on an
  // unreviewed pull request.
  assert.equal(runCli(null, 0, 'true').status, 0, 'the real skip is green')
  for (const value of ['false', '', 'TRUE', 'True', '1', 'yes', 'null', undefined]) {
    assert.equal(runCli(null, 0, value).status, 1, `\`${value}\` must not enable the skip`)
  }
})

test('the command prints the comment body it is asked for', () => {
  assert.match(runCli(healthyLog(), 0).stdout, /Reviewed this change and found nothing to raise/)
  assert.match(runCli(null, 0, 'true').stdout, /Not reviewed/)
})

test('a missing comment count is a usage error, not a silent zero', () => {
  // `Number('')` is 0, a trap this repository has recorded before. An absent
  // count reading as "no comments" would report a clean review it never
  // measured.
  const dir = mkdtempSync(join(tmpdir(), 'review-verdict-'))
  const file = join(dir, 'execution.json')
  writeFileSync(file, JSON.stringify(healthyLog()))
  let status = 0
  try {
    execFileSync('node', [SCRIPT, file, ''], { encoding: 'utf8', stdio: 'pipe' })
  } catch (error) {
    status = error.status
  }
  assert.equal(status, 2)
})

// --- The comment ------------------------------------------------------------

test('every comment carries the marker that makes it updatable', () => {
  // Without it the workflow appends a new comment on every push instead of
  // editing one.
  for (const count of [0, 3]) {
    assert.match(comment(decide(healthyLog(), count)), /^<!-- claude-code-review-verdict -->/)
  }
  assert.match(comment(decide(null, 0)), /^<!-- claude-code-review-verdict -->/)
})

test('the clean comment says the review happened, not merely that nothing was found', () => {
  // The requirement in one line: a reader must be able to tell a clean review
  // from a workflow that never ran.
  const body = comment(decide(healthyLog(), 0))
  assert.match(body, /Reviewed this change/)
  assert.match(body, /nothing to raise/)
  assert.doesNotMatch(body, /did not run/)
})

test('the failed comment says plainly that the pull request is unreviewed', () => {
  const body = comment(decide(healthyLog({ tools: [] }), 0))
  assert.match(body, /has not been reviewed/)
  assert.match(body, /examined nothing/)
})

test('the findings comment points at the comments it left', () => {
  assert.match(comment(decide(healthyLog(), 2)), /left 2 comment/)
})

test('a failure with no stats still renders', () => {
  // `decide(null, ...)` returns `stats: null`; a comment builder that assumed
  // otherwise would throw on the one path that matters most.
  const body = comment(decide(null, 0))
  assert.match(body, /has not been reviewed/)
  assert.doesNotMatch(body, /undefined/)
})
