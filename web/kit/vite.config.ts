import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'

// Build en bibliotheque ESM. `vue` est **externe** : il est fourni par
// l'import map du shell, pour qu'une seule instance serve le shell et tous
// les modules de plugin. Le nom de sortie est **stable** (pas de hash) :
// c'est l'URL que l'import map designe et contre laquelle les plugins sont
// compiles.
export default defineConfig({
  plugins: [vue()],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  // Le mode `build.lib` ne substitue pas `process.env.NODE_ENV` (il suppose
  // un bundler en aval). Sans ce `define`, `ui-kit.js` garde une reference a
  // `process.env.NODE_ENV` dans le `setup()` de `DialogContent` (reka-ui) :
  // le fichier se charge (pas de reference au niveau module), mais la
  // premiere popin montee leve une `ReferenceError` a l'execution.
  define: { 'process.env.NODE_ENV': JSON.stringify('production') },
  build: {
    lib: { entry: 'src/index.ts', formats: ['es'], fileName: () => 'ui-kit.js' },
    rollupOptions: { external: ['vue'] },
    cssCodeSplit: false,
    emptyOutDir: true,
  },
})
