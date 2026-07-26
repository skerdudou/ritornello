# ritornello

Radio internet + lecteur CD, télécommande MCE, affichage console HDMI (OLED
SSD1306 prévu ensuite). Spec et plan dans `docs/superpowers/`.

Architecture à plugins : le cœur (`ritornello-core`) orchestre des plugins —
processus séparés communiquant par socket Unix — de trois genres : **Source**
(contenu à jouer : radio, CD), **Input** (télécommande) et **Display**
(affichage). Chaque genre a une interface stable ; ajouter un nouveau
plugin (ex. une source Bluetooth, un afficheur OLED) ne touche pas au cœur.
Voir la section [Plugins](#plugins).

## Portabilité

Rien dans le code n'est spécifique au Raspberry Pi : la télécommande MCE
passe par `evdev` (l'API Linux générique d'entrée, pas du GPIO), le son par
ALSA/mpv, l'IPC par sockets Unix — tout ça tourne sur n'importe quel Linux,
x86_64 comme ARM. Le Pi 2 est le matériel de référence historique de ce
projet, pas une contrainte technique — les exemples ci-dessous en sont une
simple illustration.

## Compiler

Le workspace compile nativement pour l'architecture de la machine qui lance
la commande (x86_64 sur un PC/serveur Linux classique), et pour ARM par
cross-compilation avec [`cross`](https://github.com/cross-rs/cross) (qui a
besoin de Docker) :

    # Natif (ex. x86_64) — utilisé aussi pour les tests en développement
    cargo build --workspace
    cargo test --workspace

    # Cross-compilation ARM (ex. Raspberry Pi 2, 32 bits)
    cargo install cross --locked
    cross build --release --workspace --target armv7-unknown-linux-gnueabihf

Les deux chemins sont testés à chaque évolution du projet. Autres cibles ARM
possibles avec `cross` : `aarch64-unknown-linux-gnu` (cartes ARM 64 bits,
type Pi 3/4/5 — non testé sur ce projet faute de matériel, mais sans raison
de ne pas fonctionner).

## Exemple : Raspberry Pi 2

Raspberry Pi OS Lite, puis :

    sudo apt install mpv cd-discid eject
    sudo cp deploy/stations.example.toml /etc/ritornello/stations.toml
    sudo cp deploy/plugins.example.toml /etc/ritornello/plugins.toml
    # jack analogique en sortie par défaut + volume matériel à fond
    sudo raspi-config nonint do_audio 1
    amixer set PCM 100%

Wifi : `sudo raspi-config` (System Options > Wireless LAN).

## Exemple : machine Linux x86_64 générique

Mêmes paquets, sans les étapes propres au Pi (pas de `raspi-config`, la
sortie audio se choisit directement dans `/api/audio-output`) :

    sudo apt install mpv cd-discid eject
    sudo cp deploy/stations.example.toml /etc/ritornello/stations.toml
    sudo cp deploy/plugins.example.toml /etc/ritornello/plugins.toml

`deploy/deploy.sh` fonctionne à l'identique : `TARGET=x86_64-unknown-linux-gnu
PI=user@host ./deploy/deploy.sh` (pas besoin de `cross`/Docker pour cette
cible si la machine qui compile est déjà x86_64 — `cargo build` natif suffit
alors, `cross` reste surtout utile pour changer d'architecture).

## Plugins

`ritornello-core` charge `/etc/ritornello/plugins.toml` au démarrage (voir
`deploy/plugins.example.toml`) : chaque entrée déclare un plugin (`source`,
`display` ou `input`), le chemin de son exécutable, et peut déclarer
`admin = true` pour exposer une page d'admin servie par le cœur, sous la même
origine, avec un lien affiché sur la page de statut du cœur
(`http://<pi>:8080/status`).

- `ritornello-plugin-radio` déclare `admin = true` : sa page de gestion des
  stations est servie par le cœur, sous l'origine unique, à
  `http://<hôte>:8080/plugins/radio/` (le plugin ne lie plus aucun port).
- La mort d'un plugin est tolérée : il est marqué indisponible sur la page de
  statut, les autres continuent de fonctionner.
- Aucun de ces plugins n'est spécifique au Pi : `ritornello-plugin-radio` et
  `ritornello-plugin-cd` sont du Rust portable pur, `ritornello-plugin-mce` et
  `ritornello-plugin-console` dépendent seulement de matériel Linux générique
  (respectivement un récepteur infrarouge USB reconnu par `evdev`, et une
  console `/dev/ttyN`) — pas d'un GPIO ou d'un bus propre au Pi. Un nouveau
  plugin (Bluetooth, écran OLED SPI/I2C...) s'ajoute sans toucher au cœur ni
  aux plugins existants, du moment qu'il respecte l'interface Source/Input/
  Display et parle le protocole JSON par ligne sur socket Unix.
- `ritornello-plugin-console` est le plugin d'affichage (console HDMI, variable
  `RITORNELLO_CONSOLE_TTY`, défaut `/dev/tty1`). La page de statut du cœur
  (`http://<pi>:8080/status`) propose aussi un sélecteur de sortie audio,
  basé sur les périphériques ALSA connus du système (`aplay -L`) — une
  enceinte Bluetooth déjà appairée via `bluetoothctl` y apparaîtra
  automatiquement une fois exposée par `bluez-alsa`.

