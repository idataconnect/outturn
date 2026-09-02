import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { defineConfig } from 'vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    port: 3000,
    proxy: {
      // Ports match skaffold's portForward block. The 18xxx range avoids
      // the heavily contended 8080/8081; override if skaffold reassigns:
      // VITE_API_PORT=... npm run dev
      '/api': {
        target: `http://localhost:${process.env.VITE_API_PORT ?? 18080}`,
        rewrite: (path) => path.replace(/^\/api/, ''),
      },
      '/gateway': {
        target: `http://localhost:${process.env.VITE_GATEWAY_PORT ?? 18081}`,
        rewrite: (path) => path.replace(/^\/gateway/, ''),
      },
    },
  },
})
