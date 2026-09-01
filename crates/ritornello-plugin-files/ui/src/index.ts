import { UI_CONTRACT } from '@ritornello/ui'
import FilesAdmin from './FilesAdmin.vue'
import './ui.css'

// Contract version this module is compiled against. The shell compares it
// to its own before mounting the component.
export const contract = UI_CONTRACT
export default FilesAdmin
export { FilesAdmin }
