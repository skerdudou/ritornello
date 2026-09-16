#!/usr/bin/env node
// Anti-regression guardrail over package-lock.json, run in CI right after
// `npm ci`. Sibling in spirit of check-dist.mjs and check-plugin-dist.mjs: a
// green `npm ci` guarantees only that the lock installs, not that the tree it
// describes can actually be loaded, nor that it still covers every platform.
//
// The three checks below each encode a mistake that has been made here.
// Do not relax one to make it pass — if it fires, it has found something.
//
// Deliberately reads nothing but the lockfile. Resolving through the file
// system is what hid the second defect for a whole session: a worktree lives
// under .claude/worktrees/ inside the main checkout, so Node's upward walk
// leaves the worktree and answers from the main checkout's node_modules. A
// package missing from the tree then verifies as present, and the build only
// fails on a clean checkout — that is, in CI.
import { readFileSync } from 'node:fs'

const lockPath = process.argv[2] ?? 'package-lock.json'

const problems = []
function fail(check, message) {
  problems.push(`${check}: ${message}`)
}

let lock
try {
  lock = JSON.parse(readFileSync(lockPath, 'utf8'))
} catch (e) {
  console.error(`check-lockfile: cannot read ${lockPath} — ${e.message}`)
  process.exit(1)
}
const pkgs = lock.packages ?? {}

// A workspace entry is one whose path holds no node_modules segment: the root
// ('') and the eight members. Their manifests are the only declarations we own.
const isWorkspace = (p) => !p.includes('node_modules')

// Node's own rule, expressed on lock paths. From a package at `where`, a bare
// import is looked up in <where>/node_modules/<name>, then in the node_modules
// of each parent directory, up to the root. In lock terms that means stripping
// the trailing /node_modules/<pkg> segment and retrying. It stops at the root:
// anything above is outside the project and must never be relied upon.
function resolves(where, name) {
  let p = where
  for (;;) {
    if (pkgs[(p ? p + '/' : '') + 'node_modules/' + name]) return true
    const i = p.lastIndexOf('/node_modules/')
    if (i === -1) {
      if (p === '') return false
      p = '' // a workspace path such as web/app: next stop is the root
    } else {
      p = p.slice(0, i)
    }
  }
}

// ---------------------------------------------------------------------------
// 1. Every non-optional peer dependency must be loadable by Node's rule.
//
// npm satisfies a peer through the dependency *graph*: it is happy when the
// package that pulls the plugin in also declares the peer, wherever that sits.
// Node loads through the *file system*. The two disagree exactly when npm
// hoists a plugin to the root while the peer lives in a workspace — which is
// what happened: @vitejs/plugin-vue and @tailwindcss/vite were hoisted to the
// root and both `import 'vite'` at run time, so dropping the root's vite left
// `npm ls` exiting 0 and the build failing with ERR_MODULE_NOT_FOUND.
for (const [path, entry] of Object.entries(pkgs)) {
  if (entry.link) continue
  const meta = entry.peerDependenciesMeta ?? {}
  for (const name of Object.keys(entry.peerDependencies ?? {})) {
    if (meta[name]?.optional) continue
    if (!resolves(path, name)) {
      fail(
        'unloadable-peer',
        `${path || '<root>'} peer-depends on '${name}', but no node_modules on ` +
          `its resolution path holds it — Node will fail with ERR_MODULE_NOT_FOUND. ` +
          `Declare '${name}' where the dependent is hoisted (usually the root manifest).`,
      )
    }
  }
}

// ---------------------------------------------------------------------------
// 2. No version of a package we declare may fall outside our own ranges.
//
// A lockfile entry that satisfies its range is never revisited by npm, so one
// written when it was current survives every later install. A root `vite`
// 5.4.21 lingered this way long after the eight manifests had moved to ^8.2.2,
// and carried four security advisories that Dependabot could not act on: no
// manifest named it, so it had nothing to bump and opened no pull request.

