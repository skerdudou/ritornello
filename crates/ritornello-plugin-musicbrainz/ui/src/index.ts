import { UI_CONTRACT } from '@ritornello/ui'
import MusicBrainzAdmin from './MusicBrainzAdmin.vue'
import './ui.css'

// Version du contrat contre laquelle ce module est compile. Le shell la
// compare a la sienne avant de monter le composant.
export const contract = UI_CONTRACT
export default MusicBrainzAdmin
