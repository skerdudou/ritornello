# radio-pi — Architecture à plugins (Source / Sink / Input)

Rendre radio-pi extensible sans recompiler le cœur : de futurs plugins (sortie
Bluetooth, source de musique locale via un partage Samba, plus tard d'autres
entrées/sorties dont de la vidéo) doivent pouvoir s'ajouter comme des
processus externes, tout en réutilisant l'infrastructure existante (mpv
partagé, télécommande, affichage, page web).

Date : 2026-07-18 — Statut : validé

## Contexte

`radio-pi` (voir `docs/superpowers/specs/2026-07-17-radio-pi-design.md`,
implémenté sur `feat/v1`) code aujourd'hui Radio et CD en dur dans `core.rs` :
`Mode` est une enum figée à deux valeurs, et toute la logique de présélections,
de veille, de bascule de mode est écrite en supposant qu'il n'y aura jamais
qu'un choix binaire. Pour ajouter un vrai plugin (Bluetooth en sortie, Samba en
source), il faut d'abord généraliser cette architecture. Ce document couvre
**uniquement cette généralisation** : aucune fonctionnalité utilisateur
nouvelle n'est livrée ici (pas de Bluetooth, pas de Samba) — l'objectif est de
prouver le mécanisme de bout en bout en portant l'existant (Radio, CD,
télécommande MCE) dans le nouveau modèle. Bluetooth et Samba feront chacun
l'objet d'une spec courte et indépendante une fois ce socle en place.

## Décisions de cadrage

