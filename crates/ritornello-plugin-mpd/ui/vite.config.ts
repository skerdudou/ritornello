import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

// `vue` et `@ritornello/ui` sont **externes** : ils sont fournis par l'import
// map du shell. Le module ne pese donc que sa propre logique, et partage
// l'unique instance de Vue de la page.
export default defineConfig({
  plugins: [vue(), tailwindcss()],
  build: {
    lib: { entry: 'src/index.ts', formats: ['es'], fileName: () => 'ui.js' },
    rollupOptions: {
      external: ['vue', '@ritornello/ui'],
      output: { assetFileNames: 'ui.css' },
    },
    cssCodeSplit: false,
    emptyOutDir: true,
  },
})
