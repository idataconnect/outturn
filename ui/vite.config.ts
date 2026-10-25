/// <reference types="vitest/config" />
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { readFileSync, readdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { defineConfig, type Plugin } from 'vite'
import { plainThemeCss } from './src/themes/plain.ts'

// Which brand's tokens get compiled in: a file in src/themes/, outturn's own
// look unless VITE_THEME says otherwise. The published image keeps the
// default and lets a deployment choose another at startup, from the files
// themeStylesheets() writes below.
const theme = process.env.VITE_THEME ?? 'outturn'

const themesDir = fileURLToPath(new URL('./src/themes/', import.meta.url))

// Every theme, also written out as a stylesheet of its own at
// /themes/<name>.css, so a deployment can pick one at startup from the UI
// container's config rather than by rebuilding. See src/lib/brand.ts.
function themeStylesheets(): Plugin {
  return {
    name: 'outturn-theme-stylesheets',
    apply: 'build',
    generateBundle() {
      for (const file of readdirSync(themesDir).filter((f) => f.endsWith('.css'))) {
        this.emitFile({
          type: 'asset',
          fileName: `themes/${file}`,
          source: plainThemeCss(readFileSync(themesDir + file, 'utf8')),
        })
      }
    },
  }
}

export default defineConfig({
  plugins: [react(), tailwindcss(), themeStylesheets()],
  resolve: {
    alias: {
      // `index.css` imports this name; the alias decides what it means.
      'virtual:theme.css': fileURLToPath(
        new URL(`./src/themes/${theme}.css`, import.meta.url),
      ),
    },
  },
  test: {
    // Components reach for the DOM, so the tests need one.
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    globals: true,
    css: false,
  },
  server: {
    port: 3000,
    proxy: {
      // Ports match skaffold's portForward block. The 18xxx range avoids
      // the heavily contended 8080/8081; override if skaffold reassigns:
      // VITE_API_PORT=... npm run dev
      // Proxied at /v1 rather than under a /api prefix that is rewritten
      // away. The refresh cookie is Path-scoped to /v1/session/refresh, and a
      // browser matches that against the URL it requests, not the one the
      // proxy forwards -- so a prefix here means the cookie is never sent and
      // a session can never be refreshed. Dev has to use the same paths as
      // production for path-scoped cookies to behave the same.
      '/v1': {
        target: `http://localhost:${process.env.VITE_API_PORT ?? 18080}`,
      },
      '/gateway': {
        target: `http://localhost:${process.env.VITE_GATEWAY_PORT ?? 18081}`,
        rewrite: (path) => path.replace(/^\/gateway/, ''),
      },
    },
  },
})
