// Records the README's walkthrough, against a workspace from scripts/demo-seed.sh.
//
// Not a test. It drives the UI at a pace a person can follow and leaves a
// .webm and a list of marks beside it; scripts/demo-video.sh cuts those into
// the MP4 and GIF. Run that rather than this.
//
// Each step waits for what it is about to show rather than for a fixed time,
// except the pauses, which are there for the viewer and are the point.

import { chromium } from '@playwright/test'
import { mkdirSync, writeFileSync } from 'node:fs'

const UI = process.env.UI ?? 'http://localhost:3000'
const OUT = process.env.OUT ?? 'e2e/out'
const email = process.env.OUTTURN_DEV_ADMIN_EMAIL
const password = process.env.OUTTURN_DEV_ADMIN_PASSWORD
const size = { width: 1440, height: 900 }

mkdirSync(OUT, { recursive: true })
const browser = await chromium.launch()

// Signed in off camera, and the session carried into the recorded context: a
// login form is the least interesting thing the product does.
const setup = await browser.newContext({ viewport: size })
const login = await setup.newPage()
await login.goto(UI)
await login.getByLabel(/email/i).fill(email)
await login.getByLabel(/password/i).fill(password)
await login.getByRole('button', { name: /sign in/i }).click()
await login.getByRole('button', { name: /acme/i }).first().click()
await login.getByRole('heading', { name: 'Dashboard' }).waitFor()
const state = await setup.storageState()
await setup.close()

const context = await browser.newContext({
  viewport: size,
  colorScheme: 'light',
  storageState: state,
  recordVideo: { dir: OUT, size },
})

// A cursor, because a recording has none and a click nobody sees reads as
// the page changing by itself.
await context.addInitScript(() => {
  addEventListener('DOMContentLoaded', () => {
    const dot = document.createElement('div')
    dot.style.cssText =
      'position:fixed;z-index:2147483647;pointer-events:none;width:18px;height:18px;' +
      'margin:-9px 0 0 -9px;border-radius:50%;background:rgba(30,30,30,.35);' +
      'border:2px solid #fff;box-shadow:0 0 0 1px rgba(0,0,0,.4);transition:transform .12s;' +
      `left:${window.__cx ?? 720}px;top:${window.__cy ?? 450}px`
    document.body.appendChild(dot)
    addEventListener('mousemove', (e) => {
      dot.style.left = `${e.clientX}px`
      dot.style.top = `${e.clientY}px`
    })
    addEventListener('mousedown', () => (dot.style.transform = 'scale(.7)'))
    addEventListener('mouseup', () => (dot.style.transform = ''))
  })
})

const page = await context.newPage()
const started = Date.now()
const marks = []
const mark = (name) => marks.push({ name, at: (Date.now() - started) / 1000 })
const pause = (s) => page.waitForTimeout(s * 1000)

let cursor = { x: 720, y: 450 }
async function point(locator) {
  // Retried, because a page still settling can replace the element between
  // finding it and measuring it.
  let box = null
  for (let i = 0; i < 10 && !box; i += 1) {
    try {
      await locator.first().scrollIntoViewIfNeeded({ timeout: 2000 })
      box = await locator.first().boundingBox()
    } catch {
      await page.waitForTimeout(300)
    }
  }
  if (!box) throw new Error(`nothing to point at: ${locator}`)
  const to = { x: box.x + box.width / 2, y: box.y + box.height / 2 }
  await page.mouse.move(cursor.x, cursor.y)
  await page.mouse.move(to.x, to.y, { steps: 25 })
  cursor = to
}
async function click(locator) {
  await point(locator)
  await pause(0.35)
  await locator.click()
}
async function scroll(px, steps = 30) {
  for (let i = 0; i < steps; i += 1) {
    await page.mouse.wheel(0, px / steps)
    await page.waitForTimeout(16)
  }
}
const nav = (name) => page.getByRole('navigation').getByRole('link', { name })

// The dashboard: what is waiting, what is running, and what it all cost.
await page.goto(UI)
await page.getByRole('heading', { name: 'Dashboard' }).waitFor()
mark('dashboard')
await click(page.getByRole('button', { name: '7 days' }))
await pause(2.5)
await scroll(520)
await pause(2.5)
await scroll(700)
await pause(2.5)
await scroll(-1220, 20)
await pause(1)

// The agents, and the one held for a person's word.
mark('agents')
await click(nav('Agents'))
await click(page.getByRole('link', { name: /^Front desk/ }))
await page.getByRole('heading', { name: 'Front desk' }).waitFor()
await pause(3)

// The approval, answered from the inbox, and the conversation carrying on.
mark('inbox')
await click(nav(/^Inbox/))
await page.getByRole('heading', { name: 'Charge to approve' }).waitFor()
await pause(3)
await click(page.getByRole('button', { name: 'Approve' }))
await pause(1.5)
// Answered, the item leaves the inbox and its link with it, so the
// conversation is reached the way anybody would: from the sessions.
await click(nav('Sessions'))
await click(page.getByRole('link', { name: /Jo Okafor/ }).first())
await pause(1)
mark('waiting')
// Done when the turn the approval released has finished, asked of the API:
// the transcript keeps the words "needs approval" in its record of the call,
// so the page's text says nothing about whether the agent is done.
await page.waitForFunction(
  async () => {
    const id = location.pathname.split('/').pop()
    const r = await fetch(`/v1/agent-sessions/${id}/messages`, { credentials: 'include' })
    if (!r.ok) return false
    const users = (await r.json()).messages.filter((m) => m.role === 'user')
    return users.at(-1)?.job_state === 'succeeded'
  },
  null,
  { timeout: 300_000, polling: 1000 },
)
await pause(1)
mark('replied')
await scroll(4000, 20)
await pause(3.5)

// A skill that wants a host: nothing is reached until the workspace allows it.
mark('skills')
await click(nav('Skills'))
await click(page.getByRole('link', { name: /Local weather/ }))
await page.getByRole('heading', { name: 'Local weather' }).waitFor()
await pause(2.5)
await click(page.getByRole('button', { name: 'Allow this host' }))
await pause(2.5)

// Every version kept, and compared against the live one.
mark('versions')
await click(nav('Skills'))
await click(page.getByRole('link', { name: /Hollowbrook House/ }))
await page.getByRole('heading', { name: 'History' }).scrollIntoViewIfNeeded()
await pause(1.5)
await click(page.getByRole('button', { name: 'Compare with live' }))
await pause(1)
await scroll(300)
await pause(4)

// Settings, and where each value came from.
mark('settings')
await click(nav('Agents'))
await click(page.getByRole('link', { name: /^Concierge/ }))
await click(page.getByRole('link', { name: 'Configure' }))
await point(page.getByRole('spinbutton', { name: 'Temperature' }))
await pause(1)
await scroll(-150, 10)
await pause(3.5)
// The same setting a level up, where the operator's default shows through.
await click(nav('Settings'))
await page.waitForURL(/\/settings/)
await page.waitForLoadState('networkidle')
await point(page.getByRole('spinbutton', { name: 'Temperature' }))
await pause(4)
mark('end')

const video = page.video()
await context.close()
await browser.close()
writeFileSync(`${OUT}/marks.json`, JSON.stringify({ video: await video.path(), marks }, null, 2))
console.log(JSON.stringify(marks))