// Only the shapes this repository actually uses. Anything else is refused
// rather than guessed at — a range silently mis-parsed would make this check
// pass for the wrong reason.
function rangeAllows(version, range) {
  const v = version.match(/^(\d+)\.(\d+)\.(\d+)/)
  if (!v) return null
  for (const alt of range.split('||').map((s) => s.trim())) {
    const m = alt.match(/^([\^~]?)(\d+)\.(\d+)\.(\d+)$/)
    if (!m) return null // unsupported shape: caller must treat as unknown
    const [, op, maj, min, pat] = m
    const [V, N, P] = [+v[1], +v[2], +v[3]]
    const [M, I, A] = [+maj, +min, +pat]
    const atLeast = V > M || (V === M && (N > I || (N === I && P >= A)))
    if (op === '^' && V === M && atLeast) return true
    if (op === '~' && V === M && N === I && P >= A) return true
    if (op === '' && V === M && N === I && P === A) return true
  }
  return false
}

const declared = {}
for (const [p, v] of Object.entries(pkgs)) {
  if (!isWorkspace(p)) continue
  for (const [n, r] of Object.entries({ ...v.dependencies, ...v.devDependencies })) {
    ;(declared[n] ??= new Set()).add(r)
  }
}

for (const [path, entry] of Object.entries(pkgs)) {
  if (!path.includes('node_modules') || entry.link) continue
  const name = path.slice(path.lastIndexOf('node_modules/') + 'node_modules/'.length)
  const ranges = declared[name]
  if (!ranges || !entry.version) continue
  const verdicts = [...ranges].map((r) => rangeAllows(entry.version, r))
  if (verdicts.some((x) => x === null)) {
    fail(
      'unsupported-range',
      `'${name}' is declared as ${[...ranges].join(' | ')}, a shape rangeAllows() ` +
        `does not parse — extend it rather than dropping the check.`,
    )
  } else if (!verdicts.some(Boolean)) {
    fail(
      'stale-version',
      `${path} is at ${entry.version}, which none of our declarations allow ` +
        `(${[...ranges].join(' | ')}). Nothing asks for it: remove the entry and ` +
        `reinstall, letting npm prune what becomes orphaned.`,
    )
  }
}

// ---------------------------------------------------------------------------
// 3. Every declared optional dependency must have an entry.
//
// npm records the platform binaries of every OS, not just the one that ran the
// install — unless the lock is regenerated from scratch, which on Windows drops
// the Linux and macOS ones (lightningcss-linux-*, @rolldown/binding-*,
// @emnapi/*) while still declaring them. `npm ci` installs only what the lock
// holds, so CI, which runs on Linux, would come up without its native
// bindings. Hence: fix a lockfile by removing the offending entry and
// reinstalling, never by deleting the file.
for (const [path, entry] of Object.entries(pkgs)) {
  if (entry.link) continue
  for (const name of Object.keys(entry.optionalDependencies ?? {})) {
    if (!resolves(path, name)) {
      fail(
        'missing-optional',
        `${path || '<root>'} declares optional dependency '${name}', which has no ` +
          `entry in the lock. If several are missing at once the lock was ` +
          `regenerated on one platform; restore it and prune surgically instead.`,
      )
    }
  }
}

// ---------------------------------------------------------------------------
if (problems.length > 0) {
  // Group by check so a platform-wide loss reads as one fault, not as fifty.
  const byCheck = new Map()
  for (const p of problems) {
    const [check] = p.split(':', 1)
    if (!byCheck.has(check)) byCheck.set(check, [])
    byCheck.get(check).push(p)
  }
  for (const [check, list] of byCheck) {
    console.error(`check-lockfile: ${list.length} ${check} problem(s)`)
    for (const p of list.slice(0, 10)) console.error(`  ${p}`)
    if (list.length > 10) console.error(`  ... and ${list.length - 10} more`)
  }
  process.exit(1)
}

console.log('check-lockfile: peers loadable, versions declared, platforms complete')
