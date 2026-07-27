import { createApp } from 'vue'
import App from './App.vue'
import './app.css'
import { initTheme } from './composables/useTheme'
import { router } from './router'

// Le theme est applique **avant** le montage : le premier rendu est deja
// dans les bonnes couleurs.
initTheme()

createApp(App).use(router).mount('#app')
