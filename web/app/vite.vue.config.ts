import { defineConfig } from 'vite'

export default defineConfig({
  // Le mode `build.lib` ne substitue pas `process.env.NODE_ENV` (il suppose
  // un bundler en aval qui s'en chargera) : sans ce `define`, `vue.js`
  // conserve des centaines de references non gardees a `process`, dont une
  // au niveau module — le navigateur n'a pas de `process` global, l'evaluation
  // du fichier leve une `ReferenceError` avant la premiere ligne de code
  // applicatif. Fixer la valeur ici permet en outre a la minification
  // d'eliminer les branches de developpement de Vue.
  define: { 'process.env.NODE_ENV': JSON.stringify('production') },
  build: {
    lib: { entry: 'src/vue-entry.ts', formats: ['es'], fileName: () => 'vue.js' },
    outDir: 'dist/assets',
    // Le build de l'app est passe avant : ne pas vider, sinon on efface
    // `index.html` et les chunks hashes.
    emptyOutDir: false,
    copyPublicDir: false,
  },
})
