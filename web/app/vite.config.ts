import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { copyFileSync } from 'node:fs'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig, type Plugin } from 'vite'

const IMPORT_MAP = `<script type="importmap">
      {"imports":{"vue":"/assets/vue.js","@ritornello/ui":"/assets/ui-kit.js"}}
    </script>`

// Injecte l'import map dans le shell et recopie le bundle du kit a cote des
// actifs de l'app, pour que le coeur n'ait qu'un seul repertoire a embarquer.
function shellPlugin(): Plugin {
  return {
    name: 'ritornello-shell',
    transformIndexHtml(html) {
      // Un `replace` sur un marqueur absent est un no-op silencieux : sans
      // ce garde-fou, un `index.html` modifie par erreur produirait un
      // `dist/` sans import map — donc un 404 sur chaque import de `vue` —
      // avec un `vite build` qui reste vert.
      if (!html.includes('<!--IMPORTMAP-->')) {
        throw new Error('marqueur IMPORTMAP absent de index.html')
      }
      return html.replace('<!--IMPORTMAP-->', IMPORT_MAP)
    },
    closeBundle() {
      copyFileSync(
        fileURLToPath(new URL('../kit/dist/ui-kit.js', import.meta.url)),
        fileURLToPath(new URL('./dist/assets/ui-kit.js', import.meta.url)),
      )
    },
  }
}

export default defineConfig({
  plugins: [vue(), tailwindcss(), shellPlugin()],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  build: {
    // `vue` et le kit sont fournis par l'import map : les externaliser ici
    // aussi est ce qui garantit l'unicite de l'instance de Vue.
    rollupOptions: {
      external: ['vue', '@ritornello/ui'],
      // Les trois cles sont fixees ensemble : le prefixe `app-` marque tout
      // ce qui est hashe (entree, chunks de route paresseux, feuilles de
      // style), donc immuable par construction, par opposition a `vue.js`
      // et `ui-kit.js` qui gardent un nom stable — les URL du contract des
      // plugins — et doivent rester revalidables. Le coeur Rust (Task 6)
      // deduira sa politique de cache de ce seul prefixe.
      output: {
        entryFileNames: 'assets/app-[hash].js',
        chunkFileNames: 'assets/app-[hash].js',
        assetFileNames: 'assets/app-[hash][extname]',
      },
    },
    emptyOutDir: true,
  },
})
