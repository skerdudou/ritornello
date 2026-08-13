import { createRouter, createWebHistory } from 'vue-router'

// Les URL historiques sont conservees : `/status` et `/plugins/<nom>/`
// repondaient deja, le coeur les sert desormais par repli sur le shell.
export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', name: 'home', component: () => import('./views/HomeView.vue') },
    // `/status` est l'URL historique de cette page (servie depuis les débuts
    // par le cœur) : elle reste valide et redirige vers son nouveau nom.
    { path: '/config', name: 'config', component: () => import('./views/ConfigView.vue') },
    { path: '/system', name: 'system', component: () => import('./views/SystemView.vue') },
    { path: '/status', redirect: '/config' },
    // `strict` sur les deux formes : sans lui, le routeur tolere un slash
    // final absent ou surnumeraire, donc `/plugins/<nom>` matcherait deja la
    // route canonique et la redirection ci-dessous ne se declencherait jamais
    // — tandis que `/plugins/<nom>/` pourrait matcher la redirection et
    // boucler. Chaque forme d'URL matche ainsi exactement une route.
    {
      path: '/plugins/:name/',
      name: 'plugin',
      strict: true,
      component: () => import('./views/PluginRoute.vue'),
    },
    // `/plugins/<nom>` (sans slash final) matchait la route canonique et
    // montait la page, mais l'URL affichee restait sans slash — et les modules
    // de plugin resolvaient alors `./api/data` vers `/plugins/api/data`, que le
    // coeur interprete comme le plugin « api » (404). Ils recoivent desormais
    // un prefixe absolu par la prop `base` (voir `PluginView.ts`), donc leurs
    // requetes ne dependent plus de la forme de l'URL ; cette redirection
    // ramene malgre tout l'URL a sa forme canonique — la seule documentee pour
    // les auteurs de plugins tiers — plutot que de laisser vivre deux URL
    // equivalentes.
    { path: '/plugins/:name', strict: true, redirect: (to) => `/plugins/${to.params.name}/` },
  ],
})
