import { createT } from '@ritornello/ui'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'
import {
  emptyData, faultText, parseClock, playFailureText, runtimeErrorText, stateText, targetText, updateErrorText,
  videoCount, workInProgress,
} from './data'
import { CATALOG, state } from './harness'

const t = createT(CATALOG)

describe('faultText', () => {
  it('has a sentence for every code the plugin sends', () => {
    // The codes of `admin/probe.rs` and `admin/explore.rs`.
    for (const code of [
      'not_a_url', 'foreign_host', 'no_media', 'live_stream', 'tool_missing', 'timeout', 'failed',
      'unknown_root', 'not_writable', 'not_mounted', 'escapes', 'unresponsive', 'io', 'wrong_kind',
      'unavailable', 'invalid_id', 'timed_out', 'interrupted', 'job_lost', 'no_such_preset',
      'preset_no_playlist', 'preset_no_video', 'bad_entry', 'failed_silent', 'exited', 'local_network',
      'unreadable_output', 'no_ffmpeg', 'trim_playlist', 'trim_outside',
    ]) {
      const text = faultText(t, { code, detail: 'the detail' })
      expect(text, code).not.toMatch(/^fault_/)
      expect(text, code).not.toBe('')
    }
  })

  it('says a live stream is not supported', () => {
    expect(faultText(t, { code: 'live_stream' })).toBe('live streams are not supported')
  })

  it('shows what the tool said for a code it does not know, rather than nothing', () => {
    expect(faultText(t, { code: 'brand_new', detail: 'ERROR: x' })).toBe('yt-dlp: ERROR: x')
    expect(faultText(t, { code: 'brand_new' })).toBe('yt-dlp: brand_new')
  })
})

describe('parseClock', () => {
  it('reads seconds, m:ss and h:mm:ss, and empty as no bound', () => {
    expect(parseClock('')).toBeNull()
    expect(parseClock('  ')).toBeNull()
    expect(parseClock('95')).toBe(95)
    expect(parseClock('1:35')).toBe(95)
    expect(parseClock(' 01:35 ')).toBe(95)
    expect(parseClock('1:01:35')).toBe(3695)
    expect(parseClock('0:00')).toBe(0)
  })

  it('refuses what is not a time rather than guessing', () => {
    for (const typo of ['1:75', '1:60:00', '1:2:3:4', '1.5', '-3', 'abc', '1:', ':30', '1::30']) {
      expect(parseClock(typo), typo).toBeUndefined()
    }
  })
})

describe('updateErrorText', () => {
  it('has a sentence for every code the plugin sends, with its parameters filled', () => {
    // The codes of `UpdateError::fault` in tool.rs.
    for (const code of [
      'busy', 'python3_missing', 'python3_unusable', 'fetch', 'refused_bad_signature', 'refused_not_listed',
      'refused_mismatch', 'rollback_older', 'rollback_other_build', 'rollback_unreadable', 'rollback_other',
      'install', 'install_no_package', 'install_does_not_start', 'install_no_version', 'install_swap',
      'install_not_zip', 'install_extract_limit', 'install_extract_unsafe', 'install_write',
    ]) {
      const text = updateErrorText(t, { code, params: { detail: 'D', offered: 'O', installed: 'I' } })
      expect(text, code).not.toBe(code)
      expect(text, code).not.toMatch(/\{\w+\}/)
    }
  })

  it('keeps the raw text of an outside voice, and the versions', () => {
    expect(updateErrorText(t, { code: 'fetch', params: { detail: 'connection reset' } })).toContain('connection reset')
    expect(updateErrorText(t, { code: 'rollback_older', params: { offered: '2025.12.31', installed: '2026.10.26' } }))
      .toBe('release 2025.12.31 is older than the installed 2026.10.26')
  })
})

describe('runtimeErrorText', () => {
  it('has a sentence for every code the plugin sends', () => {
    // The codes of `RuntimeError::fault` in tool/runtime.rs.
    for (const code of [
      'no_build', 'fetch', 'bad_signature', 'not_listed', 'unexpected', 'mismatch', 'extract', 'version', 'write',
      'rollback',
    ]) {
      const params = { runtime: 'node', detail: 'D', arch: 'arm', installed: 'I', listed: 'L' }
      const text = runtimeErrorText(t, { code, params })
      expect(text, code).not.toBe(code)
      expect(text, code).not.toMatch(/\{\w+\}/)
    }
  })

  it('names the runtime as it is usually written', () => {
    expect(runtimeErrorText(t, { code: 'write', params: { runtime: 'node', detail: 'disk full' } }))
      .toBe('Node.js could not be written: disk full')
    expect(runtimeErrorText(t, { code: 'no_build', params: { runtime: 'deno', arch: 'riscv64' } }))
      .toBe('Deno publishes no build for this device (riscv64)')
    expect(runtimeErrorText(t, { code: 'version', params: { runtime: 'node', detail: 'v23.0.0 instead of v22.6.0' } }))
      .toBe('the downloaded Node.js does not answer the expected version: v23.0.0 instead of v22.6.0')
    expect(runtimeErrorText(t, { code: 'rollback', params: { runtime: 'node', installed: '22.10.0', listed: '22.9.5' } }))
      .toBe('the signed list offers Node.js 22.9.5, older than the installed 22.10.0: refused, the installed one is kept')
  })
})

