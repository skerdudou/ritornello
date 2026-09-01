import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { copyFileSync } from 'node:fs'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig, type Plugin } from 'vite'

const IMPORT_MAP = `<script type="importmap">
      {"imports":{"vue":"/assets/vue.js","@ritornello/ui":"/assets/ui-kit.js"}}
    </script>`

// Injects the import map into the shell and copies the kit bundle next to
// the app's assets, so the core only has a single directory to embed.
function shellPlugin(): Plugin {
  return {
    name: 'ritornello-shell',
    transformIndexHtml(html) {
      // A `replace` on a missing marker is a silent no-op: without this
      // guard, an `index.html` modified by mistake would produce a
      // `dist/` without the import map — hence a 404 on every `vue`
      // import — with a `vite build` that stays green.
      if (!html.includes('<!--IMPORTMAP-->')) {
        throw new Error('IMPORTMAP marker missing from index.html')
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
    // `vue` and the kit are provided by the import map: externalizing them
    // here too is what guarantees the uniqueness of the Vue instance.
    rollupOptions: {
      external: ['vue', '@ritornello/ui'],
      // The three keys are fixed together: the `app-` prefix marks
      // everything that is hashed (entry, lazy route chunks, style
      // sheets), hence immutable by construction, as opposed to `vue.js`
      // and `ui-kit.js` which keep a stable name — the URLs of the
      // plugin contract — and must remain revalidatable. The Rust core
      // (Task 6) will derive its cache policy from this single prefix.
      output: {
        entryFileNames: 'assets/app-[hash].js',
        chunkFileNames: 'assets/app-[hash].js',
        assetFileNames: 'assets/app-[hash][extname]',
      },
    },
    emptyOutDir: true,
  },
})