| Sujet | Décision |
|---|---|
| Mécanisme de plugin | Processus séparés, protocole IPC (pas de compilation statique, pas de `.so` dynamique) |
| Transport | Socket Unix, JSON une ligne par message, corrélation par `id` — même pattern que le client mpv existant (`src/player/mpv.rs`) |
| Radio / CD | Deviennent des plugins Source au même titre que les futurs plugins ; aucune spécificité conservée dans le cœur |
| Télécommande MCE | Devient un plugin Input, même modèle |
| Interface web | Reste interne au cœur (outil d'admin, pas du matériel interchangeable) |
| Sélection de la sortie audio active | Uniquement via la page web (pas de nouveau bouton télécommande) |
| Supervision | Le cœur spawn chaque plugin comme processus enfant (comme mpv aujourd'hui) ; la mort d'un plugin Source/Sink/Input est tolérée (le plugin est marqué indisponible, le reste continue) — seule la mort de mpv fait sortir le processus (cas déjà géré, inchangé) |
| Comportement perdu (assumé) | Les chiffres 1-9 ne basculent plus automatiquement en mode Radio ; ils sont relayés à la source active, qui les interprète à sa façon |
| Comportement gagné (assumé) | Le plugin CD interprète `select(n)` comme « saute à la piste n » (1-9) — nouveauté minime et gratuite, rendue possible par le protocole |

## Vue d'ensemble

Le projet devient un **workspace Cargo** :

- `radio-pi-core` — l'actuel binaire, devenu orchestrateur : possède le seul
  `Player` (mpv) partagé, le registre de sources, le registre de sinks, la
  veille, le volume, l'affichage, la page web, la persistance.
- `radio-pi-proto` — types du protocole IPC partagés (Command, requêtes/réponses
  Source et Sink, sérialisation).
- `radio-pi-plugin-sdk` — boucle d'écoute de socket + traits `SourcePlugin`,
  `SinkPlugin`, `InputPlugin` : écrire un plugin, c'est implémenter ces
  méthodes, pas réécrire la tuyauterie JSON.
- `radio-pi-plugin-radio`, `radio-pi-plugin-cd` — portage des logiques
  existantes (présélections TOML, ioctl CD + MusicBrainz) derrière `SourcePlugin`.
- `radio-pi-plugin-mce` — portage de la lecture evdev + mapping de touches
  derrière `InputPlugin`.

Un fichier `/etc/radio-pi/plugins.toml` déclare les plugins actifs (nom, genre,
chemin de l'exécutable). Au démarrage, `radio-pi-core` les spawn tous, se
connecte à leur socket (même boucle de connexion avec retry que pour mpv), et
les traite comme indisponibles s'ils ne répondent pas — sans bloquer le
démarrage du reste.

## Les trois types de plugin

Le cœur garde la responsabilité qu'il a déjà : **c'est toujours lui qui pilote
mpv**. Aucun plugin ne décode ni ne joue de l'audio lui-même.

### Source

Représente un mode d'écoute (radio, CD, plus tard Samba, Spotify Connect…).
Reçoit du cœur, un message JSON par ligne avec un `id` de corrélation :

- `activate` / `deactivate` — devient/cesse d'être la source active
- `select {n: u8}` — chiffre 1-9 de la télécommande
- `next` / `prev`
- `play_pause` / `stop` / `eject`

Répond avec soit une action pour le cœur (`{"play":{"uri":"..."}}` /
`{"stop":true}` / `{"noop":true}`), soit une mise à jour d'affichage
(`line1`/`line2`/`line3`, le format `View` existant inchangé). Peut aussi
émettre des notifications spontanées sans attendre de requête (ex. piste CD
suivante détectée par mpv, métadonnées MusicBrainz arrivées en différé).

`uri` peut être un flux `http(s)://`, `cdda://n` (piste CD), ou plus tard un
chemin de fichier local (Samba) — mpv les joue tous de la même façon,
inchangé.

### Sink

Représente une destination audio alternative au jack analogique (plus tard
Bluetooth, sortie vidéo…). Reçoit `activate`/`deactivate`, répond avec le
périphérique audio à donner à mpv (`{"audio_device":"alsa/bluealsa:DEV=..."}`)
ou une erreur, et notifie spontanément ses changements d'état
(connecté/déconnecté) — une perte de connexion fait revenir automatiquement
le cœur sur la sortie par défaut (jack).

### Input

Représente du matériel de contrôle (télécommande IR, plus tard peut-être un
autre boîtier). Protocole à sens unique, plugin → cœur : un message par
appui, `{"cmd":"Preset","arg":3}` etc. — pas de requête/réponse, pas d'`id`.
`Command` gagne `Serialize`/`Deserialize` (nécessaire pour traverser l'IPC ;
il ne le faisait pas dans la version mono-binaire).

## Ce qui change dans le cœur

- `Mode` (enum figée) disparaît, remplacé par un identifiant de source
  (`String`, le nom déclaré dans `plugins.toml`) et un registre de sources
  connectées. `ToggleMode` devient « source suivante », qui cycle sur N
  sources au lieu de basculer entre exactement deux.
- Les commandes deviennent génériques et sont relayées telles quelles à la
  source active — le cœur ne les interprète plus. Les deux paires physiques
  de boutons de la télécommande restent deux axes distincts (pas de fusion) :
  `Command::Select(u8)` (chiffre 1-9, remplace `Preset`),
  `Command::Next`/`Command::Prev` (boutons chaîne+/-, remplace
  `StationNext`/`StationPrev` — « élément suivant au niveau de navigation
  principal » : station pour Radio, dossier/playlist pour un futur Samba),
  `Command::NextTrack`/`Command::PrevTrack` (boutons média, inchangés dans
  leur rôle — « piste suivante à l'intérieur de la sélection courante » :
  piste CD aujourd'hui, piste dans une playlist Samba demain),
  `Command::ToggleMode` renommée `Command::SourceCycle` (source suivante,
  cycle sur N sources au lieu de deux).
- `Eject` est relayé à la source active comme les autres (seul le plugin CD y
  répond ; les autres l'ignorent) ; l'éjection matérielle elle-même reste
  déclenchée par le cœur après la réponse (inchangé dans son principe).
- Volume, mute, veille (`Power`), persistance de la source active et du
  volume : restent gérés directement par le cœur, inchangés dans leur esprit
  — ils s'appliquent au `Player` partagé, indépendamment de la source/sink
  active.
- L'état persisté se simplifie : `{ active_source: String, volume: u8 }`. À
  chaque plugin Source de retenir et reprendre sa propre dernière
  présélection/piste dans son propre fichier d'état (ex.
  `/var/lib/radio-pi/plugin-radio.json`) — le cœur ne connaît plus ces
  détails.
- Au démarrage, le cœur envoie `activate` à la dernière source persistée ; si
  elle est indisponible, il retombe sur la première source enregistrée
  disponible et affiche un état d'erreur sinon.
- L'affichage (`View`) n'est plus calculé par le cœur à partir de champs
  internes (mode, preset, disc_info…) : c'est la **source active** qui
  fournit `line1/line2/line3` à chaque réponse/notification ; le cœur les
  relaie tels quels au composant `display` (inchangé). Tant qu'une source
  n'a encore rien renvoyé, ou si elle est indisponible, le cœur affiche un
  texte générique (nom de la source + état de connexion).
- La veille (`Power`) reste un concept du cœur : elle court-circuite
  l'affichage (« VEILLE ») quel que soit l'état de la source active, et
  envoie `deactivate`/`activate` à la source active en plus de
  `stop`/reprise sur le `Player`.

## Déploiement

Un seul service systemd (le cœur, inchangé dans ses champs). Les plugins ne
sont pas des unités systemd séparées : ce sont des binaires présents sur le
disque, déclarés dans `plugins.toml`, spawnés par le cœur. `deploy/deploy.sh`
est étendu pour cross-compiler et copier l'ensemble des binaires du workspace
(cœur + plugins de premier jet), pas seulement un binaire unique.

## Tests

- Les tests actuels de `core.rs` (sur `FakePlayer`) sont remplacés par des
  tests sur le `Core` généralisé, avec des sources/sinks factices en mémoire
  (implémentations directes des traits, sans passer par un vrai socket) —
  même esprit de test comportemental, nouvelle forme.
- La logique métier de Radio (présélections TOML) et CD (ioctl, TOC,
  MusicBrainz) migre presque inchangée dans les deux nouveaux binaires ; les
  tests unitaires déjà écrits pour ces logiques survivent, seul leur point
  d'entrée (désormais un plugin, via le SDK) change.
- Le protocole IPC lui-même (sérialisation, corrélation par `id`, notifications
  spontanées) est testé côté `radio-pi-plugin-sdk` avec `UnixStream::pair()`,
  sur le modèle des tests déjà écrits pour le client mpv.

## Hors périmètre (specs futures)

- Plugin Sink Bluetooth (appairage, `bluealsa`, bascule automatique).
- Plugin Source Samba (parcours d'un partage SMB déjà monté par l'OS,
  présélections = dossiers de premier niveau, chiffres = sélection,
  next/prev = piste dans le dossier — à détailler dans sa propre spec).
- Tout nouveau type de plugin (sortie vidéo, autres entrées) suivra le même
  modèle (transport, supervision, tolérance à la panne) sans reprendre ce
  document.
