/// <reference types="vite/client" />

interface ImportMetaEnv {
  /**
   * The product name a deployment shows. Defaults to "outturn". These four are
   * the fallback for what the UI container's `/config.json` says at startup;
   * see src/lib/brand.ts.
   */
  readonly VITE_BRAND_NAME?: string
  /** Path to the mark, served from `public/`. Defaults to "/favicon.svg". */
  readonly VITE_BRAND_LOGO?: string
  /** An optional line under the wordmark on the login page. */
  readonly VITE_BRAND_TAGLINE?: string
  readonly VITE_DOCS_URL?: string
  /** Which file in `src/themes/` sets the color tokens. Defaults to "outturn". */
  readonly VITE_THEME?: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}
