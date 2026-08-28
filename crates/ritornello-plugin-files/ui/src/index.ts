import { UI_CONTRACT } from '@ritornello/ui'
import FilesAdmin from './FilesAdmin.vue'
import './ui.css'

// Version du contract contre laquelle ce module est compilé. Le shell la
// compare à la sienne avant de mount le composant.
export const contract = UI_CONTRACT
export default FilesAdmin
export { FilesAdmin }
