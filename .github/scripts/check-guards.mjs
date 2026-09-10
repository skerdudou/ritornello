import { appendFileSync, readFileSync, writeFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

// The mechanical check on what the analysis wrote, before anything is pushed.
//
// The dispositif lets one agent write a fix and judge whether it is good; CI
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

// Ways to make a test stop asserting without deleting it. `.skip`, `.only`
// and `.todo` cover vitest; `#[ignore]` covers Rust. `.only` is here because
// it silences every sibling in the file, which is a larger loss than a skip.
const SILENCERS = [/\.skip\s*\(/, /\.only\s*\(/, /\.todo\s*\(/, /#\[ignore\]/]

const HAS_INSTALL_SCRIPT = /"hasInstallScript"\s*:\s*true/

const countMarkers = (lines) =>
  lines.reduce((total, { text }) => total + TEST_MARKERS.reduce((n, re) => n + (text.match(re) ?? []).length, 0), 0)

// Parse a unified diff into the added and removed lines, each tagged with the
// file it belongs to. `+++ b/path` is the authority on the path: a rename
// shows the destination, which is the file the change lands in.
function parse(diffText) {
  const added = []
  const removed = []
  let path = null

  for (const raw of String(diffText ?? '').split('\n')) {
    const line = raw.replace(/\r$/, '')

    if (line.startsWith('+++ ')) {
      // Not `.trim()`ed: the carriage return is already gone, stripped once
      // for the whole line above. Two mechanisms for one problem means the
      // tested one can be deleted without anything turning red -- which is
      // exactly what happened before this comment existed.
      const target = line.slice(4).replace(/^b\//, '')
      path = target === '/dev/null' ? path : target
      continue
    }
    if (line.startsWith('--- ') || line.startsWith('diff --git') || line.startsWith('@@') || line.startsWith('index ')) continue

    if (line.startsWith('+')) added.push({ path, text: line.slice(1) })
    else if (line.startsWith('-')) removed.push({ path, text: line.slice(1) })
  }

  return { added, removed }
}

export function checkGuards(diffText) {
  const { added, removed } = parse(diffText)
  const failures = []

  // Guard 1 -- nothing under `.github`. Never necessary for a bump, and it is
  // where the controls themselves live. It also keeps policy with the owner:
  // capping a dependency in `dependabot.yml` is a decision, not an
  // adaptation.
  const touched = [...new Set([...added, ...removed].map(({ path }) => path).filter(Boolean))]
  for (const path of touched.filter((p) => p.startsWith('.github/'))) {
    failures.push({ guard: 1, detail: `the fix touches ${path}, which is out of bounds for a dependency bump` })
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
