# radio-pi

Radio internet + lecteur CD sur Raspberry Pi 2, télécommande MCE, affichage
console HDMI (OLED SSD1306 prévu ensuite). Spec et plan dans `docs/superpowers/`.

## Préparation du Pi (une fois)

Raspberry Pi OS Lite, puis :

    sudo apt install mpv cd-discid eject
    sudo cp deploy/stations.example.toml /etc/radio-pi/stations.toml
    # jack analogique en sortie par défaut + volume matériel à fond
    sudo raspi-config nonint do_audio 1
    amixer set PCM 100%

Wifi : `sudo raspi-config` (System Options > Wireless LAN).

## Développement (WSL)

    cargo test
    RADIO_PI_STATIONS=/tmp/rp/stations.toml RADIO_PI_STATE=/tmp/rp/state.json \
    RADIO_PI_MPV_SOCKET=/tmp/rp/mpv.sock RADIO_PI_TTY=/dev/stdout \
    RADIO_PI_HTTP=127.0.0.1:8080 cargo run

## Déploiement

    PI=pi@raspberrypi.local ./deploy/deploy.sh

Interface web : http://raspberrypi.local:8080 — logs : `journalctl -u radio-pi -f`.

## Télécommande

Si une touche ne répond pas : `sudo evtest` sur le Pi, noter le code de la
touche, ajuster `src/keymap.rs`.
