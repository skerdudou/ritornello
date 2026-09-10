import { appendFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

// The caret rule, and nothing else.
//
// `update-type` alone is not a safe gate, and this repository learned it the
// hard way: `tower 0.4.13 -> 0.5.3` and `evdev 0.12.2 -> 0.13.2` are reported
// as `semver-minor`, because in literal semver the major component is still 0
// -- and both merged themselves without anyone reading them. But a caret
// requirement pins the **leftmost non-zero** component, so `0.4 -> 0.5` is
// exactly as breaking as `1.x -> 2.x`, and `0.0.3 -> 0.0.4` is too. Cargo and
// npm agree on this; semver's own vocabulary is what disagrees.
//
// This is a module and not an inline heredoc for two reasons: both workflows
// need the same verdict, and a heredoc cannot be tested.

// Which component of a dotted version each update type moves.
const MOVED_INDEX = {
  'version-update:semver-major': 0,
  'version-update:semver-minor': 1,
  'version-update:semver-patch': 2,
}

// A readable version is a dotted numeric core, optionally carrying `+build`
// metadata. Two deliberate decisions live in this regexp:
//
//   * **Build metadata is accepted.** It describes the build, never
//     compatibility. `toml 1.1.4+spec-1.1.0 -> 1.1.5+spec-1.1.0` is a plain
//     patch; the earlier expression called it unreadable, and pull request
//     #23 waited for a human because of it.
//   * **A prerelease is not.** `6.0.0-beta` lies outside what a caret
//     requirement covers, so there is no rule here to apply to it, and
//     "unreadable" is the honest answer.
//
// The shape is checked before splitting because `Number('')` is 0, not NaN:
// `''.split('.').map(Number)` yields `[0]`, which reads as a perfectly
// ordinary version and would wave the bump through.
const READABLE = /^(\d+(?:\.\d+)*)(?:\+[0-9A-Za-z.-]+)?$/

export const parseVersion = (value) => {
  const match = READABLE.exec(String(value ?? ''))
  return match ? match[1].split('.').map(Number) : null
}

// The index of the leftmost non-zero component: 0 for `1.2.3`, 1 for
// `0.4.13`, 2 for `0.0.3`. That component is the one a caret refuses to
// cross. `-1` is `0.0.0`, which nothing depends on.
export const pinnedIndex = (parts) => parts.findIndex((n) => n > 0)

// Dependabot no longer publishes the version being moved *from* in the
// machine-readable block of its commit message, only the one being moved to.
// It is not needed: the update type already says which component moved, and
// the arrival version says which one a caret pins. The bump crosses exactly
// when the moved component is at or left of the pinned one.
export function classifyBump(updates) {
  const list = Array.isArray(updates) ? updates : []
  const reasons = []

  for (const update of list) {
    const name = update?.dependencyName || '(unnamed)'
    const moved = MOVED_INDEX[update?.updateType]
    if (moved === undefined) {
      reasons.push(`${name}: unreadable update type (${update?.updateType})`)
      continue
    }
    const arrival = parseVersion(update?.newVersion)
    if (!arrival) {
      reasons.push(`${name}: unreadable version (${update?.newVersion})`)
      continue
    }
    const pinned = pinnedIndex(arrival)
    if (pinned < 0) {
      reasons.push(`${name}: unreadable version (${update?.newVersion})`)
      continue
    }
    if (moved <= pinned) {
      const kind = update.updateType.replace('version-update:semver-', '')
      reasons.push(`${name}: ${kind} to ${update.newVersion} moves the component a caret pins`)
    }
  }

  if (list.length === 0) reasons.push('no dependency metadata to read')

  return { compatible: reasons.length === 0, reasons }
}

// --- Command line -----------------------------------------------------------
//
// Reads fetch-metadata's `updated-dependencies-json` from UPDATES, prints
// every blocking reason, and writes `compatible=` to GITHUB_OUTPUT.
//
// A CLI here rather than `node -e` in the workflow, for a reason worth
// recording: `node -e` runs as CommonJS, where a top-level `await import()`
// is a syntax error -- and a workflow step whose script cannot parse fails in
// a way that reads like the gate refusing. Keeping the entry point beside the
// rule also means the YAML holds no JavaScript.
if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const verdict = classifyBump(JSON.parse(process.env.UPDATES || '[]'))
  for (const reason of verdict.reasons) console.log(`breaking :: ${reason}`)
  console.log(`${verdict.reasons.length} blocking reason(s)`)
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `compatible=${verdict.compatible}\n`)
  }
}
