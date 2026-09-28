// A thinking model's reasoning, end to end, in a real browser.
//
// Not part of `npm test` -- it needs the cluster from `scripts/dev.sh` and a
// live model that thinks (qwen3.8 does). Run it by hand:
//
//     cd ui && node e2e/reasoning.mjs
//
// It exists because of a failure no unit test could have caught: ollama emits
// qwen3's thinking on a `reasoning` field with `content` set to "" on every
// thinking chunk, and the runtime read `content` alone. A turn the model spent
// entirely on thinking therefore streamed nothing, stored an empty reply, and
// finished with `chat.done` and no deltas -- while the usage ledger recorded the
// 170 completion tokens it had cost. On screen that was a question that had
// silently gone unanswered.
//
// So what is checked here is the wire fact, not the rendering: that a real model
// through a real gateway produces `chat.reasoning` events at all, and that they
// never reach the stored reply.

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

async function openHollowbrook(page) {
  await page.goto(`${UI}/sessions`, { waitUntil: 'domcontentloaded' })
  const reveal = page.getByRole('button', { name: /show sessions/i })
  if ((await reveal.count()) > 0) await reveal.first().click()
  const start = page.getByRole('button', { name: /^Front desk$/i })
  await start.first().waitFor({ state: 'visible', timeout: 30_000 })
  const before = page.url()
  await start.first().click()
  await page.waitForFunction((was) => window.location.href !== was, before, { timeout: 30_000 })
  await page.waitForTimeout(1500)
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
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } })
  const errors = []
  page.on('pageerror', (e) => errors.push(String(e)))

  log('\n=== a thinking model answering an ordinary question ===')
  await signIn(page, 'admin@outturn.local', adminPass)
  await openHollowbrook(page)

  const url = page.url()
  const session = url.slice(url.lastIndexOf('/') + 1)
  log(`  session ${session}`)

  const box = page.getByRole('textbox').first()
  await box.click()
  // Something that invites deliberation without needing a tool, so the turn is
  // thinking and prose and nothing else.
  await box.fill('A guest asks whether the Orchard Room suits a family of four. Think it through, then answer.')
  await box.press('Enter')
  await waitForIdle(page)

  // ---- the wire fact
  const events = Number(
    psql(`select count(*) from events where session_id='${session}' and kind='chat.reasoning'`),
  )
  if (events > 0) pass(`${events} chat.reasoning events reached the API`)
  else fail('no chat.reasoning event was ever produced -- thinking is still being dropped')

  const blocks = psql(
    `select count(*) from agent_messages m,
       jsonb_array_elements(coalesce(m.metadata->'parts', '[]'::jsonb)) p
     where m.session_id='${session}' and m.role='assistant' and p->>'type'='reasoning'`,
  )
  if (Number(blocks) > 0) pass(`the reply stored ${blocks} block(s) of thinking, in place`)
  else fail('the reply stored no thinking')

  // The invariant that matters most: thinking is not the reply. If it leaked
  // into `content` it would be replayed to the next turn as something the agent
  // said, and the agent would answer its own deliberation.
  const leaked = psql(
    `select content from agent_messages
     where session_id='${session}' and role='assistant' order by id desc limit 1`,
  )
  const thinking = psql(
    `select string_agg(p->>'text', '' order by ord) from agent_messages m,
       jsonb_array_elements(coalesce(m.metadata->'parts', '[]'::jsonb)) with ordinality t(p, ord)
     where m.session_id='${session}' and m.role='assistant' and p->>'type'='reasoning'`,
  )
  const firstWords = thinking.split(/\s+/).slice(0, 6).join(' ')
  if (firstWords && leaked.includes(firstWords)) {
    fail('thinking leaked into the stored reply')
  } else {
    pass('the stored reply holds no thinking')
  }

  // Thinking is stored in `parts`, which is what the projection walks to build
  // a model's history -- so the one thing that must hold is that it is dropped
  // on the way out. A turn after this one proves it: if reasoning went back,
  // the agent would be answering its own deliberation.
  const inParts = psql(
    `select count(*) from agent_messages m,
       jsonb_array_elements(coalesce(m.metadata->'parts', '[]'::jsonb)) p
     where m.session_id='${session}' and p->>'type' = 'text'
       and p->>'text' like '%${thinking.slice(0, 30).replace(/'/g, "''")}%'`,
  )
  if (Number(inParts) === 0) pass('no thinking is stored as a text part')
  else fail('thinking leaked into a text part, which is what reaches the model')

  // ---- and that a reader can actually get at it
  const toggle = page.getByRole('button', { name: /thought about this|thinking/i })
  if ((await toggle.count()) > 0) {
    pass('the reply offers the thinking behind a disclosure')
    await toggle.first().click()
    await page.waitForTimeout(500)
    const shown = await page.locator('body').innerText()
    if (shown.includes(thinking.slice(0, 40))) pass('opening it shows what the model thought')
    else fail('the disclosure opened onto something other than the thinking')
  } else {
    fail('no reasoning disclosure was drawn')
  }

  await page.screenshot({ path: '/tmp/reasoning.png', fullPage: true })
  log('  screenshot: /tmp/reasoning.png')

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
