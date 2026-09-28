// The approval loop, end to end, in a real browser against a running cluster.
//
// Not part of `npm test` -- it needs the cluster from `scripts/dev.sh`, a live
// model, and several minutes. Run it by hand when the approval path changes:
//
//     cd ui && node e2e/approvals.mjs
//
// Two scenarios, because the interesting half is who may answer:
//   A. A clerk triggers the gate and cannot approve; an admin answers, and the
//      clerk's conversation carries on by itself.
//   B. The admin triggers it and approves inline, in the same chat.
//
// It exists because the parts that broke were never the ones a unit test
// watches: a held banner that only ever arrived on a live event, an Approve
// button offered to somebody who could not use it, a column absent from three
// query lists, and a resumed turn that overwrote the refusal it was approved
// against. Each of those passed every test in the repo.
//
// It assumes the `Front desk` agent and the `desk` role from the Hollowbrook
// walkthrough, and books a room before charging it -- asking for both in one
// message makes the resumed turn re-book, find the room taken by its own
// booking, and stop before the charge.

import { chromium } from '@playwright/test'

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

  // Somebody in more than one workspace picks which, and the session is minted
  // for it on the click. Polled through the API rather than by watching the URL,
  // which does not change: a read issued before the session lands comes back
  // empty rather than failing, and an empty queue reads as "nobody was asked" --
  // which is exactly how this looked like a product bug twice.
  const pick = page.getByRole('button', { name: /acme/i })
  if ((await pick.count()) > 0) {
    await pick.first().click()
    for (let i = 0; i < 30; i += 1) {
      await page.waitForTimeout(1000)
      const ok = await page.evaluate(async () => {
        const r = await fetch('/v1/session', { credentials: 'include' })
        if (!r.ok) return false
        const s = await r.json()
        return Boolean(s?.workspace_id)
      })
      if (ok) return
    }
    throw new Error('the session never settled on a workspace')
  }
}


/** Starts a fresh conversation with the guesthouse agent.
 *
 * Through the sidebar button rather than by posting to the API: the point of
 * driving a browser is that everything between the click and the reply is
 * exercised, and a session created behind the UI's back skips the half that
 * decides whether the held banner ever appears.
 */
async function openHollowbrook(page) {
  await page.goto(`${UI}/sessions`, { waitUntil: 'domcontentloaded' })

  // The landing redirects to whichever conversation was last open, so the
  // button is not there the instant the page is. Waited for rather than slept
  // past: a fixed pause is the thing that made this fail once already.
  //
  // And the sidebar listing the agents can be put away at any width -- once it
  // is, the button is not merely late but absent, which is what made the second
  // scenario time out looking for it.
  const reveal = page.getByRole('button', { name: /show sessions/i })
  if ((await reveal.count()) > 0) await reveal.first().click()

  const start = page.getByRole('button', { name: /^Front desk$/i })
  await start.first().waitFor({ state: 'visible', timeout: 30_000 })

  const before = page.url()
  await start.first().click()
  await page.waitForFunction((was) => window.location.href !== was, before, {
    timeout: 30_000,
  })
  await page.waitForTimeout(1500)
}

async function send(page, text) {
  const box = page.getByRole('textbox').first()
  await box.click()
  await box.fill(text)
  await box.press('Enter')
}

/** Waits for a turn to produce the text it is going to produce.
 *
 * Keyed on the page settling rather than on the composer, which re-enables
 * between turns and so reported "finished" while the reply was still an empty
 * placeholder -- the agent had said nothing yet and there was no booking id to
 * read.
 */
async function waitForIdle(page, timeout = 900_000) {
  const until = Date.now() + timeout
  let last = ''
  let stableFor = 0
  while (Date.now() < until) {
    await page.waitForTimeout(5000)
    const text = await page.locator('body').innerText()
    if (text === last && text.length > 0) {
      stableFor += 5
      // Quiet for half a minute with something on screen: the turn is done.
      if (stableFor >= 30) return true
    } else {
      stableFor = 0
      last = text
    }
  }
  return false
}

/** Books a room and returns its id, in a turn of its own.
 *
 * Separate from the charge on purpose. Asking for both in one message is what
 * made every earlier run fail: the booking succeeds, the charge is gated, the
 * turn parks -- and on resume the agent starts over, tries to book the same
 * room again, finds it taken by its own booking seconds earlier, and stops
 * before it ever reaches the charge. With the booking already made, the resumed
 * turn has exactly one thing left to do.
 */