## Télécommande web

La page de statut (`/status`) embarque une télécommande : les 13 commandes du
protocole (présélections 1-9, présélection et piste suivante/précédente,
volume, muet, lecture/pause, stop, éjecter, changement de source, veille).

Elle passe par `POST /api/command`, dont le corps est exactement une commande
du protocole — le même canal que celui alimenté par les plugins Input, donc
aucune logique métier dupliquée :

    curl -X POST http://<hôte>:8080/api/command \
      -H 'content-type: application/json' -d '{"cmd":"VolumeUp"}'
    curl -X POST http://<hôte>:8080/api/command \
      -H 'content-type: application/json' -d '{"cmd":"Select","arg":3}'

Pratique pour piloter l'appareil sans télécommande (depuis un téléphone sur le
réseau local, ou en SSH pendant la mise au point).

## Internationalisation (i18n)

L'interface est multilingue. La langue de base est l'**anglais**, embarquée dans
chaque binaire ; le français (et d'autres langues) sont fournis par des **packs
TOML externes**, décentralisés par composant :

    /etc/ritornello/locales/
      common/fr.toml   # vocabulaire commun (play/pause/stop/error…)
      core/fr.toml     # texte du cœur + page de statut
      radio/fr.toml    # plugin radio + page d'admin
      cd/fr.toml       # plugin cd
      <plugin-tiers>/fr.toml

- Racine configurable par `RITORNELLO_LOCALES` (défaut `/etc/ritornello/locales`).
- **Sélecteur** de langue sur la page de statut (`/status`) : il liste `en` plus
  tout pack `core/<lang>.toml` présent. Le changement est appliqué à chaud, poussé
  aux plugins, et persisté (`state.json`).
- **Ajouter une langue** : copier l'`en` de référence, traduire les valeurs, le
  déposer sous `<root>/<composant>/<lang>.toml`. Une clé ou un pack manquant
  retombe automatiquement sur l'anglais (dégradation par clé, jamais d'erreur).
- Les packs français initiaux sont livrés dans `deploy/locales/` et copiés par
  `deploy/deploy.sh`.

## Développement

Sur n'importe quelle machine Linux (ou WSL sous Windows, l'environnement
utilisé pour développer ce projet — WSL n'est qu'un détail d'environnement,
pas une exigence : un Linux natif fonctionne à l'identique). Après
`cargo build --workspace` (voir [Compiler](#compiler)), lancer une instance
locale sans matériel Pi :

    mkdir -p /tmp/rp
    cat > /tmp/rp/plugins.toml <<'PLUGINS'
    [[plugin]]
    name = "radio"
    kind = "source"
    exec = "target/debug/ritornello-plugin-radio"

    [[plugin]]
    name = "console"
    kind = "display"
    exec = "target/debug/ritornello-plugin-console"
    PLUGINS
    cat > /tmp/rp/stations.toml <<'STATIONS'
    [[stations]]
    name = "FIP"
    url = "http://icecast.radiofrance.fr/fip-midfi.mp3"
    preset = 1
    STATIONS
    RITORNELLO_PLUGINS=/tmp/rp/plugins.toml RITORNELLO_STATE=/tmp/rp/state.json \
    RITORNELLO_MPV_SOCKET=/tmp/rp/mpv.sock RITORNELLO_RUNTIME_DIR=/tmp/rp \
    RITORNELLO_HTTP=127.0.0.1:8080 \
    RITORNELLO_CONSOLE_TTY=/dev/stdout \
    RITORNELLO_RADIO_STATIONS=/tmp/rp/stations.toml RITORNELLO_RADIO_STATE=/tmp/rp/plugin-radio.json \
    cargo run -p ritornello-core

## Déploiement

    PI=pi@raspberrypi.local ./deploy/deploy.sh

`PI` désigne n'importe quel hôte SSH cible (Pi ou autre Linux), et `TARGET`
la cible de compilation (voir [Compiler](#compiler)) — les deux se
surchargent indépendamment, ex. `TARGET=x86_64-unknown-linux-gnu PI=user@host
./deploy/deploy.sh`.

Interface web : http://<hôte>:8080 — logs : `journalctl -u ritornello -f`.

## Télécommande

Si une touche ne répond pas : `sudo evtest` sur la machine cible, noter le
code de la touche, ajuster `crates/ritornello-plugin-mce/src/keymap.rs`.