describe('texts', () => {
  it('counts videos in the singular too', () => {
    expect(videoCount(t, 1)).toBe('1 video')
    expect(videoCount(t, 12)).toBe('12 videos')
    expect(videoCount(t, 0)).toBe('0 videos')
  })

  it('names states and targets', () => {
    expect(stateText(t, 'skipped')).toBe('skipped')
    expect(stateText(t, 'something else')).toBe('something else')
    expect(targetText({ root: 'nas', path: '' })).toBe('nas')
    expect(targetText({ root: 'nas', path: 'a/b' })).toBe('nas/a/b')
  })
})

describe('workInProgress', () => {
  it('is false for a quiet page and for no page', () => {
    expect(workInProgress(null)).toBe(false)
    expect(workInProgress(state())).toBe(false)
    expect(workInProgress(emptyData())).toBe(false)
  })

  it('is true for each thing the page waits for, and only those', () => {
    const d = state()
    expect(workInProgress({ ...d, probe: { running: true, results: [] } })).toBe(true)
    expect(workInProgress({ ...d, explore: { ...d.explore, busy: true } })).toBe(true)
    expect(workInProgress({ ...d, tool: { ...d.tool, updating: true } })).toBe(true)
    expect(workInProgress({ ...d, tool: { ...d.tool, waiting_for_jobs: true } })).toBe(true)
    expect(workInProgress({ ...d, capabilities: { ...d.capabilities, checking: true } })).toBe(true)
    const job = { id: 1, presets: [1], target: { root: 'n', path: '' }, rows: [] }
    expect(workInProgress({ ...d, jobs: [{ ...job, state: 'queued' }] })).toBe(true)
    expect(workInProgress({ ...d, jobs: [{ ...job, state: 'running' }] })).toBe(true)
    expect(workInProgress({ ...d, jobs: [{ ...job, state: 'done' }, { ...job, state: 'failed' }] })).toBe(false)
  })

  it('waits for a first install from before it starts, and not past a failed one', () => {
    const d = state()
    const pending = { ...d, tool: { ...d.tool, installed: false, version: null } }
    expect(workInProgress(pending), 'python3 there, nothing installed yet').toBe(true)
    expect(workInProgress({ ...pending, capabilities: { ...d.capabilities, python3: null } }), 'no python3').toBe(false)
    const failed = { ...pending, tool: { ...pending.tool, error_code: { code: 'fetch', params: { detail: 'x' } } } }
    expect(workInProgress(failed), 'the install failed and says so').toBe(false)
  })
})

/** A shipped catalogue of this plugin, flat `key = "value"` lines. */
function shipped(lang: string): Record<string, string> {
  const path = lang === 'en' ? '../src/locales/en.toml' : `../../../deploy/locales/web-streaming/${lang}.toml`
  const out: Record<string, string> = {}
  for (const line of readFileSync(resolve(process.cwd(), path), 'utf8').split(/\r?\n/)) {
    const m = /^([A-Za-z0-9_]+) = "(.*)"$/.exec(line.trim())
    if (m) out[m[1]!] = m[2]!
  }
  return out
}

describe('playFailureText', () => {
  it('has its own sentence for every code the wrapper writes, in each of the five languages', () => {
    // The codes of `failure_code` in tool/wrapper.rs.
    const codes = ['guard_refused', 'live_refused', 'no_formats', 'ytdlp_failed']
    for (const lang of ['en', 'fr', 'de', 'es', 'it']) {
      const tl = createT(shipped(lang))
      const texts = codes.map((code) => playFailureText(tl, code))
      for (const [i, text] of texts.entries()) {
        expect(text, `${lang} ${codes[i]}`).not.toMatch(/^play_failed_/)
        expect(text, `${lang} ${codes[i]}`).not.toMatch(/\{\w+\}/)
      }
      expect(new Set(texts).size, `${lang}: one sentence per code`).toBe(codes.length)
      const line = tl('last_failure', { when: 'W', host: 'H', cause: texts[0]! })
      expect(line, lang).toContain('W')
      expect(line, lang).toContain('H')
      expect(line, lang).toContain(texts[0]!)
    }
  })

  it('reads a code a newer plugin may add as yt-dlp\'s own failure', () => {
    expect(playFailureText(t, 'brand_new')).toBe(playFailureText(t, 'ytdlp_failed'))
  })
})
