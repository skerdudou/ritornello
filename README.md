# radio-pi

Radio internet + lecteur CD, télécommande MCE, affichage console HDMI (OLED
SSD1306 prévu ensuite). Spec et plan dans `docs/superpowers/`.

Architecture à plugins : le cœur (`radio-pi-core`) orchestre des plugins —
processus séparés communiquant par socket Unix — de trois genres : **Source**
(contenu à jouer : radio, CD), **Input** (télécommande) et **Display**
(affichage). Chaque genre a une interface stable ; ajouter un nouveau
plugin (ex. une source Bluetooth, un afficheur OLED) ne touche pas au cœur.
Voir la section [Plugins](#plugins).

## Portabilité

Rien dans le code n'est spécifique au Raspberry Pi : la télécommande MCE
passe par `evdev` (l'API Linux générique d'entrée, pas du GPIO), le son par
ALSA/mpv, l'IPC par sockets Unix — tout ça tourne sur n'importe quel Linux.
Le Pi 2 est le matériel de référence de ce projet, pas une contrainte
technique : c'est `deploy/deploy.sh` qui cible en dur `armv7-unknown-linux-gnueabihf`
pour cross-compiler vers cette architecture précise. Pour déployer sur un
autre Linux (x86_64, un autre SBC ARM 64 bits...), changer cette cible de
compilation suffit ; le reste (Cargo workspace, plugins, protocole) est
indépendant de la plateforme.

## Préparation du Pi (une fois)

Raspberry Pi OS Lite, puis :

    sudo apt install mpv cd-discid eject
    sudo cp deploy/stations.example.toml /etc/radio-pi/stations.toml
    sudo cp deploy/plugins.example.toml /etc/radio-pi/plugins.toml
    # jack analogique en sortie par défaut + volume matériel à fond
    sudo raspi-config nonint do_audio 1
    amixer set PCM 100%

Wifi : `sudo raspi-config` (System Options > Wireless LAN).

## Plugins

`radio-pi-core` charge `/etc/radio-pi/plugins.toml` au démarrage (voir
`deploy/plugins.example.toml`) : chaque entrée déclare un plugin (`source`,
`display` ou `input`), le chemin de son exécutable, et un `admin_url` optionnel
affiché sur la page de statut du cœur (`http://<pi>:8080/status`).

- `radio-pi-plugin-radio` sert sa propre page de gestion des stations sur
  `http://<pi>:8081` (`stations.toml`, comme avant).
- La mort d'un plugin est tolérée : il est marqué indisponible sur la page de
  statut, les autres continuent de fonctionner.
- Aucun de ces plugins n'est spécifique au Pi : `radio-pi-plugin-radio` et
  `radio-pi-plugin-cd` sont du Rust portable pur, `radio-pi-plugin-mce` et
  `radio-pi-plugin-console` dépendent seulement de matériel Linux générique
  (respectivement un récepteur infrarouge USB reconnu par `evdev`, et une
  console `/dev/ttyN`) — pas d'un GPIO ou d'un bus propre au Pi. Un nouveau
  plugin (Bluetooth, écran OLED SPI/I2C...) s'ajoute sans toucher au cœur ni
  aux plugins existants, du moment qu'il respecte l'interface Source/Input/
  Display et parle le protocole JSON par ligne sur socket Unix.
- `radio-pi-plugin-console` est le plugin d'affichage (console HDMI, variable
  `RADIO_PI_CONSOLE_TTY`, défaut `/dev/tty1`). La page de statut du cœur
  (`http://<pi>:8080/status`) propose aussi un sélecteur de sortie audio,
  basé sur les périphériques ALSA connus du système (`aplay -L`) — une
  enceinte Bluetooth déjà appairée via `bluetoothctl` y apparaîtra
  automatiquement une fois exposée par `bluez-alsa`.

## Développement (WSL)

    cargo test --workspace
    cargo build --workspace
    mkdir -p /tmp/rp
    cat > /tmp/rp/plugins.toml <<'PLUGINS'
    [[plugin]]
    name = "radio"
    kind = "source"
    exec = "target/debug/radio-pi-plugin-radio"

    [[plugin]]
    name = "console"
    kind = "display"
    exec = "target/debug/radio-pi-plugin-console"
    PLUGINS
    cat > /tmp/rp/stations.toml <<'STATIONS'
    [[stations]]
    name = "FIP"
    url = "http://icecast.radiofrance.fr/fip-midfi.mp3"
    preset = 1
    STATIONS
    RADIO_PI_PLUGINS=/tmp/rp/plugins.toml RADIO_PI_STATE=/tmp/rp/state.json \
    RADIO_PI_MPV_SOCKET=/tmp/rp/mpv.sock RADIO_PI_RUNTIME_DIR=/tmp/rp \
    RADIO_PI_HTTP=127.0.0.1:8080 \
    RADIO_PI_CONSOLE_TTY=/dev/stdout \
    RADIO_PI_RADIO_STATIONS=/tmp/rp/stations.toml RADIO_PI_RADIO_STATE=/tmp/rp/plugin-radio.json \
    RADIO_PI_RADIO_HTTP=127.0.0.1:8081 \
    cargo run -p radio-pi-core

## Déploiement

    PI=pi@raspberrypi.local ./deploy/deploy.sh

Interface web : http://raspberrypi.local:8080 — logs : `journalctl -u radio-pi -f`.

## Télécommande

Si une touche ne répond pas : `sudo evtest` sur le Pi, noter le code de la
touche, ajuster `crates/radio-pi-plugin-mce/src/keymap.rs`.
