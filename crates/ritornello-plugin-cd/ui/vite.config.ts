import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

// `vue` and `@ritornello/ui` are **external**: they are provided by the
// shell's import map. The module therefore weighs only its own logic, and
// shares the page's single Vue instance.
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
