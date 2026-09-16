import { appendFileSync, readFileSync, writeFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

// The mechanical check on what the analysis wrote, before anything is pushed.
//
// The mechanism lets one agent write a fix and judge whether it is good; CI
// is the only independent control left. These guards exist so that control
// cannot be disarmed by the thing it watches -- the shortest path to green
// must not be to delete what turns red.
//
// All three are pure functions of a unified diff: no GitHub, no network, no
// clock. That is what makes them testable, and a guard that is not tested is
// a comment.

// Test declarations, in both languages this repository writes.
//
// `\b` before `it` matters: without it the pattern fires inside `submit(`,
// and every ordinary edit to a form would read as a test being removed.
const TEST_MARKERS = [/\bit\s*\(/g, /\btest\s*\(/g, /#\[test\]/g]

// A line that is only a comment declares nothing. This repository mandates
// dense comments, so a comment carrying `it (` or `test(` is a realistic
// collision rather than an adversarial one -- and it errs both ways: an added
// one pads `after` and makes the guard more permissive, a removed one pads
// `before` and makes it refuse a diff that loses no coverage.
//
// `#` is deliberately absent from these prefixes: `#[test]` begins with it,
// and treating `#` as a comment marker would stop counting Rust tests
// altogether -- a far worse failure than the one being fixed.
const COMMENT_ONLY = /^\s*(\/\/|\/\*|\*|<!--)/

// Ways to make a test stop asserting without deleting it. `.skip`, `.only`
// and `.todo` cover vitest; `#[ignore]` covers Rust. `.only` is here because
// it silences every sibling in the file, which is a larger loss than a skip.
// `#[cfg_attr(target_os = "windows", ignore)]` is the idiomatic conditional
// ignore, and it is the worst case for guard 2: it silences a test without
// matching `#[ignore]` and without touching the `#[test]` line, so it passed
// both halves of the guard.
//
// Deliberately **not** filtered through `COMMENT_ONLY`. A silencer inside a
// comment counted as real produces a refusal, and a refusal is the safe
// direction; a marker miscounted the other way makes the gate permissive.
const SILENCERS = [/\.skip\s*\(/, /\.only\s*\(/, /\.todo\s*\(/, /#\[ignore\]/, /#\[cfg_attr\([^\]]*\bignore\b/]

const HAS_INSTALL_SCRIPT = /"hasInstallScript"\s*:\s*true/

const countMarkers = (lines) =>
  lines
    .filter(({ text }) => !COMMENT_ONLY.test(text))
    .reduce((total, { text }) => total + TEST_MARKERS.reduce((n, re) => n + (text.match(re) ?? []).length, 0), 0)

// Parse a unified diff into the added and removed lines, each tagged with the
// file it belongs to, plus **every path the diff names on either side**.
//
// Reading both sides is load-bearing, and the first version of this file did
// not. It tracked only `+++ b/path`, reasoning that a rename shows its
// destination and the destination is where the change lands. Review found two
// entirely ordinary diff shapes that walked through guard 1 as a result:
//
//   * a **deletion** emits `+++ /dev/null`, so the path stayed stale from the
//     previous file -- or `null` for the first file in the diff -- and got
//     dropped. Deleting `.github/workflows/ci.yml` was invisible, which is
//     precisely the "delete what turns red" move these guards exist to stop.
//   * a **rename out of `.github/`** names the old path only on the `---`
//     line, which was skipped. Moving this very file to `scripts/` and
//     weakening it in the same hunk passed cleanly.
//
// The lesson is in the shape of the fix rather than the fix: enumerating diff
// shapes is how the hole appeared, so `paths` now collects from every header
// that names a file -- `diff --git`, `rename from`/`rename to`,
// `copy from`/`copy to`, `---` and `+++` -- and guard 1 judges that union.
// Mode-only and binary changes, which emit no `---`/`+++` at all, come along
// for free with `diff --git`.
//
// That list is exhaustive as written, and it has to stay that way: a review
// caught an earlier version of this comment claiming "every header that names
// a file" while `copy from` was not among them. A comment that overstates a
// safety guard is worse than no comment.
//
// Line attribution still prefers the destination, since that is where content
// ends up, and falls back to the source for a deletion.
function parse(diffText) {
  const added = []
  const removed = []
  const paths = new Set()
  let inHunk = false
  let source = null
  let path = null

  const name = (value) => {
    if (!value || value === '/dev/null') return null
    paths.add(value)
    return value
  }

  for (const raw of String(diffText ?? '').split('\n')) {
    const line = raw.replace(/\r$/, '')

    if (line.startsWith('diff --git')) {
      inHunk = false
      source = null
      path = null
      // Both sides at once, but only when they are the same path: this is the
      // only header a mode-only or binary change emits. A rename makes the two
      // differ, and `rename from`/`rename to` below carry that case.
      const same = /^diff --git a\/(.+) b\/\1$/.exec(line)
      if (same) name(same[1])
      continue
    }
    if (line.startsWith('@@')) {
      inHunk = true
      continue
    }

    // Header lines only count before the first `@@` of a file. Without that,
    // an ordinary removed line whose content begins `-- ` arrives here as
    // `--- ` and would be read as a header, resetting the path.
    if (!inHunk) {
      if (line.startsWith('rename from ')) source = name(line.slice('rename from '.length))
      else if (line.startsWith('rename to ')) path = name(line.slice('rename to '.length))
      // Copies, for the same reason as renames and against the same
      // objection. `copy from`/`copy to` only appear when the diff was taken
      // with copy detection on, which is off by default and which the workflow
      // calling this does not enable -- so this branch is unreachable *given
      // how it is invoked today*. That sentence is precisely the kind of
      // reasoning that produced the hole this file was rewritten to close, and
      // two lines cost less than being right about it.
      else if (line.startsWith('copy from ')) source = name(line.slice('copy from '.length))
      else if (line.startsWith('copy to ')) path = name(line.slice('copy to '.length))
      else if (line.startsWith('--- ')) source = name(line.slice(4).replace(/^a\//, ''))
      else if (line.startsWith('+++ ')) {
        // Not `.trim()`ed: the carriage return is already gone, stripped once
        // for the whole line above. Two mechanisms for one problem means the
        // tested one can be deleted without anything turning red -- which is
        // exactly what happened before this comment existed.
        path = name(line.slice(4).replace(/^b\//, '')) ?? source
      }
      continue
    }

    if (line.startsWith('+')) added.push({ path, text: line.slice(1) })
    else if (line.startsWith('-')) removed.push({ path, text: line.slice(1) })
  }

  return { added, removed, paths: [...paths] }
}

export function checkGuards(diffText) {
  const { added, removed, paths } = parse(diffText)
  const failures = []

  // Guard 1 -- nothing under `.github`. Never necessary for a bump, and it is
  // where the controls themselves live. It also keeps policy with the owner:
  // capping a dependency in `dependabot.yml` is a decision, not an
  // adaptation.
  // Every path either side of the diff names, not only the ones content lines
  // could be attributed to: a deleted or renamed-away file is exactly the case
  // that has no attributable content under its own name.
  for (const touched of paths.filter((p) => p.startsWith('.github/'))) {
    failures.push({ guard: 1, detail: `the fix touches ${touched}, which is out of bounds for a dependency bump` })
  }

  // Guard 2 -- the suite does not get quieter. Two ways to lose coverage:
  // remove a declaration, or leave it in place and stop running it.
  const before = countMarkers(removed)
  const after = countMarkers(added)
  if (before > after) {
    failures.push({ guard: 2, detail: `the fix removes ${before - after} more test declaration(s) than it adds` })
  }
  for (const { path, text } of added) {
    const silencer = SILENCERS.find((re) => re.test(text))
    if (silencer) {
      failures.push({ guard: 2, detail: `the fix adds a silenced test in ${path}: ${text.trim()}` })
      break
    }
  }

  // Guard 3 -- no package gains an install script. Three declare one today
  // (`esbuild`, `fsevents`, and the `vue-demi` nested under
  // `@floating-ui/vue`), so on a surface that small any appearance is an
  // event. This is the delivery mechanism of the npm worms of the Shai-Hulud
  // kind, and it is the one guard whose failure asks for the owner by name.
  for (const { path, text } of added) {
    if (path?.endsWith('package-lock.json') && HAS_INSTALL_SCRIPT.test(text)) {
      failures.push({ guard: 3, detail: `a package gains an install script in ${path}: ${text.trim()}` })
      break
    }
  }

  return { ok: failures.length === 0, failures }
}

// --- Command line -----------------------------------------------------------
//
// `node check-guards.mjs <fix.diff>` — prints each failure, writes
// `guards.json` for the comment step to read, and writes `ok=` and
// `owner_alert=` to GITHUB_OUTPUT. `owner_alert` is guard 3 alone: it is the
// only failure that asks for a person by name.
if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const result = checkGuards(readFileSync(process.argv[2], 'utf8'))
  writeFileSync('guards.json', JSON.stringify(result))
  for (const failure of result.failures) console.log(`guard ${failure.guard} :: ${failure.detail}`)
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `ok=${result.ok}\n`)
    appendFileSync(process.env.GITHUB_OUTPUT, `owner_alert=${result.failures.some((f) => f.guard === 3)}\n`)
  }
}
