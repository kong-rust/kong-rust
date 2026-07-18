import { createApp } from 'vue'
import App from '@/App.vue'
import { router } from '@/router'
import { registerGlobalComponents } from './registerGlobalComponents'
import './styles/index'
import { createPinia } from 'pinia'

// This only sets up worker initializers. They will be lazy-loaded when needed.
import '@/monaco-workers'

const app = createApp(App)

const pinia = createPinia()

app.use(pinia)
app.use(router)
registerGlobalComponents(app)
app.mount('#app')
