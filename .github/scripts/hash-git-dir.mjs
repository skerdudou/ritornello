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
    for (const name of readdirSync(dir).sort()) {
      const path = join(dir, name)
      // **`lstat`, not the dirent's own type flags.** `readdirSync` with
      // `withFileTypes` reports UNKNOWN on any filesystem that does not fill
      // in `d_type`, and a classifier that falls through to "something else"
      // there would record every ordinary file as a mode with no content
      // hash -- the digest would silently stop covering content, which is the
      // fail-open direction. `lstat` answers on every filesystem, and it
      // describes the entry itself rather than what a symlink points at:
      // following one would hash the target and miss that a link appeared.
      const stat = lstatSync(path)
      if (stat.isSymbolicLink()) {
        entries.push(['l', path, readlinkSync(path)])
      } else if (stat.isDirectory()) {
        entries.push(['d', path])
        walk(path)
      } else if (stat.isFile()) {
        entries.push(['f', path, createHash('sha256').update(readFileSync(path)).digest('hex')])
      } else {
        // A socket, a fifo, a device. None of these belongs in a git
        // directory, and refusing to classify one as "nothing" is the point:
        // its presence must change the digest.
        entries.push(['?', path, `mode=${stat.mode}`])
      }
    }
  }

  walk(root)
  // **JSON, not a joined string.** A separator "that cannot occur in any line
  // above" was the first version's claim and it was false: a file name and a
  // symlink target may both contain a newline, so `["f a", "f b"]` and
  // `["f a\nf b"]` hashed identically -- two different trees, one digest.
  // JSON escapes the separator inside each value, and the array structure
  // survives it.
  return createHash('sha256').update(JSON.stringify(entries)).digest('hex')
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const root = process.argv[2]
  if (!root) {
    console.error('usage: hash-git-dir.mjs <directory>')
    process.exit(2)
  }
  process.stdout.write(`${hashGitDir(root)}\n`)
}
