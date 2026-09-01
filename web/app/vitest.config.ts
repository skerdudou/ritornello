import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'
import { configDefaults, defineConfig, type Plugin } from 'vitest/config'

const appSrc = fileURLToPath(new URL('./src', import.meta.url))
const kitSrc = fileURLToPath(new URL('../kit/src', import.meta.url))
// Absolute prefix, normalized and ending with '/', to compare importers
// against. A substring comparison (`includes('/web/kit/')`) would trigger
// wrongly if the repo itself were cloned under a path containing
// "web/kit" elsewhere than in this monorepo; an absolute prefix derived
// from the package's real URL cannot produce that false positive.
const kitSrcPrefix = `${kitSrc.replaceAll('\\', '/').replace(/\/$/, '')}/`

// `@ritornello/ui` exposes its raw (uncompiled) TypeScript sources: its own
// files import via the '@' alias, which points to `web/kit/src` in its
// own workspace. The same alias points to `web/app/src` here. Since Vite
// resolves aliases by specifier and not by importer, a single global alias
// would break one of the two workspaces (e.g. Button.vue importing
// `@/lib/utils` would get redirected to `web/app/src/lib/utils`, which
// doesn't exist). This plugin therefore routes `@/...` to the root of the
// package doing the importing.
function crossPackageAlias(): Plugin {
  return {
    name: 'ritornello-cross-package-alias',
    resolveId(source, importer) {
      if (!source.startsWith('@/') || !importer) return null
      const normalizedImporter = importer.replaceAll('\\', '/')
      // `node_modules` (including our own packages, symlinked by npm
      // workspaces) is never concerned by this alias: only source files
      // of web/app and web/kit are entitled to it. Without this guard,
      // any importer not matching web/kit would be silently claimed by
      // web/app, including from a third-party dependency.
      if (normalizedImporter.includes('/node_modules/')) return null
      const root = normalizedImporter.startsWith(kitSrcPrefix) ? kitSrc : appSrc
      return this.resolve(source.replace('@/', `${root}/`), importer, { skipSelf: true })
    },
  }
}

export default defineConfig({
  plugins: [vue(), crossPackageAlias()],
  // `restoreMocks`: the `vi.spyOn(console, 'warn')` calls in PluginView.test.ts
  // would otherwise leak from one test to the next within the same file.
  // `exclude`: `e2e/*.spec.ts` are Playwright journeys (Task 13), not
  // vitest tests — without this exclusion, `vitest run` would try to
  // run them and fail on the `@playwright/test` import.
  test: {
    environment: 'jsdom',
    globals: true,
    restoreMocks: true,
    // 20 s instead of the default 5 s, and this is not hiding a product
    // slowness: four pre-existing tests alone take 2 to 4.6 s, not by
    // *waiting* for anything but by **transforming** lazily-imported
    // views (`router.push('/config')` compiles the view on the fly).
    // Measured: `router.test.ts` alone takes 2.2 s; in the full suite,
    // where several workers transform in parallel, it crossed the 5 s
    // mark about every other run. The cap was therefore measuring the
    // machine's load, not the code under test — exactly what a cap must
    // not do.
    testTimeout: 20000,
    exclude: [...configDefaults.exclude, 'e2e/**'],
  },
})
