# radio-pi — Display en plugin + sélecteur de sortie audio

Faire suivre à l'affichage le même modèle d'extensibilité que Source et Input
(promotion en plugin), retirer le mécanisme Sink jamais utilisé, et ajouter un
vrai sélecteur de sortie audio basé sur les périphériques déjà connus de l'OS.

Date : 2026-07-21 — Statut : validé

## Contexte

L'architecture à plugins (voir `docs/superpowers/specs/2026-07-18-plugin-architecture-design.md`,
implémentée sur `feat/v1`) définit trois genres de plugin : Source, Sink,
Input. Source et Input ont chacun un plugin réel (Radio/CD, MCE). **Sink n'en
a jamais eu** : c'était de l'architecture posée par anticipation d'un futur
plugin Bluetooth qui gérerait lui-même la sortie audio via IPC. L'affichage
console, lui, n'est pas un plugin du tout — il reste un composant intégré au
cœur (`display::ConsoleDisplay`), ce qui est incohérent avec le principe
« tout ce qui est remplaçable/matériel est un plugin » déjà appliqué à la
télécommande.

En creusant le besoin réel de sortie audio (un jour, une enceinte Bluetooth),
il apparaît qu'un mécanisme Sink séparé n'est pas nécessaire : une fois un
appareil Bluetooth appairé via `bluetoothctl` (étape manuelle, hors radio-pi),
`bluez-alsa` l'expose comme un périphérique ALSA ordinaire, visible par
`aplay -L` au même titre que le jack. Un simple sélecteur listant les sorties
connues de l'OS suffit donc ; l'appairage d'un nouvel appareil reste une
opération manuelle (ou une fonctionnalité future séparée, si un jour on veut
la piloter depuis l'IHM).

## Décisions de cadrage

| Sujet | Décision |
|---|---|
| Genres de plugin | `Source`, `Input`, `Display` — **`Sink` retiré** (code mort, jamais instancié) |
| Affichage | Devient un plugin (`radio-pi-plugin-console`, portage à l'identique de `ConsoleDisplay`), protocole à sens unique cœur → plugin (miroir d'Input, qui va plugin → cœur) |
| Sortie audio | **Pas un plugin** : le cœur interroge l'OS (`aplay -L`) et expose un sélecteur sur sa page de statut existante |
| Bluetooth | Hors périmètre de cette livraison ; l'appairage reste manuel. Un futur plugin Sink pourra être réintroduit dans sa propre spec si un jour la gestion de l'appairage depuis l'IHM est voulue |
| Comportement | Zéro régression sur Radio/CD/MCE ; le sélecteur de sortie audio est la seule fonctionnalité utilisateur nouvelle |

## Ce qui disparaît

- `crates/radio-pi-proto/src/sink.rs` (`SinkReq`, `SinkRequest`, `SinkMessage`)
- Dans `crates/radio-pi-plugin-sdk` : `SinkPlugin`, `SinkOutcome`,
  `run_sink_plugin`, `SinkClient`
- `PluginKind::Sink` dans `crates/radio-pi-core/src/plugins.rs`
- Au passage, nettoyage des `#[allow(dead_code)]` par item devenus inutiles
  sur `plugins.rs` (signalés par la revue finale de la livraison précédente,
  cohérent de les traiter ici puisqu'on touche déjà ce fichier)

## Le plugin Display

Même transport que les autres (socket Unix, JSON par ligne, le plugin lie et
écoute, le cœur se connecte). Protocole à sens unique, **cœur → plugin**
(l'inverse d'Input) : le cœur envoie une `View` chaque fois qu'elle change,
sans attendre de réponse.

- `radio_pi_proto::DisplayMessage` — encapsule une `View` (`{ view: View }`,
  une ligne JSON par mise à jour).
- `radio_pi_plugin_sdk::DisplayPlugin` (trait, côté plugin) :
  `async fn show(&mut self, view: View) -> Result<()>`. `run_display_plugin`
  lie le socket, accepte une connexion, lit les lignes et appelle `show` pour
  chacune.
- `radio_pi_plugin_sdk::DisplayClient` (côté cœur) : `connect` (même boucle de
  retry que les autres clients), `send(view: View)` — écrit la ligne, ne
  corrèle rien (pas d'`id`, pas de réponse attendue).

`radio-pi-plugin-console` est un nouveau binaire portant `ConsoleDisplay` à
l'identique (mêmes variables d'environnement renommées avec le préfixe du
plugin, ex. `RADIO_PI_CONSOLE_TTY`). Le cœur perd son module `display` : au
lieu d'appeler `ConsoleDisplay::show` directement dans la boucle qui observe
`view_rx`, il appelle `DisplayClient::send`. Si aucun plugin Display n'est
déclaré ou n'est joignable, le cœur continue de fonctionner sans affichage
(comportement déjà existant pour tout plugin absent), avec un avertissement
loggé une fois au démarrage.

## Le sélecteur de sortie audio

Entièrement interne au cœur, aucun protocole IPC nouveau.

- `crates/radio-pi-core/src/audio_output.rs` (nouveau) : `list_devices() -> Result<Vec<String>>`
  exécute `aplay -L` et parse la sortie (une entrée par périphérique nommé,
  ex. `default`, `hw:CARD=...`, `bluealsa:...`).
- Le trait `Player` gagne `async fn set_audio_device(&self, device: &str) -> Result<()>` ;
  `MpvPlayer` l'implémente via `set_property audio-device <device>` (propriété
  mpv déjà pilotable à chaud, pas besoin de relancer mpv).
- `PersistedState` gagne `audio_device: Option<String>` (`None` = laisser mpv
  sur son défaut). Appliqué par le cœur juste après le volume, au démarrage
  et à la reprise de veille (même point que `set_volume` aujourd'hui).
- Page de statut du cœur : `GET /api/audio-output` (liste des périphériques +
  sélection courante), `PUT /api/audio-output` (change la sélection, appelle
  `set_audio_device`, persiste) ; un petit formulaire ajouté à `/status`.
- Erreur de lecture de `aplay -L` (binaire absent, périphérique invalide) :
  liste vide renvoyée, message d'erreur affiché sur la page plutôt qu'un
  crash — même esprit que la tolérance déjà appliquée ailleurs dans le cœur.

## Tests

- `audio_output::list_devices` testé en injectant une sortie `aplay -L`
  simulée (fonction pure de parsing séparée de l'appel au binaire, comme déjà
  fait pour `cd::mb_toc_param`).
- `DisplayClient`/`run_display_plugin` testés sur le même modèle que les
  autres genres de plugin (socket réel en répertoire temporaire).
- Les tests existants de `Core` (sources factices) ne sont pas affectés : ce
  changement ne touche pas au routage des commandes ni au registre de
  sources.

## Hors périmètre

- Tout plugin Sink réel (Bluetooth ou autre) — spec future si le besoin de
  piloter l'appairage depuis l'IHM se confirme.
- Un second plugin Display (OLED SSD1306) — cette livraison prouve le
  mécanisme avec le portage à l'identique de la console ; l'OLED reste pour
  quand le matériel sera en main, comme prévu depuis le tout premier design.
