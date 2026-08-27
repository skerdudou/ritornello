import type { Mode } from '@ritornello/ui'

export interface PluginStatus {
  name: string
  kind: string
  connected: boolean
  admin: boolean
  /** Lancé, pas encore annoncé, et **passé** le délai normal : un diagnostic. */
  stalled?: boolean
  /** Lancé à l'instant, pas encore annoncé, et c'est normal. Exclusif avec
   * `stalled`, dont il ne diffère que par le temps écoulé — mais la différence
   * est tout : « figé » accuse, « démarrage » constate. */
  starting?: boolean
  disabled?: boolean
  /** Joint, mais sa page d'admin ne répond pas au ping : un `set_data` long
   * tient son verrou (le plus souvent un partage réseau). Calculé au moment
   * du `/api/status`, donc peut changer d'un rafraîchissement à l'autre. */
  busy?: boolean
}
export interface StatusPayload { plugins: PluginStatus[]; active_source: string }
export interface AudioDevice { name: string; description: string }
export interface AudioPayload { devices: AudioDevice[]; current: string | null }
export interface LocalePayload { locales: string[]; current: string | null }
export interface ThemePayload { theme: string; mode: Mode }
export interface LogsPayload { lines: string[] }
/** Les trois valeurs de `settings.startup_power`, cote coeur comme cote IHM. */
export type StartupPower = 'on' | 'standby' | 'previous'
/** Réglages de comportement, tels que les sert `GET /api/settings`. */
export interface SettingsPayload {
  volume_repeat_initial_ms: number
  volume_repeat_interval_ms: number
  /**
   * Comportement au demarrage du service : `on` reveille la source active,
   * `standby` laisse l'appareil en veille, `previous` reprend l'etat qu'il
   * avait au dernier arret.
   */
  startup_power: StartupPower
  /** Durée d'affichage de l'incrustation volume/muet et des messages éphémères des sources. */
  overlay_ms: number
  /** Fenêtre de saisie du cumul `+10` de la télécommande (temps laissé pour la seconde pression). */
  tens_window_ms: number
  /**
   * Plafond de la pochette **source**, en mébioctets. Toujours appliqué, que le
   * réencodage soit actif ou non — c'est la seule garde qui subsiste quand il
   * est décoché, et la raison pour laquelle l'IHM le sort de l'encart grisé.
   */
  cover_source_max_mio: number
  /** Réencoder les pochettes en vignette, ou pousser la source telle quelle. */
  cover_rendition: boolean
  /** Côté le plus long de la vignette, en pixels. Rendu seulement. */
  cover_max_edge_px: number
  /** Qualité JPEG de la vignette. Rendu seulement, et ignorée si l'image a un canal alpha. */
  cover_jpeg_quality: number
  /** Plafond de la vignette produite, en kibioctets. Rendu seulement. */
  cover_max_bytes_ko: number
  /** Plafond de pixels à décoder, en mégapixels. Rendu seulement. */
  cover_max_pixels_mpx: number
  /** Pas des touches « avancer » / « reculer », en secondes. */
  seek_step_s: number
}
/**
 * Etat du lecteur, tel que le pousse `/api/player` : tout ce qui est volatil.
 *
 * Un seul objet, plat, pour un seul encart. `/api/status` porte a cote le
 * contrat de navigation, structurellement stable et lu une fois au montage —
 * c'est pourquoi le volume n'y est pas.
 *
 * Les champs du morceau sont optionnels : on affiche toute information
 * disponible, meme partielle. `origin` dit qui l'a fournie — `"icy"` pour ce
 * que le flux annonce lui-meme, sinon le nom du plugin `metadata` qui a gagne.
 */
