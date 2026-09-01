import vue from '@vitejs/plugin-vue'
import { fileURLToPath } from 'node:url'
import { defineConfig, type Plugin } from 'vitest/config'

// `@ritornello/ui` is **external** in production (provided by the shell's
// import map): never transformed. Under Vitest it is not external: the kit's
// barrel is really transformed, and its internal imports (`@/lib/utils`,
// etc.) then resolve through the '@' alias, which designates `web/kit/src` in
// the kit's workshop. This module has no '@' of its own (see src/index.ts and
// src/MpdAdmin.vue): so no global alias was needed -- just redirect `@/...`
// to `web/kit/src` for the files imported from the kit only. Same approach as
// `crates/ritornello-plugin-radio/ui/vitest.config.ts`.
const kitSrc = fileURLToPath(new URL('../../../web/kit/src', import.meta.url))
const kitSrcPrefix = `${kitSrc.replaceAll('\\', '/').replace(/\/$/, '')}/`

function kitAlias(): Plugin {
  return {
    name: 'ritornello-kit-alias',
    resolveId(source, importer) {
      if (!source.startsWith('@/') || !importer) return null
      const normalizedImporter = importer.replaceAll('\\', '/')
      // `node_modules` (including our own packages, symlinked by npm
      // workspaces) is never concerned: only the kit's source files are
      // entitled to it. Without this guard, an '@/...' import coming from
      // elsewhere (e.g. another dependency) would be wrongly claimed.
      if (normalizedImporter.includes('/node_modules/')) return null
      if (!normalizedImporter.startsWith(kitSrcPrefix)) return null
      return this.resolve(source.replace('@/', `${kitSrc}/`), importer, { skipSelf: true })
    },
  }
}

export default defineConfig({
  plugins: [vue(), kitAlias()],
  test: { environment: 'jsdom', globals: true },
})
