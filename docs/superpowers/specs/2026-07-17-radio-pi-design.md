# radio-pi — Design

Radio internet + lecteur CD autonome sur Raspberry Pi 2, piloté par un programme Rust
lancé au démarrage. Usage type « radio de cuisine » : on l'allume, ça joue.

Date : 2026-07-17 — Statut : validé

## Décisions de cadrage

| Sujet | Décision |
|---|---|
| Philosophie | Rust comme chef d'orchestre ; l'audio est délégué à un démon éprouvé |
| Matériel | Raspberry Pi 2 + dongle wifi USB + lecteur CD USB |
| Sortie audio | Jack analogique du Pi, via ALSA |
| Backend audio | **mpv** piloté en JSON-IPC (socket Unix) — un seul lecteur pour flux web et `cdda://` |
| Télécommande | MCE avec son récepteur USB (vu comme clavier → lecture evdev, pas de lirc) |
| Affichage | v1 : console texte sur écran HDMI (le temps des tests/mise en place) ; ensuite OLED SSD1306 128×64 en I2C — mode, station, titre, piste, volume |
| Mode CD | CD audio (CDDA) + reconnaissance des titres via MusicBrainz (DiscID) |
| Présélections | Gérées par une petite interface web embarquée (téléphone/PC du réseau local) |
| Bluetooth | Hors périmètre v1 ; l'architecture des modes doit permettre de l'ajouter plus tard |
| Démarrage | Reprend le dernier mode/station/volume ; premier boot = Radio, présélection 1 |
| OS | Raspberry Pi OS Lite (armhf), service systemd |

## Vue d'ensemble

Un binaire Rust unique (`radio-pi`), service systemd `Restart=always`, logs vers journald.
Il orchestre :

- **mpv** : lecture audio (flux http/https des radios, `cdda://` pour les CD),
  supervisé par le programme (spawn, respawn en cas de crash) ;
- **evdev** : touches de la télécommande MCE via `/dev/input` ;
- **I2C** : écran OLED SSD1306 ;
- **HTTP** : mini serveur web (axum) pour gérer les stations ;
- **fichiers** : configuration des stations + état persisté.

## Modes et commandes télécommande

Deux modes en v1, derrière une abstraction commune (l'ajout futur d'un mode
Bluetooth ne doit toucher ni l'input, ni le display, ni le player).

- Commun : `vol+ / vol− / mute`, bouton de bascule Radio ↔ CD, power (arrêt lecture / reprise).
- Radio : chiffres 1-9 = présélections directes ; `ch+ / ch−` = station suivante/précédente.
- CD : `play/pause`, `next/prev` piste, `stop`, éjection.

## Composants

Tâches async (tokio) communiquant par channels ; chaque composant expose un trait
pour être mocké en test.

1. **player** — supervise mpv : lance le process avec la socket IPC, envoie les
   commandes (loadfile, pause, volume…), observe les propriétés (`media-title` /
   métadonnées ICY, fin de piste, idle). Respawn de mpv s'il meurt ; relance du
   flux radio avec backoff si la lecture s'interrompt (wifi pas encore prêt au
   boot, micro-coupures réseau).
2. **input** — lit le récepteur MCE en evdev, traduit les codes touche en
   commandes métier.
3. **display** — derrière le trait `Display`, deux backends sélectionnés par la
   config :
   - `console` (v1) : texte formaté sur la console HDMI du Pi (`/dev/tty1`,
     l'unité systemd s'y attache) — suffisant pour les tests et la mise en place ;
   - `ssd1306` (plus tard) : OLED en I2C (crates `ssd1306` + `embedded-graphics`
     + `linux-embedded-hal`), avec titre défilant si trop long.

   Contenu identique dans les deux cas : mode courant, nom de station, titre en
   cours, numéro/titre de piste CD, affichage éphémère du volume, messages
   d'état (« connexion… », « CD illisible »).
4. **web** — axum ; page unique embarquée dans le binaire (HTML/JS statique) +
   API JSON : lister/ajouter/modifier/supprimer/réordonner les stations
   (nom, URL de flux, numéro de présélection 1-9). Écrit `stations.toml` et
   notifie le cœur du programme.
5. **cd** — détecte l'insertion/retrait du disque (`/dev/sr0`), lit la TOC,
   calcule le DiscID MusicBrainz, interroge l'API MusicBrainz pour obtenir
   artiste/album/titres ; repli « Piste N » hors ligne. La lecture elle-même
   passe par mpv (`cdda://`).
6. **state** — machine à états centrale (mode courant, station courante, piste,
   volume) ; persiste `state.json` à chaque changement pour la reprise au boot.

## Fichiers

- `/etc/radio-pi/stations.toml` — présélections (éditable aussi à la main en SSH).
- `/var/lib/radio-pi/state.json` — dernier mode, station, volume.

## Gestion d'erreurs

| Situation | Comportement |
|---|---|
| Wifi pas prêt au boot / flux injoignable | « connexion… » sur l'OLED, retries avec backoff |
| Flux qui coupe en cours de lecture | relance automatique du flux |
| Crash de mpv | respawn transparent, reprise de la lecture en cours |
| MusicBrainz inaccessible | affichage « Piste N » |
| CD illisible / pas de disque | message sur l'OLED, on reste en mode CD |
| `stations.toml` invalide | garder la dernière config valide en mémoire, signaler dans l'IHM web |

## Tests

- Logique pure en tests unitaires : machine à états, mapping télécommande,
  parsing/écriture de `stations.toml`, calcul de DiscID, découpage des trames IPC mpv.
- Traits `Player`, `Display`, `Input` mockés pour tester le cœur sans matériel.
- Matériel (I2C, evdev, son, lecteur CD) : validation manuelle sur le Pi.

## Développement et déploiement

- Cross-compilation `armv7-unknown-linux-gnueabihf` (outil `cross` sous WSL/Docker) ;
  le build natif sur Pi 2 serait trop lent.
- Sur le Pi : paquets `mpv` et `libcdio` ; I2C activé (`raspi-config`) ; unité
  systemd installée par un petit script de déploiement (scp + restart).
