import { createHash } from 'node:crypto'
import { readdirSync, readFileSync, readlinkSync, lstatSync } from 'node:fs'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'

// Fingerprint a git directory, so that a later step can prove nothing rewrote
// it in between.
//
// This exists because `Edit(pr/**)` necessarily reaches `pr/.git/`, and a
// `filter.*.clean` entry written there, plus a matching `pr/.gitattributes`,
// turns an innocuous `git add` into arbitrary command execution inside a
// privileged job. That was a confirmed exploit here, not a hypothetical. The
// analysis's allow-list is supposed to prevent it; this is the second layer,
// chosen because it fails independently of how any glob matcher treats a
// dotted directory.
//
// **It is one file rather than four copies of a `find | sha256sum` pipeline
// on purpose.** The pipeline appears wherever a git command is about to run
// on a tree the analysis could have touched, and four copies of a security
// primitive drift -- this repository has the scars. One implementation, with
// tests, including the case the shell version got wrong.
//
// What the shell version got wrong: `find … -type f` matches neither symlinks
// nor the directories they might replace, so a symlinked
// `pr/.git/hooks/pre-commit` was invisible to the fingerprint and would have
// been executed by the very commit the fingerprint was meant to protect. No
// tool the analysis is given can create a symlink today, so it was never
// reachable -- but an unreachable hole guarded by an assumption is exactly
// what this design refuses to rely on.
//
// Every entry is recorded with its type. A file contributes its content hash,
// a symlink its target (read, never followed), a directory its name alone.
// Sorted, so the digest is a property of the tree and not of readdir order.

export function hashGitDir(root) {
  const entries = []

  const walk = (dir) => {
    // `withFileTypes` reports the entry itself, not what a symlink points at,
    // which is what is wanted here: following one would hash the target and
    // miss that a link appeared at all.
    for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : 1))) {
      const path = join(dir, entry.name)
      if (entry.isSymbolicLink()) {
        entries.push(`l ${path} -> ${readlinkSync(path)}`)
      } else if (entry.isDirectory()) {
        entries.push(`d ${path}`)
        walk(path)
      } else if (entry.isFile()) {
        entries.push(`f ${path} ${createHash('sha256').update(readFileSync(path)).digest('hex')}`)
      } else {
        // A socket, a fifo, a device. None of these belongs in a git
        // directory, and refusing to classify one as "nothing" is the point:
        // its presence must change the digest.
        const stat = lstatSync(path)
        entries.push(`? ${path} mode=${stat.mode}`)
      }
    }
  }

  walk(root)
  // Separator that cannot occur in any line above, so two different trees
  // cannot produce one identical concatenation.
  return createHash('sha256').update(entries.join('\n')).digest('hex')
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const root = process.argv[2]
  if (!root) {
    console.error('usage: hash-git-dir.mjs <directory>')
    process.exit(2)
  }
  process.stdout.write(`${hashGitDir(root)}\n`)
}
