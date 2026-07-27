import vue from '@vitejs/plugin-vue'
import { fileURLToPath } from 'node:url'
import { defineConfig, type Plugin } from 'vitest/config'

// `@ritornello/ui` est **externe** en production (fourni par l'import map du
// shell) : jamais transforme. Sous Vitest il ne l'est pas : le barrel du kit
// est reellement transforme, et ses imports internes (`@/lib/utils`, etc.)
// se resolvent alors avec l'alias '@', qui designe `web/kit/src` dans
// l'atelier du kit. Ce module-ci n'a pas de '@' a lui (voir src/index.ts et
// src/InputAdmin.vue) : il ne fallait donc pas d'alias global -- juste
// rediriger `@/...` vers `web/kit/src` pour les seuls fichiers importes
// depuis le kit. Meme approche que `crates/ritornello-plugin-radio/ui/vitest.config.ts`.
const kitSrc = fileURLToPath(new URL('../../../web/kit/src', import.meta.url))
const kitSrcPrefixe = `${kitSrc.replaceAll('\\', '/').replace(/\/$/, '')}/`

function kitAlias(): Plugin {
  return {
    name: 'ritornello-kit-alias',
    resolveId(source, importer) {
      if (!source.startsWith('@/') || !importer) return null
      const importeurNormalise = importer.replaceAll('\\', '/')
      // `node_modules` (y compris nos propres paquets, symlinkes par npm
      // workspaces) n'est jamais concerne : seuls les fichiers sources du
      // kit y ont droit. Sans ce garde, un import '@/...' venu d'ailleurs
      // (par ex. une autre dependance) se verrait revendique a tort.
      if (importeurNormalise.includes('/node_modules/')) return null
      if (!importeurNormalise.startsWith(kitSrcPrefixe)) return null
      return this.resolve(source.replace('@/', `${kitSrc}/`), importer, { skipSelf: true })
    },
  }
}

export default defineConfig({
  plugins: [vue(), kitAlias()],
  test: { environment: 'jsdom', globals: true },
})
