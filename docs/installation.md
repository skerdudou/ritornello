# Installation et exploitation

## Portabilité

Rien dans le code n'est spécifique au Raspberry Pi : la télécommande passe
par `evdev` (l'API Linux générique d'entrée, pas du GPIO), le son par
ALSA/mpv, l'IPC par sockets Unix — tout ça tourne sur n'importe quel Linux,
x86_64 comme ARM. Le Pi 2 est le matériel de référence historique de ce
projet, pas une contrainte technique — les exemples ci-dessous en sont une
simple illustration.

## Compiler

L'interface web est une SPA (Vue 3 + shadcn-vue) embarquée dans le binaire du
cœur : **Node 20+** est donc un prérequis de développement, là où `cargo`
suffisait. La procédure de référence est `deploy/build.sh`, qui enchaîne
toujours les trois étapes dans cet ordre :

    ./deploy/build.sh                 # npm, puis cargo x86_64, puis cross ARM
    TARGET=aarch64-unknown-linux-gnu ./deploy/build.sh

Le build npm ne tourne qu'une fois : son livrable est lu à la compilation par
les deux étapes cargo. C'est ce qui permet à `cross` de fonctionner avec une
image Docker sans Node.

Un `cargo build` lancé seul, sans avoir construit l'IHM, **réussit** : un
bouchon est embarqué à la place, et la page servie invite à lancer
`npm run build --workspaces`. Ce n'est pas une panne. Les tests
(`cargo test --workspace`) restent verts dans cette situation ; côté
navigateur, `npm test --workspaces` couvre l'IHM et `npm run e2e -w app` les
parcours complets (voir [developpement.md](developpement.md)).

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
    sudo cp -r deploy/input-presets /etc/ritornello/input-presets
    sudo cp deploy/input-bindings.example.toml /etc/ritornello/input-bindings.toml
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
    sudo cp -r deploy/input-presets /etc/ritornello/input-presets
    sudo cp deploy/input-bindings.example.toml /etc/ritornello/input-bindings.toml

`deploy/deploy.sh` fonctionne à l'identique : `TARGET=x86_64-unknown-linux-gnu
PI=user@host ./deploy/deploy.sh` (pas besoin de `cross`/Docker pour cette
cible si la machine qui compile est déjà x86_64 — `cargo build` natif suffit
alors, `cross` reste surtout utile pour changer d'architecture).

## Déploiement

    PI=pi@raspberrypi.local ./deploy/deploy.sh

`PI` désigne n'importe quel hôte SSH cible (Pi ou autre Linux), et `TARGET`
la cible de compilation (voir [Compiler](#compiler)) — les deux se
surchargent indépendamment, ex. `TARGET=x86_64-unknown-linux-gnu PI=user@host
./deploy/deploy.sh`. Le script enchaîne `build.sh` (donc l'IHM npm **puis**
la cross-compilation — l'ordre garantit que la SPA embarquée est fraîche),
copie les binaires, les packs de langue et les presets, installe l'unité
systemd et redémarre le service.

Interface web : http://<hôte>:8080 — logs : `journalctl -u ritornello -f`.

`deploy.sh` installe les binaires mais **ne touche jamais** à
`/etc/ritornello/plugins.toml` : à la première installation, le provisionner
depuis `deploy/plugins.example.toml` (voir les exemples ci-dessus) ; lors
d'une mise à jour qui introduit de nouveaux plugins, y ajouter les entrées à
la main (voir [plugins.md](plugins.md)).

## Microcoupures audio

Deux tampons distincts protègent la lecture, et ils ne traitent pas le même
problème. Les confondre fait perdre du temps.

| Variable | Défaut | Ce qu'elle protège |
|---|---|---|
| `RITORNELLO_AUDIO_BUFFER` | `0.2` | la **sortie** : une échéance d'écriture ALSA manquée parce que la machine était occupée |
| `RITORNELLO_NETWORK_READAHEAD` | `1` | l'**entrée** : une gigue réseau qui vide l'avance de lecture d'un flux internet |

Les deux sont en secondes et s'appliquent au lancement de mpv
(`--audio-buffer` et `--demuxer-readahead-secs`). Les défauts sont **ceux de
mpv** : sans variable définie, la lecture se comporte exactement comme si ces
options n'étaient pas passées. Une valeur illisible ou hors bornes est ignorée
avec un avertissement dans les logs, sans empêcher le démarrage.

Avant de tourner une molette, **identifier laquelle** — les deux symptômes
s'entendent pareil mais ne se soignent pas au même endroit :

    mpv --no-video --msg-level=ao=v,cache=v <url-de-la-station> 2>&1 \
      | grep -iE "underrun|buffering|cache"

Des `underrun` désignent la sortie : monter `RITORNELLO_AUDIO_BUFFER`, par
exemple à `0.5`. Des `buffering` désignent l'entrée : monter
`RITORNELLO_NETWORK_READAHEAD`, par exemple à `10`, voire `30` sur une liaison
capricieuse — dix secondes de MP3 à 128 kbit/s pèsent environ 160 Ko,
négligeable même sur 1 Go de RAM.

Un cas à écarter d'emblée : en développement sous **WSL**, l'audio traverse le
pont WSLg vers Windows, dont la gigue propre produit des microcoupures que
ces deux réglages ne corrigeront pas. Ne conclure sur les tampons qu'après
avoir écouté sur la machine cible.

Augmenter le tampon de **sortie** aide contre les coupures dues à la charge de
la machine, au prix d'une latence d'autant sur la prise en compte du volume ou
du muet. **Le réduire aggrave les coupures** : c'est le sens de la variation,
pas son ampleur, qui compte.

Pour distinguer les deux causes, `journalctl -u ritornello -f` pendant une
coupure : mpv journalise le vidage du cache réseau, pas les sous-alimentations
d'ALSA.
