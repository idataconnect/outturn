// A file the agent stores or removes shows in the files panel at once.
//
// Needs the cluster and a live model. Run by hand: cd ui && node e2e/agent-files.mjs
//
// The agent writes straight to the object store, which the API never sees, so
// the panel used to show an agent's files only once somebody reloaded.

import { chromium } from '@playwright/test'
import { execSync } from 'node:child_process'

const UI = 'http://localhost:3000'
const log = (...a) => console.log(...a)
const pass = (m) => log(`  PASS  ${m}`)
let failures = 0
const fail = (m) => {
  log(`  FAIL  ${m}`)
  failures += 1
}

async function signIn(page, email, password) {
  await page.goto(UI, { waitUntil: 'domcontentloaded' })
  await page.getByLabel(/email/i).fill(email)
  await page.getByLabel(/password/i).fill(password)
  await page.getByRole('button', { name: /sign in|log in/i }).click()
  await page.waitForTimeout(2500)
  const pick = page.getByRole('button', { name: /acme/i })
  if ((await pick.count()) > 0) {
    await pick.first().click()
    for (let i = 0; i < 30; i += 1) {
      await page.waitForTimeout(1000)
      const ok = await page.evaluate(async () => {
        const r = await fetch('/v1/session', { credentials: 'include' })
        return r.ok ? Boolean((await r.json())?.workspace_id) : false
      })
      if (ok) return
    }
    throw new Error('the session never settled on a workspace')
  }
}

async function waitForIdle(page, timeout = 600_000) {
  const until = Date.now() + timeout
  let last = ''
  let stableFor = 0
  while (Date.now() < until) {
    await page.waitForTimeout(5000)
    const text = await page.locator('body').innerText()
    if (text === last && text.length > 0) {
      stableFor += 5
      if (stableFor >= 25) return true
    } else {
      stableFor = 0
      last = text
    }
  }
  return false
}



async function main() {
  const secrets = execSync('scripts/dev-secrets.sh --print', {
    cwd: '/Users/b/src/outturn',
    encoding: 'utf8',
  })
  const adminPass = secrets.match(/PASSWORD=(.+)/)[1].trim()
  const browser = await chromium.launch()
  const page = await browser.newPage({ viewport: { width: 1400, height: 900 } })
  const errors = []
  page.on('pageerror', (e) => errors.push(String(e)))

  log('\n=== a file the agent stores appears without a reload ===')
  await signIn(page, 'admin@outturn.local', adminPass)
  await page.goto(`${UI}/sessions`, { waitUntil: 'domcontentloaded' })
  await page.getByText('New chat').first().click()
  await page.waitForTimeout(1500)
  await page.getByText('Helper', { exact: true }).first().click()
  await page.waitForTimeout(1000)
  const name = `note-${Date.now()}.txt`
  const box = page.getByPlaceholder(/message the agent/i)
  await box.click()
  await box.fill(`Write a file called ${name} in this session's files containing the word hello. Nothing else.`)
  await box.press('Enter')
  await page.waitForURL(/\/sessions\/[0-9a-f-]{36}/, { timeout: 30_000 })
  log(`  ${page.url()}`)
  await waitForIdle(page)

  // The panel's own control rather than the name: the name is in the
  // transcript too, where the agent says what it wrote.
  const control = page.getByLabel(`Download session/${name}`)
  if ((await control.count()) > 0) pass(`${name} is listed without a reload`)
  else fail(`${name} is not listed until a reload`)

  await box.click()
  await box.fill(`Delete ${name}.`)
  await box.press('Enter')
  await waitForIdle(page)
  if ((await control.count()) === 0) pass('and gone once the agent deletes it')
  else fail('still listed after the agent deleted it')

  if (errors.length > 0) fail(`browser errors: ${errors.join('; ')}`)
  else pass('no browser errors')
  log(`\n${failures === 0 ? 'ALL PASSED' : `${failures} FAILED`}`)
  await browser.close()
  process.exit(failures ? 1 : 0)
}

main().catch((e) => {
  console.error(e)
  process.exit(1)
})
