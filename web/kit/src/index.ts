export { UI_CONTRACT } from './contract'
export { createT, type Catalog } from './i18n'
export { api } from './api'
export { onPlayer } from './player'
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
// `CardAction` is what switches `CardHeader` to two columns: its class
// `has-data-[slot=card-action]:grid-cols-[1fr_auto]` only activates in the
// presence of a child carrying that slot. Without it, an action placed in the
// header ends up on the second row of the grid, under the title — and no
// hand-added utility class fixes that cleanly.
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
// Tabs only mount the active panel: `TabsContent` unmounts the others. A page
// that puts server-polling panes in them therefore stops polling what is not
// being looked at -- this is intended, but a pane that must survive the hidden
// tab belongs to the page, not to the panel.
export {
  Tabs, TabsContent, TabsList, TabsTrigger,
} from './components/ui/tabs'
export { Toaster } from './components/ui/sonner'
export { toast } from 'vue-sonner'
