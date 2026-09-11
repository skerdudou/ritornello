import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, symlinkSync, renameSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { hashGitDir } from './hash-git-dir.mjs'

// A fingerprint that does not change when the tree changes is worse than none:
// it is a check that reports "untouched" while something was touched. Every
// test below is a mutation of the tree, and asserts the digest moved.

function scratch() {
  const root = mkdtempSync(join(tmpdir(), 'hash-git-dir-'))
  mkdirSync(join(root, 'hooks'), { recursive: true })
  mkdirSync(join(root, 'refs', 'heads'), { recursive: true })
  writeFileSync(join(root, 'config'), '[core]\n\trepositoryformatversion = 0\n')
  writeFileSync(join(root, 'HEAD'), 'ref: refs/heads/main\n')
  writeFileSync(join(root, 'index'), 'binary-ish')
  return root
}

const withTree = (fn) => {
  const root = scratch()
  try {
    return fn(root)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}

test('the same tree hashes the same twice', () => {
  withTree((root) => {
    assert.equal(hashGitDir(root), hashGitDir(root))
  })
})

test('two identical trees in different places hash differently', () => {
  // The paths are part of the digest, so this is expected -- stated as a test
  // so nobody later "fixes" it into a content-only hash and loses the ability
  // to notice a file moving.
  withTree((a) => {
    withTree((b) => {
      assert.notEqual(hashGitDir(a), hashGitDir(b))
    })
  })
})

test('changing a file changes the digest', () => {
  withTree((root) => {
    const before = hashGitDir(root)
    writeFileSync(join(root, 'config'), '[core]\n\trepositoryformatversion = 0\n[filter "x"]\n\tclean = touch /tmp/pwned\n')
    assert.notEqual(hashGitDir(root), before)
  })
})

test('adding a file changes the digest', () => {
  withTree((root) => {
    const before = hashGitDir(root)
    writeFileSync(join(root, 'hooks', 'pre-commit'), '#!/bin/sh\ntouch /tmp/pwned\n')
    assert.notEqual(hashGitDir(root), before)
  })
})

test('removing a file changes the digest', () => {
  withTree((root) => {
    const before = hashGitDir(root)
    rmSync(join(root, 'HEAD'))
    assert.notEqual(hashGitDir(root), before)
  })
})

test('adding an empty directory changes the digest', () => {
  // `find -type f` would not have seen this at all.
  withTree((root) => {
    const before = hashGitDir(root)
    mkdirSync(join(root, 'objects'))
    assert.notEqual(hashGitDir(root), before)
  })
})

test('A SYMLINK APPEARING CHANGES THE DIGEST', (t) => {
  // The case the shell version missed: `find … -type f` matches no symlink, so
  // a symlinked hook was invisible to the fingerprint and would still have
  // been executed by the git command it was meant to protect.
  withTree((root) => {
    const before = hashGitDir(root)
    try {
      symlinkSync('/bin/sh', join(root, 'hooks', 'pre-commit'))
    } catch (error) {
      // Windows refuses symlinks without privilege; the CI runner is Linux and
      // does not. Skipping silently would make this pass for the wrong reason.
      if (error.code === 'EPERM' || error.code === 'EACCES') {
        // Reported as skipped, never as passed. A test that asserts
        // nothing while the summary says `skipped 0` is a test that lies
        // about its own coverage -- and this is the assertion the whole
        // module was rewritten for, so its absence has to be visible.
        t.skip('this platform refuses to create symlinks')
        return
      }
      throw error
    }
    assert.notEqual(hashGitDir(root), before)
  })
})

test('repointing a symlink changes the digest', (t) => {
  withTree((root) => {
    try {
      symlinkSync('/bin/true', join(root, 'hooks', 'pre-commit'))
    } catch (error) {
      if (error.code === 'EPERM' || error.code === 'EACCES') {
        // Reported as skipped, never as passed. A test that asserts
        // nothing while the summary says `skipped 0` is a test that lies
        // about its own coverage -- and this is the assertion the whole
        // module was rewritten for, so its absence has to be visible.
        t.skip('this platform refuses to create symlinks')
        return
      }
      throw error
    }
    const before = hashGitDir(root)
    rmSync(join(root, 'hooks', 'pre-commit'))
    symlinkSync('/bin/sh', join(root, 'hooks', 'pre-commit'))
    assert.notEqual(hashGitDir(root), before)
  })
})

test('replacing a file with a symlink of the same name changes the digest', (t) => {
  // The nastiest shape: the entry keeps its name, so a listing that records
  // only names would not move. The type is part of each line for this reason.
  withTree((root) => {
    writeFileSync(join(root, 'hooks', 'pre-commit'), 'x')
    const before = hashGitDir(root)
    rmSync(join(root, 'hooks', 'pre-commit'))
    try {
      symlinkSync('/bin/sh', join(root, 'hooks', 'pre-commit'))
    } catch (error) {
      if (error.code === 'EPERM' || error.code === 'EACCES') {
        // Reported as skipped, never as passed. A test that asserts
        // nothing while the summary says `skipped 0` is a test that lies
        // about its own coverage -- and this is the assertion the whole
        // module was rewritten for, so its absence has to be visible.
        t.skip('this platform refuses to create symlinks')
        return
      }
      throw error
    }
    assert.notEqual(hashGitDir(root), before)
  })
})

test('renaming a file changes the digest', () => {
  withTree((root) => {
    const before = hashGitDir(root)
    renameSync(join(root, 'HEAD'), join(root, 'HEAD2'))
    assert.notEqual(hashGitDir(root), before)
  })
})

test('a newline in a file name does not flatten into its neighbours', (t) => {
  // The first version joined the listing with `\n` and claimed the separator
  // "cannot occur in any line above". False: a file name may contain one, and
  // the review demonstrated at the encoding layer that
  // `["f R/a HA", "f R/z HZ"]` and `["f R/a HA\nf R/z HZ"]` hashed identically
  // -- two different trees, one digest.
  //
  // A filesystem-level collision cannot be constructed from here, since the
  // content hash in each line is ours and not the attacker's to choose. What
  // this test does prove is that such a name is recorded, survives, and moves
  // the digest -- and the encoding is now JSON, which escapes the separator
  // inside each value instead of promising it never appears.
  withTree((root) => {
    const before = hashGitDir(root)
    const nasty = join(root, 'refs', 'a\nf x')
    try {
      writeFileSync(nasty, 'x')
    } catch (error) {
      // Windows refuses a newline in a file name outright.
      if (error.code === 'EINVAL' || error.code === 'ENOENT' || error.code === 'EPERM') {
        t.skip('this platform refuses a newline in a file name')
        return
      }
      throw error
    }
    const withNasty = hashGitDir(root)
    assert.notEqual(withNasty, before)

    rmSync(nasty)
    writeFileSync(join(root, 'refs', 'a\nf y'), 'x')
    assert.notEqual(hashGitDir(root), withNasty)
  })
})

test('a missing directory throws rather than hashing to a constant', () => {
  // A fingerprint of a tree that is not there must not be a value the
  // comparison could match. Under `set -e` the step dies, which is the
  // fail-closed direction.
  assert.throws(() => hashGitDir(join(tmpdir(), 'hash-git-dir-does-not-exist-9e3a')), /ENOENT/)
})
