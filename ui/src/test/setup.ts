import '@testing-library/jest-dom/vitest'
import { cleanup } from '@testing-library/react'
import { afterEach } from 'vitest'

// Each test gets an empty document; Testing Library does not unmount on its
// own, and a component left behind makes the next test's queries ambiguous.
afterEach(cleanup)
