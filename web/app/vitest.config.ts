import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'
import { configDefaults, defineConfig, type Plugin } from 'vitest/config'

const appSrc = fileURLToPath(new URL('./src', import.meta.url))
const kitSrc = fileURLToPath(new URL('../kit/src', import.meta.url))
// Prefixe absolu, normalise et termine par '/', contre lequel comparer les
// importeurs. Une comparaison par sous-chaine (`includes('/web/kit/')`) se
// declencherait a tort si le depot lui-meme etait clone sous un chemin
// contenant "web/kit" ailleurs que dans ce monorepo ; un prefixe absolu
// derive de l'URL reelle du paquet ne peut pas produire ce faux positif.
const kitSrcPrefixe = `${kitSrc.replaceAll('\\', '/').replace(/\/$/, '')}/`

// `@ritornello/ui` expose ses sources TypeScript brutes (non compilees) :
// ses propres fichiers importent via l'alias '@', qui designe `web/kit/src`
// dans son atelier. Le meme alias designe `web/app/src` ici. Vite resolvant
// les alias par specificateur et non par importeur, un alias global unique
// casserait l'un des deux ateliers (par ex. Button.vue important
// `@/lib/utils` se verrait redirige vers `web/app/src/lib/utils`, inexistant).
// Ce plugin route donc `@/...` vers la racine du paquet qui importe.
function crossPackageAlias(): Plugin {
  return {
    name: 'ritornello-cross-package-alias',
    resolveId(source, importer) {
      if (!source.startsWith('@/') || !importer) return null
      const importeurNormalise = importer.replaceAll('\\', '/')
      // `node_modules` (y compris nos propres paquets, symlinkes par npm
      // workspaces) n'est jamais concerne par cet alias : seuls les
      // fichiers sources de web/app et de web/kit y ont droit. Sans ce
      // garde, tout importateur ne matchant pas web/kit se voyait
      // revendique silencieusement par web/app, y compris depuis une
      // dependance tierce.
      if (importeurNormalise.includes('/node_modules/')) return null
      const racine = importeurNormalise.startsWith(kitSrcPrefixe) ? kitSrc : appSrc
      return this.resolve(source.replace('@/', `${racine}/`), importer, { skipSelf: true })
    },
  }
}

export default defineConfig({
  plugins: [vue(), crossPackageAlias()],
  // `restoreMocks` : les `vi.spyOn(console, 'warn')` de PluginView.test.ts
  // fuyaient sinon d'un test a l'autre au sein du meme fichier.
  // `exclude` : `e2e/*.spec.ts` sont des parcours Playwright (Task 13), pas
  // des tests vitest — sans cette exclusion, `vitest run` tenterait de les
  // executer et echouerait sur l'import de `@playwright/test`.
  test: {
    environment: 'jsdom',
    globals: true,
    restoreMocks: true,
    exclude: [...configDefaults.exclude, 'e2e/**'],
  },
})
