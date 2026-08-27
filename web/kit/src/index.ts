export { UI_CONTRACT } from './contract'
export { createT, type Catalog } from './i18n'
export { api } from './api'
export { surLecteur } from './lecteur'
export { cn } from './lib/utils'
export {
  applyTheme,
  DEFAULT_MODE,
  DEFAULT_PRESET,
  fontFamilies,
  presets,
  resolveVars,
  withFallback,
  type Mode,
  type Preset,
} from './themes/engine'

export { Button } from './components/ui/button'
export { Input } from './components/ui/input'
export { Label } from './components/ui/label'
export { Badge } from './components/ui/badge'
export { Switch } from './components/ui/switch'
export { ScrollArea } from './components/ui/scroll-area'
export { Slider } from './components/ui/slider'
// `CardAction` est ce qui fait passer `CardHeader` en deux colonnes : sa classe
// `has-data-[slot=card-action]:grid-cols-[1fr_auto]` ne s'active qu'en presence
// d'un enfant portant ce slot. Sans lui, une action placee dans l'en-tete se
// retrouve sur la deuxieme ligne de la grille, sous le titre — et aucune classe
// utilitaire ajoutee a la main ne corrige proprement cela.
export {
  Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle,
} from './components/ui/card'
export {
  Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle, DialogTrigger,
} from './components/ui/dialog'
export {
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from './components/ui/select'
export {
  Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
} from './components/ui/table'
// Les onglets ne montent que le panneau actif : `TabsContent` demonte les
// autres. Une page qui y range des volets sondant le serveur cesse donc de
// sonder ce qu'on ne regarde pas -- c'est voulu, mais un volet qui doit
// survivre a l'onglet cache appartient a la page, pas au panneau.
export {
  Tabs, TabsContent, TabsList, TabsTrigger,
} from './components/ui/tabs'
export { Toaster } from './components/ui/sonner'
export { toast } from 'vue-sonner'
