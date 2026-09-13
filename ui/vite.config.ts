/// <reference types="vitest/config" />
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vite'

// Which brand's tokens get compiled in. A deployment sets VITE_THEME to a
// file in src/themes/; the default is outturn's own look. Resolved at build
// time rather than fetched, so there is no frame where the page is the wrong
// colour, and no theme a deployment is not using shipped in its bundle.
const theme = process.env.VITE_THEME ?? 'outturn'

export default defineConfig({
  plugins: [react(), tailwindcss()],
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
