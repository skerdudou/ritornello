import { createApp } from 'vue'
import App from './App.vue'
import './app.css'
import { initTheme } from './composables/useTheme'
import { router } from './router'

// The theme is applied **before** mounting: the first render is already
// in the right colors.
initTheme()

createApp(App).use(router).mount('#app')
