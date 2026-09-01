import { createRouter, createWebHistory } from 'vue-router'

// Historical URLs are preserved: `/status` and `/plugins/<name>/` already
// answered, the core now serves them by falling back to the shell.
export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', name: 'home', component: () => import('./views/HomeView.vue') },
    // `/status` is the historical URL of this page (served by the core since
    // the beginning): it stays valid and redirects to its new name.
    { path: '/config', name: 'config', component: () => import('./views/ConfigView.vue') },
    { path: '/system', name: 'system', component: () => import('./views/SystemView.vue') },
    { path: '/status', redirect: '/config' },
    // `strict` on both forms: without it, the router tolerates a missing or
    // extra trailing slash, so `/plugins/<name>` would already match the
    // canonical route and the redirect below would never fire — while
    // `/plugins/<name>/` could match the redirect and loop. Each URL form thus
    // matches exactly one route.
    // The list of plugin pages: the target of the "Plugins" tab of the bottom
    // bar, which needs a fixed destination whatever the number of plugins.
    // `strict` so as not to match a bare `/plugins`.
    { path: '/plugins/', name: 'plugins', strict: true, component: () => import('./views/PluginsView.vue') },
    {
      path: '/plugins/:name/',
      name: 'plugin',
      strict: true,
      component: () => import('./views/PluginRoute.vue'),
    },
    // `/plugins/<name>` (without trailing slash) matched the canonical route and
    // mounted the page, but the displayed URL stayed without a slash — and the
    // plugin modules then resolved `./api/data` to `/plugins/api/data`, which
    // the core interprets as the plugin "api" (404). They now receive an
    // absolute prefix through the `base` prop (see `PluginView.ts`), so their
    // requests no longer depend on the URL form; this redirect nevertheless
    // brings the URL back to its canonical form — the only one documented for
    // third-party plugin authors — rather than letting two equivalent URLs live.
    { path: '/plugins/:name', strict: true, redirect: (to) => `/plugins/${to.params.name}/` },
  ],
})
