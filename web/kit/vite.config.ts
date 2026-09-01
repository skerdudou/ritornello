import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'

// ESM library build. `vue` is **external**: it is provided by the shell's
// import map, so that a single instance serves the shell and every plugin
// module. The output name is **stable** (no hash): it is the URL the
// import map points to and against which plugins are compiled.
export default defineConfig({
  plugins: [vue()],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  // `build.lib` mode does not substitute `process.env.NODE_ENV` (it assumes
  // a downstream bundler). Without this `define`, `ui-kit.js` keeps a
  // reference to `process.env.NODE_ENV` in `DialogContent`'s `setup()`
  // (reka-ui): the file loads fine (no module-level reference), but the
  // first mounted dialog throws a `ReferenceError` at runtime.
  define: { 'process.env.NODE_ENV': JSON.stringify('production') },
  build: {
    lib: { entry: 'src/index.ts', formats: ['es'], fileName: () => 'ui-kit.js' },
    rollupOptions: { external: ['vue'] },
    cssCodeSplit: false,
    emptyOutDir: true,
  },
})