export interface PlayerPayload {
  source: string
  volume: number
  muted: boolean
  standby: boolean
  /**
   * Touche numerotee correspondant a ce qui joue (preselection radio, piste cd),
   * telle que la source active l'a declaree — c'est elle que la telecommande
   * met en evidence. `null` : rien ne joue, ou rien de declare.
   */
  preset: number | null
  /**
   * Nombre de preselections que la source active declare (stations radio,
   * pistes cd), ou `null` si elle ne le declare pas — la grille retombe alors
   * sur les 9 touches nues historiques. `0` est significatif ("rien a
   * numeroter", ex. cd sans disque) et distinct de `null`.
   */
  preset_count: number | null
  /**
   * Nom lisible que la source active donne a la preselection en cours
   * (nom configure de la station pour la radio), ou `null` quand elle n'en
   * declare pas (le cd, ou rien ne joue). Vit et meurt avec `preset`.
   */
  preset_name: string | null
  /**
   * Phrase d'etat deja traduite : le statut declare par la source (« PAS DE
   * DISQUE ») ou le mot de veille resolu par le coeur. `null` quand il n'y a
   * rien a dire.
   */
  status: string | null
  /**
   * Incrustation en cours cote afficheur. La SPA l'ignore — elle montre deja
   * le volume en clair, et un ecran de navigateur n'a pas les contraintes de
   * largeur d'un afficheur de vingt colonnes — mais le champ voyage parce que
   * la charge utile est unique.
   */
  overlay: unknown | null
  artist: string | null
  title: string | null
  album: string | null
  /**
   * Année de sortie, quand un contributeur la connaît.
   *
   * Optionnelle, comme `links` : le cœur **omet** le champ plutôt que d'émettre
   * un `null`, pour qu'une trame sans année reste identique à l'octet près à
   * ce qu'elle était avant ce chantier.
   */
  year?: number | null
  /**
   * Les plateformes d'écoute où trouver ce morceau.
   *
   * Absent de la trame quand la liste est vide, d'où l'optionnel : le cœur
   * omet le champ plutôt que d'émettre un tableau vide. `platform` est un
   * ensemble fermé côté protocole, et l'URL a déjà été validée contre l'hôte
   * de cette plateforme — l'IHM n'a donc rien à revérifier avant d'en faire
   * un lien.
   */
  links?: { platform: 'youtube' | 'deezer' | 'apple_music'; url: string }[]
  duration_s: number | null
  origin: string | null
  /** URL locale de la pochette, servie par l'appareil. Jamais une URL externe. */
  cover_href: string | null
  /** Qui a fourni la pochette : nom de la Source, `tags`, ou nom du greffon. */
  cover_origin: string | null
  /**
   * Ou en est ce qui joue, en secondes, a l'instant ou la trame a ete
   * publiee — le coeur en pousse une par seconde pendant la lecture.
   * `null` quand personne ne sait : rien ne joue, ou c'est un flux qu'aucun
   * plugin `metadata` ne suit.
   */
  position_s: number | null
  /**
   * Ce qui joue accepte un deplacement. Distinct de « une duree est connue » :
   * Radio France annonce la duree d'un morceau sur un direct qu'on ne peut pas
   * rembobiner. C'est ce champ, et lui seul, qui rend la barre cliquable.
   */
  seekable: boolean
  /**
   * La source active a de quoi ejecter : c'est ce qui grise la touche Eject
   * ailleurs que sur le lecteur de cd. Une capacite de la **source**, pas du
   * contenu — un tiroir vide s'ouvre aussi.
   *
   * Faux par defaut, et absent de la trame quand il est faux (comme
   * `seekable`) : ne pas savoir, c'est n'offrir rien.
   */
  can_eject: boolean
  /**
   * Ce que fait le lecteur : `playing`, `paused`, ou absent quand rien ne joue.
   * C'est ce qui choisit l'icône du bouton de lecture (▶ ou ❚❚). Le champ
   * voyageait déjà sans être lu.
   */
  playback?: Playback
}
export type Command = { cmd: string; arg?: number }
/** Ce que fait le lecteur. Absent de la trame quand il est arrêté (idiome de `seekable`). */
export type Playback = 'playing' | 'paused'
/** Une présélection nommée telle que `GET /api/presets` la sert. */
export interface PresetNomme { index: number; name: string }
/** Une source et sa liste ; `presets` est absent quand elle n'énumère pas. */
export interface SourcePresets { name: string; presets?: PresetNomme[] }
/** Le catalogue des sources, tel que le cœur le diffuse aux afficheurs. */
export interface PresetsPayload { sources: SourcePresets[] }
export interface SystemUsage { total_kb: number; available_kb: number }
/**
 * Metriques de l'OS, telles que les sert `GET /api/system`.
 *
 * Tout champ que la machine n'expose pas vaut `null` — pas de capteur
 * thermique, pas de cpufreq, pas de sonde de sous-tension — et la vue
 * affiche « — » sans traiter cela comme une panne. Le jeu de cles, lui, est
 * stable.
 */
export interface SystemPayload {
  temperature_c: number | null
  cpu_mhz: number | null
  load: [number, number, number] | null
  cpus: number | null
  memory: SystemUsage | null
  disk: SystemUsage | null
  under_voltage: boolean | null
  /** Sous-tension survenue depuis le démarrage (bit collant du micrologiciel),
   *  distincte de `under_voltage` (l'alarme instantanée) : un épisode dure
   *  quelques millisecondes à quelques secondes et un sondage à 5 s a peu de
   *  chances de tomber pile dessus, alors que ce bit reste vrai jusqu'au
   *  prochain démarrage. */
  under_voltage_since_boot: boolean | null
  uptime_s: number | null
  service_uptime_s: number
  hostname: string | null
  ip: string | null
  os: string | null
  kernel: string | null
  version: string
  can_power_off: boolean
  can_reboot: boolean
  /**
   * logind a-t-il répondu à la sonde de démarrage, quelle qu'ait été sa
   * réponse ? Départage les deux causes d'un bouton grisé : un refus appelle
   * la règle polkit, une absence de réponse appelle un `systemd-logind` qui
   * tourne. Deux réparations différentes, donc deux phrases.
   */
  logind_reachable: boolean
  /**
   * Compteurs cumulatifs de `/proc/stat` depuis le démarrage — jamais un
   * pourcentage : deux onglets sondant hors phase corrompraient un delta
   * calculé côté cœur. La vue les compare entre deux sondages successifs.
   */
  cpu_total_jiffies: number | null
  cpu_idle_jiffies: number | null
}
