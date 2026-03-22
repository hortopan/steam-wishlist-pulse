import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import { imagetools } from 'vite-imagetools'

export default defineConfig({
  plugins: [svelte(), imagetools()],
  server: {
    proxy: {
      '/api': 'http://localhost:3000',
    },
  },
})
