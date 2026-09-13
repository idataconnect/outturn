import '@testing-library/jest-dom/vitest'
import { cleanup } from '@testing-library/react'
import { afterEach } from 'vitest'

// Each test gets an empty document; Testing Library does not unmount on its
// own, and a component left behind makes the next test's queries ambiguous.
afterEach(cleanup)

// jsdom supplies a `localStorage` that is missing parts of the API (`clear`
// among them), so tests that remember anything get a real in-memory one. The
// app only ever reads and writes through try/catch, but a test that cannot
// reset storage leaks state into the next one.
class MemoryStorage implements Storage {
  #items = new Map<string, string>()

  get length() {
    return this.#items.size
  }
  key(index: number) {
    return Array.from(this.#items.keys())[index] ?? null
  }
  getItem(key: string) {
    return this.#items.get(key) ?? null
  }
  setItem(key: string, value: string) {
    this.#items.set(key, String(value))
  }
  removeItem(key: string) {
    this.#items.delete(key)
  }
  clear() {
    this.#items.clear()
  }
}

Object.defineProperty(window, 'localStorage', {
  configurable: true,
  value: new MemoryStorage(),
})
