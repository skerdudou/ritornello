import { UI_CONTRACT } from '@ritornello/ui'
import MpdAdmin from './MpdAdmin.vue'
import './ui.css'

// Version du contract contre laquelle ce module est compile. Le shell la
// compare a la sienne avant de monter le composant.
export const contract = UI_CONTRACT
export default MpdAdmin
