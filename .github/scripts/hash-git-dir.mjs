import { createHash } from 'node:crypto'
import { readdirSync, readFileSync, readlinkSync, lstatSync } from 'node:fs'
import { sep } from 'node:path'
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
// on purpose.** The pipeline appeared wherever a git command was about to run
// on a tree the analysis could have touched, and four copies of a security
// primitive drift -- this repository has the scars. One implementation, with
// tests, including the cases the shell version got wrong.
//
// Being a file rather than inline shell has a cost, and the workflow pays it
// rather than ignoring it: a `run:` block's text is materialised by the runner
// before the job starts, so it cannot be rewritten from the runner, while this
// module is read at run time. Every call site after the analysis therefore
// runs it from a `tools/` checkout fetched from the server AFTER the analysis
// has exited. See that step.
//
// Three things the shell version got wrong, each now a test:
//
//   - `find … -type f` matched neither symlinks nor the directories they might
//     replace, so a symlinked `pr/.git/hooks/pre-commit` was invisible to the
//     fingerprint and would have been executed by the very commit it was meant
//     to protect.
//   - joining the listing with a newline and calling it "a separator that
//     cannot occur" was false: a file name may contain one.
//   - and the first version of THIS file classified entries from
//     `readdirSync`'s dirent flags, which report UNKNOWN wherever `d_type` is
//     not filled in -- every ordinary file would then have recorded a mode and
//     no content hash, which is the fail-open direction.
//
// **Names are handled as bytes, never as strings.** Node decodes directory
// entries as UTF-8 by default, and invalid bytes all collapse to U+FFFD, so
// two names differing only in such bytes would share a digest -- injective
// encoding applied one step too late to matter. Reading with
// `encoding: 'buffer'` and hashing the raw bytes closes that. It is a weak
// hole (it needs non-UTF-8 names and yields nothing useful), and closing it
// costs two words.

const SEPARATOR = Buffer.from(sep)

export function hashGitDir(root) {
  const entries = []

  const walk = (dir) => {
    const names = readdirSync(dir, { encoding: 'buffer' }).sort(Buffer.compare)
    for (const name of names) {
      const path = Buffer.concat([dir, SEPARATOR, name])
      // `lstat`, so the entry is described rather than whatever a symlink
      // points at: following one would hash the target and miss that a link
      // appeared at all. Proven by a test that changes a link target's
      // contents outside the hashed tree and asserts the digest does not move.
      const stat = lstatSync(path)
      if (stat.isSymbolicLink()) {
        entries.push(['l', path.toString('hex'), readlinkSync(path, { encoding: 'buffer' }).toString('hex')])
      } else if (stat.isDirectory()) {
        entries.push(['d', path.toString('hex')])
        walk(path)
      } else if (stat.isFile()) {
        entries.push(['f', path.toString('hex'), createHash('sha256').update(readFileSync(path)).digest('hex')])
      } else {
        // A socket, a fifo, a device. None of these belongs in a git
        // directory, and refusing to classify one as "nothing" is the point:
        // its presence must change the digest.
        entries.push(['?', path.toString('hex'), `mode=${stat.mode}`])
      }
    }
  }

  walk(Buffer.from(root))
  // JSON, not a joined string: it escapes any separator inside a value instead
  // of promising none appears, and the array structure survives it. Every
  // value here is hex or a small literal, so the encoding is injective over
  // the bytes the tree actually holds.
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
