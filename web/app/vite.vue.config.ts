import { defineConfig } from 'vite'

export default defineConfig({
  // `build.lib` mode does not substitute `process.env.NODE_ENV` (it assumes
  // a downstream bundler will take care of it): without this `define`,
  // `vue.js` keeps hundreds of unguarded references to `process`, one of
  // them at module level — the browser has no global `process`, so
  // evaluating the file throws a `ReferenceError` before the first line
  // of application code. Fixing the value here also lets minification
  // eliminate Vue's development branches.
  define: { 'process.env.NODE_ENV': JSON.stringify('production') },
  build: {
    lib: { entry: 'src/vue-entry.ts', formats: ['es'], fileName: () => 'vue.js' },
    outDir: 'dist/assets',
    // The app build runs before this one: do not empty the dir, or we'd
    // erase `index.html` and the hashed chunks.
    emptyOutDir: false,
    copyPublicDir: false,
  },
})
