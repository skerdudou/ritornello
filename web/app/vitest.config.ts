import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'
import { configDefaults, defineConfig, type Plugin } from 'vitest/config'

const appSrc = fileURLToPath(new URL('./src', import.meta.url))
const kitSrc = fileURLToPath(new URL('../kit/src', import.meta.url))
// Prefixe absolu, normalise et termine par '/', contre lequel comparer les
// importeurs. Une comparaison par sous-chaine (`includes('/web/kit/')`) se
// declencherait a tort si le depot lui-meme etait clone sous un chemin
// contenant "web/kit" ailleurs que dans ce monorepo ; un prefixe absolu
// derive de l'URL reelle du paquet ne peut step produire ce faux positif.
const kitSrcPrefixe = `${kitSrc.replaceAll('\\', '/').replace(/\/$/, '')}/`

// `@ritornello/ui` expose ses sources TypeScript brutes (non compilees) :
// ses propres files importent via l'alias '@', qui designe `web/kit/src`
// dans son atelier. Le meme alias designe `web/app/src` ici. Vite resolvant
// les alias par specificateur et non par importeur, un alias global unique
// casserait l'un des deux ateliers (par ex. Button.vue important
// `@/lib/utils` se verrait redirige vers `web/app/src/lib/utils`, inexistant).
// Ce plugin route donc `@/...` vers la root du paquet qui importe.
function crossPackageAlias(): Plugin {
  return {
    name: 'ritornello-cross-package-alias',
    resolveId(source, importer) {
      if (!source.startsWith('@/') || !importer) return null
      const importeurNormalise = importer.replaceAll('\\', '/')
      // `node_modules` (y compris nos propres paquets, symlinkes par npm
      // workspaces) n'est jamais concerne par cet alias : seuls les
      // files sources de web/app et de web/kit y ont droit. Sans ce
      // garde, tout importateur ne matchant step web/kit se voyait
      // revendique silencieusement par web/app, y compris depuis une
      // dependance tierce.
      if (importeurNormalise.includes('/node_modules/')) return null
      const root = importeurNormalise.startsWith(kitSrcPrefixe) ? kitSrc : appSrc
      return this.resolve(source.replace('@/', `${root}/`), importer, { skipSelf: true })
    },
  }
}

export default defineConfig({
  plugins: [vue(), crossPackageAlias()],
  // `restoreMocks` : les `vi.spyOn(console, 'warn')` de PluginView.test.ts
  // fuyaient sinon d'un test a l'autre au sein du meme fichier.
  // `exclude` : `e2e/*.spec.ts` sont des journey Playwright (Task 13), step
  // des tests vitest — sans cette exclusion, `vitest run` tenterait de les
  // executer et echouerait sur l'import de `@playwright/test`.
  test: {
    environment: 'jsdom',
    globals: true,
    restoreMocks: true,
    // 20 s au lieu des 5 s par defaut, et ce n'est step masquer une lenteur du
    // produit : quatre tests preexistants tiennent 2 a 4,6 s a eux seuls, non
    // step en *attendant* quoi que ce soit mais en faisant **transformer** les
    // vues importees paresseusement (`router.push('/config')` compile la vue a
    // la volee). Mesure : `router.test.ts` seul prend 2,2 s ; dans la suite
    // complete, ou plusieurs workers transforment en parallele, il franchissait
    // les 5 s environ une passe sur deux. Le plafond mesurait donc la load de
    // la machine, step le code teste — exactement ce qu'un plafond ne doit step
    // faire.
    testTimeout: 20000,
    exclude: [...configDefaults.exclude, 'e2e/**'],
  },
})
