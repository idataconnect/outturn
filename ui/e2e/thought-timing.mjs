// Thinking time, end to end: a tab that watched a turn think must show each
// thought's duration, and the same one a reload shows.
//
// Not part of `npm test` -- it needs the cluster from `scripts/dev.sh` and a
// live model that thinks. Run it by hand:
//
//     cd ui && node e2e/thought-timing.mjs
//
// It exists because the durations were measured by the API and only reached
// the browser with the stored parts, which a watching tab never re-reads: every
// thought showed a word count and no time until somebody reloaded.

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

const psql = (sql) =>
  execSync(
    // Flattened: a newline inside the quoted argument reaches psql as a literal
    // backslash-n and is a syntax error there.
    `kubectl exec postgres-0 -- psql -U outturn -d outturn -t -A -c ${JSON.stringify(
      sql.replace(/\s+/g, ' ').trim(),
    )}`,
    { encoding: 'utf8' },
  ).trim()

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


// Each thought's duration, read off the page.
async function thoughtTimes(page) {
  const labels = await page.getByRole('button', { name: /^Thought/ }).allInnerTexts()
  return labels.map((l) => {
    const m = /([\d.]+)s/.exec(l)
    return m ? Number(m[1]) : null
  })
}

async function main() {
  const secrets = execSync('scripts/dev-secrets.sh --print', {
    cwd: '/Users/b/src/outturn',
    encoding: 'utf8',
  })
  const adminPass = secrets.match(/PASSWORD=(.+)/)[1].trim()

  const browser = await chromium.launch()
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } })
  const errors = []
  page.on('pageerror', (e) => errors.push(String(e)))

  log('\n=== thinking time, watched live and after a reload ===')
  await signIn(page, 'admin@outturn.local', adminPass)
  await page.goto(`${UI}/sessions`, { waitUntil: 'domcontentloaded' })
  await page.getByText('New chat').first().click()
  await page.waitForTimeout(1500)
  await page.getByText('Helper', { exact: true }).first().click()
  await page.waitForTimeout(1000)

  const box = page.getByPlaceholder(/message the agent/i)
  await box.click()
  // A tool call between two thoughts, so there is more than one to time.
  await box.fill('Think about what day it might be, then check the current time with your tool, then think about whether the Orchard Room is free this weekend and answer briefly.')
  await box.press('Enter')
  await page.waitForURL(/\/sessions\/[0-9a-f-]{36}/, { timeout: 30_000 })
  log(`  ${page.url()}`)
  await waitForIdle(page)

  const live = await thoughtTimes(page)
  log(`  live:     ${JSON.stringify(live)}`)
  if (live.length === 0) fail('no thought was drawn')
  else if (live.some((t) => t === null)) fail('a thought watched live shows no time')
  else pass(`every one of ${live.length} thought(s) shows a time live`)

  await page.reload({ waitUntil: 'domcontentloaded' })
  await page.waitForTimeout(4000)
  const stored = await thoughtTimes(page)
  log(`  reloaded: ${JSON.stringify(stored)}`)
  if (stored.length !== live.length) {
    fail(`${live.length} thoughts live, ${stored.length} after reload`)
  } else {
    // The live clock is the events' ids and the stored one the worker's wall
    // clock as fragments arrived: the same instants to within a poll.
    const off = live.map((t, i) => Math.abs((t ?? 0) - (stored[i] ?? 0)))
    if (off.every((d) => d <= 1)) pass('live and stored times agree to within a second')
    else fail(`live and stored times differ by ${JSON.stringify(off)}`)
    if (new Set(stored).size < stored.length && stored.length > 1)
      log('  note: two thoughts share a time')
  }

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
