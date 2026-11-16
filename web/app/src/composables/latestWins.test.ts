import { flushPromises } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import { latestWins } from './latestWins'

/** A run whose settlement the test decides. */
function controlled() {
  const calls: number[] = []
  const settle: { resolve: () => void; reject: () => void }[] = []
  const push = latestWins((v: number) => {
    calls.push(v)
    return new Promise<void>((resolve, reject) => settle.push({ resolve, reject: () => reject(new Error('boom')) }))
  })
  return { calls, settle, push }
}

describe('latestWins', () => {
  it('runs at once when idle', () => {
    const { calls, push } = controlled()
    push(1)
    expect(calls).toEqual([1])
  })

  it('while one runs, only the newest waiting value goes next', async () => {
    const { calls, settle, push } = controlled()
    push(1)
    push(2)
    push(3)
    push(4)
    expect(calls).toEqual([1])
    settle[0]!.resolve()
    await flushPromises()
    expect(calls).toEqual([1, 4])
  })

  it('a rejected run does not stall the queue', async () => {
    const { calls, settle, push } = controlled()
    push(1)
    push(2)
    settle[0]!.reject()
    await flushPromises()
    expect(calls).toEqual([1, 2])
  })

  it('a run that throws synchronously does not stall the queue', async () => {
    const calls: number[] = []
    let first = true
    const push = latestWins((v: number) => {
      calls.push(v)
      if (first) {
        first = false
        throw new Error('sync boom')
      }
      return Promise.resolve()
    })
    push(1)
    await flushPromises()
    push(2)
    await flushPromises()
    push(3)
    expect(calls).toEqual([1, 2, 3])
  })

  it('once everything settled, the next value runs at once', async () => {
    const { calls, settle, push } = controlled()
    push(1)
    settle[0]!.resolve()
    await flushPromises()
    push(2)
    expect(calls).toEqual([1, 2])
  })
})
