import { beforeEach, describe, expect, it, vi } from 'vitest'

// The bindings are module state, so each test gets a fresh module.
async function fresh() {
  vi.resetModules()
  return import('./brand')
}

function respond(body: string, type = 'application/json', status = 200): typeof fetch {
  return vi.fn(async () => new Response(body, { status, headers: { 'content-type': type } }))
}

beforeEach(() => {
  document.head.innerHTML = '<link rel="icon" href="/favicon.svg" />'
  document.title = 'outturn'
})

describe('loadBrand', () => {
  it('takes the name, logo and tagline the container says', async () => {
    const brand = await fresh()
    await brand.loadBrand(
      respond(JSON.stringify({ name: 'Acme Desk', logo: '/brand/acme.svg', tagline: 'Hello' })),
    )
    expect(brand.productName).toBe('Acme Desk')
    expect(brand.logoUrl).toBe('/brand/acme.svg')
    expect(brand.productTagline).toBe('Hello')
    expect(document.title).toBe('Acme Desk')
    expect(document.querySelector<HTMLLinkElement>('link[rel="icon"]')!.getAttribute('href')).toBe(
      '/brand/acme.svg',
    )
  })

  // What the container writes for a variable nobody set.
  it('treats empty fields as unset', async () => {
    const brand = await fresh()
    await brand.loadBrand(respond(JSON.stringify({ name: '', logo: '', tagline: '', theme: '' })))
    expect(brand.productName).toBe('outturn')
    expect(brand.logoUrl).toBe('/favicon.svg')
    expect(brand.productTagline).toBeNull()
    expect(document.querySelectorAll('link[rel="stylesheet"]')).toHaveLength(0)
  })

  // `npm run dev`: Vite answers an unknown path with index.html.
  it('keeps the build branding when the answer is not JSON', async () => {
    const brand = await fresh()
    await brand.loadBrand(respond('<!doctype html>', 'text/html'))
    expect(brand.productName).toBe('outturn')
  })

  it('keeps the build branding when the fetch fails, and says so', async () => {
    const brand = await fresh()
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    await brand.loadBrand(vi.fn(async () => Promise.reject(new TypeError('offline'))))
    expect(brand.productName).toBe('outturn')
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('/config.json'), expect.anything())
    warn.mockRestore()
  })

  it('refuses a logo from another host, and says so', async () => {
    const brand = await fresh()
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    await brand.loadBrand(respond(JSON.stringify({ logo: 'https://example.com/x.svg' })))
    expect(brand.logoUrl).toBe('/favicon.svg')
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('logo'))
    warn.mockRestore()
  })

  it('refuses a theme from another host, and says so', async () => {
    const brand = await fresh()
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    await brand.loadBrand(respond(JSON.stringify({ theme: 'https://example.com/x.css' })))
    expect(document.querySelectorAll('link[rel="stylesheet"]')).toHaveLength(0)
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('theme'))
    warn.mockRestore()
  })

  it('ignores fields that are not strings', async () => {
    const brand = await fresh()
    await brand.loadBrand(respond(JSON.stringify({ name: { evil: true } })))
    expect(brand.productName).toBe('outturn')
  })

  // The first render waits on this promise, so it must not settle until the
  // theme's stylesheet has: that is the frame of wrong colors prevented.
  it('waits for a chosen theme to load before resolving', async () => {
    const brand = await fresh()
    let settled = false
    const done = brand
      .loadBrand(respond(JSON.stringify({ theme: 'hollowbrook' })))
      .then(() => (settled = true))
    await vi.waitFor(() => expect(document.querySelector('link[rel="stylesheet"]')).not.toBeNull())
    const link = document.querySelector<HTMLLinkElement>('link[rel="stylesheet"]')!
    expect(link.getAttribute('href')).toBe('/themes/hollowbrook.css')
    expect(settled).toBe(false)
    link.dispatchEvent(new Event('load'))
    await done
    expect(settled).toBe(true)
  })

  // main.tsx renders only after this settles, so a sheet that never loads
  // must not mean a page that never draws.
  it('gives up on a theme still loading at the deadline, and removes it', async () => {
    const brand = await fresh()
    const deadline = new AbortController()
    const done = brand.loadBrand(
      respond(JSON.stringify({ name: 'Acme Desk', theme: 'hollowbrook' })),
      document,
      deadline.signal,
    )
    await vi.waitFor(() => expect(document.querySelector('link[rel="stylesheet"]')).not.toBeNull())
    deadline.abort()
    await done
    expect(document.querySelectorAll('link[rel="stylesheet"]')).toHaveLength(0)
    expect(brand.productName).toBe('Acme Desk')
  })

  it('loads nothing for the theme already compiled in', async () => {
    const brand = await fresh()
    await brand.loadBrand(respond(JSON.stringify({ theme: 'outturn' })))
    expect(document.querySelectorAll('link[rel="stylesheet"]')).toHaveLength(0)
  })
})

describe('themeHref', () => {
  it('maps names to the shipped files and admits same-origin paths only', async () => {
    const { themeHref } = await fresh()
    expect(themeHref('hollowbrook')).toBe('/themes/hollowbrook.css')
    expect(themeHref('/brand/acme.css')).toBe('/brand/acme.css')
    expect(themeHref('//example.com/x.css')).toBeNull()
    expect(themeHref('https://example.com/x.css')).toBeNull()
    expect(themeHref('../etc')).toBeNull()
  })
})
