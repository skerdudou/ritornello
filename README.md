# radio-pi

Radio internet + lecteur CD sur Raspberry Pi 2, télécommande MCE, affichage
console HDMI (OLED SSD1306 prévu ensuite). Spec et plan dans `docs/superpowers/`.

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
`sink` ou `input`), le chemin de son exécutable, et un `admin_url` optionnel
affiché sur la page de statut du cœur (`http://<pi>:8080/status`).

- `radio-pi-plugin-radio` sert sa propre page de gestion des stations sur
  `http://<pi>:8081` (`stations.toml`, comme avant).
- La mort d'un plugin est tolérée : il est marqué indisponible sur la page de
  statut, les autres continuent de fonctionner.

## Développement (WSL)

    cargo test --workspace
    cargo build --workspace
    mkdir -p /tmp/rp
    cat > /tmp/rp/plugins.toml <<'PLUGINS'
    [[plugin]]
    name = "radio"
    kind = "source"
    exec = "target/debug/radio-pi-plugin-radio"
    PLUGINS
    cat > /tmp/rp/stations.toml <<'STATIONS'
    [[stations]]
    name = "FIP"
    url = "http://icecast.radiofrance.fr/fip-midfi.mp3"
    preset = 1
    STATIONS
    RADIO_PI_PLUGINS=/tmp/rp/plugins.toml RADIO_PI_STATE=/tmp/rp/state.json \
    RADIO_PI_MPV_SOCKET=/tmp/rp/mpv.sock RADIO_PI_TTY=/dev/stdout \
    RADIO_PI_HTTP=127.0.0.1:8080 RADIO_PI_RUNTIME_DIR=/tmp/rp \
    RADIO_PI_RADIO_STATIONS=/tmp/rp/stations.toml RADIO_PI_RADIO_STATE=/tmp/rp/plugin-radio.json \
    RADIO_PI_RADIO_HTTP=127.0.0.1:8081 \
    cargo run -p radio-pi-core

## Déploiement

    PI=pi@raspberrypi.local ./deploy/deploy.sh

Interface web : http://raspberrypi.local:8080 — logs : `journalctl -u radio-pi -f`.

## Télécommande

Si une touche ne répond pas : `sudo evtest` sur le Pi, noter le code de la
touche, ajuster `crates/radio-pi-plugin-mce/src/keymap.rs`.
