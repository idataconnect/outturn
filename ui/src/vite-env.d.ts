/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** The product name a deployment shows. Defaults to "outturn". */
  readonly VITE_BRAND_NAME?: string
  /** Path to the mark, served from `public/`. Defaults to "/favicon.svg". */
  readonly VITE_BRAND_LOGO?: string
  /** An optional line under the wordmark on the login page. */
  readonly VITE_BRAND_TAGLINE?: string
  /** Which file in `src/themes/` sets the colour tokens. Defaults to "outturn". */
  readonly VITE_THEME?: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}