async function bookARoom(page, room, date, guest) {
  await send(page, `Book the ${room} for one night from ${date} for ${guest}. Just the booking.`)
  await waitForIdle(page)
  const text = await page.locator('body').innerText()
  const found = text.match(/bk_[A-Za-z0-9]+/)
  return found ? found[0] : null
}

/** Waits for the conversation to say it is held, and returns the banner text.
 *
 * The banner is the whole point of the held event reaching the browser: without
 * it the reader sees a red tool error and a working composer, which reads as a
 * conversation that finished rather than one that is paused.
 */
async function waitForHeld(page, timeout = 600_000) {
  const banner = page.getByRole('status').filter({ hasText: /approv|pause|hold/i })
  try {
    await banner.first().waitFor({ state: 'visible', timeout })
    return await banner.first().innerText()
  } catch {
    return null
  }
}

async function main() {
  const browser = await chromium.launch()
  const secrets = (await import('node:child_process')).execSync(
    'scripts/dev-secrets.sh --print',
    { cwd: '/Users/b/src/outturn', encoding: 'utf8' },
  )
  const adminPass = secrets.match(/PASSWORD=(.+)/)[1].trim()

  // ---------------------------------------------------------------- A
  log('\n=== A. clerk triggers it, admin approves ===')
  const clerk = await browser.newPage({ viewport: { width: 1280, height: 900 } })
  const clerkErrors = []
  clerk.on('pageerror', (e) => clerkErrors.push(String(e)))

  await signIn(clerk, 'clerk@hollowbrook.local', adminPass)
  pass('the clerk signed in')
  await openHollowbrook(clerk)

  const bookingA = await bookARoom(clerk, 'orchard room', '2027-05-04', 'J. Okafor')
  if (!bookingA) {
    fail('the clerk\'s agent did not make a booking to charge')
    await browser.close()
    process.exit(1)
  }
  pass(`the clerk's agent booked ${bookingA}`)

  await send(clerk, `Now charge booking ${bookingA} to payment account pa_4471 for 9000 pence.`)
  log('  -> asked the clerk\'s agent to charge it')

  const held = await waitForHeld(clerk)
  if (!held) {
    fail('the clerk\'s conversation never said it was held')
  } else {
    pass(`held: ${held.replace(/\s+/g, ' ').slice(0, 120)}`)
  }

  await clerk.screenshot({ path: '.shots/hitl-clerk-held.png', fullPage: true })

  // The clerk must not be offered the buttons.
  const clerkApprove = clerk.getByRole('button', { name: /^Approve$/ })
  if ((await clerkApprove.count()) > 0) {
    fail('the clerk is offered an Approve button they may not use')
  } else {
    pass('the clerk is not offered Approve')
  }

  // What the clerk's page says before anybody answers, so "it resumed" can be
  // judged against a change rather than against a regex that may already match.
  const beforeAnswer = await clerk.locator('body').innerText()

  // The admin answers, in their own browser context.
  const admin = await browser.newPage({ viewport: { width: 1280, height: 900 } })
  await signIn(admin, 'admin@outturn.local', adminPass)
  pass('the admin signed in')

  const queue = await admin.evaluate(async () => {
    // Same origin: the session cookie belongs to the UI, and the dev server
    // proxies /v1 to the API. A fetch straight at :18080 carries no cookie and
    // silently comes back empty, which reads as "nobody was asked".
    const r = await fetch('/v1/action-items', { credentials: 'include' })
    return r.ok ? await r.json() : { items: [], status: r.status }
  })
  const item = (queue.items || []).find((i) => String(i.kind).startsWith('approval.'))
  if (!item) {
    fail('the admin has no approval waiting')
  } else {
    pass(`the admin was asked: ${item.kind}`)
    const answered = await admin.evaluate(
      async ({ id }) => {
        const r = await fetch(`/v1/approvals/${id}/answer`, {
          method: 'POST',
          credentials: 'include',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ approved: true, note: 'checked with the guest' }),
        })
        return { status: r.status, body: await r.text() }
      },
      { id: item.id },
    )
    if (answered.status === 200) {
      pass(`answered: ${answered.body}`)
    } else {
      fail(`the admin could not answer: ${answered.status} ${answered.body}`)
    }
  }

  // And the clerk's conversation carries on by itself -- no reload.
  //
  // Judged by the page *changing*, against what it said before the approval.
  // A regex over the whole body passed once on text that was already there
  // before anybody answered, and two byte-identical screenshots were the only
  // thing that gave it away.
  log('  -> waiting for the clerk\'s turn to resume')
  let resumed = false
  for (let i = 0; i < 60; i += 1) {
    await clerk.waitForTimeout(5000)
    // The banner going is the claim: it is rendered from the held event and
    // cleared when the turn runs again. A page that merely *changed* is
    // streaming settling, and that check passed twice on nothing.
    const stillHeld = await clerk
      .getByRole('status')
      .filter({ hasText: /approv|pause|hold/i })
      .count()
    if (stillHeld === 0 && (await clerk.locator('body').innerText()) !== beforeAnswer) {
      resumed = true
      log(`  -> the hold lifted after ${(i + 1) * 5}s`)
      break
    }
  }
  await clerk.screenshot({ path: '.shots/hitl-clerk-resumed.png', fullPage: true })
  if (resumed) {
    pass('the clerk\'s conversation carried on after the approval')
  } else {
    fail('the clerk\'s conversation did not visibly resume')
  }

  // The refused attempt must still be there. Before attempts existed the
  // resumed turn took its reply back and streamed over it, so the refusal the
  // reader approved against was gone and the APPROVED badge pointed at nothing.
  let afterText = await clerk.locator('body').innerText()
  for (let i = 0; i < 24 && !/APPROVED/.test(afterText); i += 1) {
    await clerk.waitForTimeout(5000)
    afterText = await clerk.locator('body').innerText()
  }
  if (/needs approval before it can go out/i.test(afterText)) {
    pass('the refusal that was approved is still in the transcript')
  } else {
    fail('the refused attempt was overwritten by the resumed turn')
  }
  if (/APPROVED/.test(afterText)) {
    pass('the approval is recorded in the transcript')
  } else {
    fail('nothing in the transcript says it was approved')
  }

  if (clerkErrors.length) fail(`console errors: ${clerkErrors.join(' | ')}`)
  else pass('no page errors in the clerk\'s browser')

  // ---------------------------------------------------------------- B
  log('\n=== B. admin triggers it and approves inline ===')
  await openHollowbrook(admin)
  const bookingB = await bookARoom(admin, 'garden room', '2027-05-09', 'R. Sandoval')
  if (!bookingB) {
    fail('the admin\'s agent did not make a booking to charge')
    log(`\n${failures} FAILED`)
    await browser.close()
    process.exit(1)
  }
  pass(`the admin's agent booked ${bookingB}`)

  await send(admin, `Now charge booking ${bookingB} to payment account pa_8802 for 4500 pence.`)
  log('  -> asked the admin\'s agent to charge it')

  const held2 = await waitForHeld(admin)
  if (!held2) {
    fail('the admin\'s conversation never said it was held')
  } else {
    pass(`held: ${held2.replace(/\s+/g, ' ').slice(0, 120)}`)
  }

  await admin.screenshot({ path: '.shots/hitl-admin-held.png', fullPage: true })

  const approve = admin.getByRole('button', { name: /^Approve$/ })
  if ((await approve.count()) === 0) {
    fail('the admin is not offered Approve in the conversation')
  } else {
    pass('the admin can approve without leaving the chat')
    const beforeClick = await admin.locator('body').innerText()
    await approve.first().click()
    log('  -> clicked Approve')

    let done = false
    for (let i = 0; i < 48; i += 1) {
      await admin.waitForTimeout(5000)
      const text = await admin.locator('body').innerText()
      if (text !== beforeClick) {
        done = true
        log(`  -> the page changed after ${(i + 1) * 5}s`)
        break
      }
    }
    await admin.screenshot({ path: '.shots/hitl-admin-resumed.png', fullPage: true })
    if (done) pass('the charge went through after approving inline')
    else fail('the conversation did not visibly resume after approving inline')
  }

  // The claim the whole feature exists to make: an approved charge actually
  // reached the guesthouse. Asked of the guesthouse rather than inferred from
  // the page, because a page that changed is not a charge that went out -- that
  // inference passed twice on nothing.
  const { execSync: run } = await import('node:child_process')
  // Asked of the guesthouse, not of its logs. It logs only startup, so the
  // grep this used to do could never have matched -- and reported "no charge
  // ever reached the guesthouse" in every run while charges were going through.
  const charges = run(
    "kubectl run hbcheck --rm -i --restart=Never --image=curlimages/curl:latest " +
      "-- -s http://outturn-hollowbrook:8084/charges 2>/dev/null",
    { encoding: 'utf8' },
  )
  const taken = (charges.match(/"id":"ch_/g) || []).length
  if (taken > 0) {
    pass(`${taken} charge(s) reached the guesthouse`)
  } else {
    fail('no charge ever reached the guesthouse')
  }

  log(`\n${failures === 0 ? 'ALL PASSED' : `${failures} FAILED`}`)
  await browser.close()
  process.exit(failures ? 1 : 0)
}

main().catch((e) => {
  console.error(e)
  process.exit(1)
})
