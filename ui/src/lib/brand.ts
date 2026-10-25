/**
 * Who this deployment says it is.
 *
 * outturn is Apache-2.0 and expects to be run by companies serving their own
 * customers under their own name. That makes the product name and the mark
 * configuration, not source: a deployer who has to edit JSX to change a
 * wordmark carries a patch on a hot file forever, and every upgrade is a
 * merge conflict on the one line they changed.
 *
 * Read at startup from `/config.json`, which the UI's own container writes
 * from its environment, so one published image serves every deployment. The
 * build environment is the fallback, which is what `npm run dev` sees: Vite
 * serves no such file. Rather than from the API, because these are properties
 * of the deployment rather than of the workspace being viewed -- the login
 * page needs them before anyone is signed in -- and because a logo or a theme
 * is a file, which has to be served from this origin anyway.
 *
 * main.tsx waits for `loadBrand` before the first render, so nothing is ever
 * drawn under the wrong name: a wordmark that arrives a frame late is a
 * wordmark that flickers. The bindings below are live, so a component reading
 * them at render time sees what was loaded.
 */

/** The product name, as a person reads it. */
export let productName = import.meta.env.VITE_BRAND_NAME ?? 'outturn'

/**
 * The mark. One constant rather than six copies of a path, so replacing it is
 * replacing a file and not finding every `<img>` that pointed at the old one.
 */
export let logoUrl = import.meta.env.VITE_BRAND_LOGO ?? '/favicon.svg'

/** Shown where the name needs a sentence around it, as on the login page. */
export let productTagline: string | null = import.meta.env.VITE_BRAND_TAGLINE ?? null

/** The theme compiled into this build, which needs no stylesheet loaded. */
const builtTheme = import.meta.env.VITE_THEME ?? 'outturn'

/** What `/config.json` may say. Every field optional; absent means the build's. */
export interface RuntimeBrand {
  name?: string
  logo?: string
  tagline?: string
  theme?: string
}

/**
 * How long a missing or hung config may hold up the page. It is a small file
 * from the same origin, so this is only ever reached when something is wrong,
 * and then the build's own branding is better than a blank page.
 */
const CONFIG_TIMEOUT_MS = 3000

/**
 * Where a theme's stylesheet is. A shipped theme is named, and the build
 * writes each one to `/themes/<name>.css`; a deployment's own is a path on
 * this origin -- the UI container serves a mounted directory at `/brand/`.
 * Anything else is refused: a stylesheet from another host is a request the
 * page makes on every load to somewhere nobody here chose.
 */
export function themeHref(theme: string): string | null {
  if (/^[a-z0-9-]+$/.test(theme)) return `/themes/${theme}.css`
  if (theme.startsWith('/') && !theme.startsWith('//')) return theme
  return null
}

/** Only same-origin paths and data URIs, which the page's CSP admits anyway. */
function sameOrigin(url: string): boolean {
  return (url.startsWith('/') && !url.startsWith('//')) || url.startsWith('data:')
}

/** Puts a loaded config into effect. Exported for the tests. */
export async function applyBrand(brand: RuntimeBrand, doc: Document = document) {
  if (brand.name) productName = brand.name
  if (brand.logo && sameOrigin(brand.logo)) logoUrl = brand.logo
  if (brand.tagline !== undefined) productTagline = brand.tagline || null

  doc.title = productName
  const icon = doc.querySelector<HTMLLinkElement>('link[rel="icon"]')
  if (icon && logoUrl !== '/favicon.svg') icon.href = logoUrl

  const href = brand.theme && brand.theme !== builtTheme ? themeHref(brand.theme) : null
  if (href) await loadStylesheet(href, doc)
}

/**
 * Waits for the stylesheet as well as adding it: rendering before it applies
 * is the one frame of the wrong colors this is all here to prevent. A sheet
 * that fails to load leaves the built-in theme, which is at least a whole one.
 */
function loadStylesheet(href: string, doc: Document): Promise<void> {
  return new Promise((resolve) => {
    const link = doc.createElement('link')
    link.rel = 'stylesheet'
    link.href = href
    link.onload = () => resolve()
    link.onerror = () => resolve()
    doc.head.appendChild(link)
  })
}

/**
 * Reads `/config.json` and applies it. Never rejects: any failure -- no file,
 * a dev server answering with index.html, malformed JSON, a timeout -- leaves
 * the build's branding, which is right for `npm run dev` and survivable
 * anywhere else.
 */
export async function loadBrand(fetchFn: typeof fetch = fetch, doc: Document = document) {
  let brand: RuntimeBrand = {}
  try {
    const res = await fetchFn('/config.json', {
      cache: 'no-cache',
      signal: AbortSignal.timeout(CONFIG_TIMEOUT_MS),
    })
    const type = res.headers.get('content-type') ?? ''
    if (res.ok && type.includes('json')) {
      const body: unknown = await res.json()
      if (body && typeof body === 'object') brand = pickStrings(body as Record<string, unknown>)
    }
  } catch {
    // The build's branding stands.
  }
  await applyBrand(brand, doc)
}

/** Keeps only string fields, so a typo'd config cannot put an object in the title. */
function pickStrings(body: Record<string, unknown>): RuntimeBrand {
  const out: RuntimeBrand = {}
  for (const key of ['name', 'logo', 'tagline', 'theme'] as const) {
    if (typeof body[key] === 'string') out[key] = body[key]
  }
  return out
}
