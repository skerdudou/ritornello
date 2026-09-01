// Vue re-export, built separately under a stable name (`assets/vue.js`) to
// be the import map's target. Every bundle (shell, kit, plugin modules)
// marks `vue` as external and resolves it here: a single Vue instance
// lives in the page, hence a single reactivity system and a single
// `provide`/`inject` tree.
export * from 'vue'
