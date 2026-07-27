# ritornello

Radio internet + lecteur CD, télécommande configurable (evdev), affichage console HDMI (OLED
SSD1306 prévu ensuite). Spec et plan dans `docs/superpowers/`.

Architecture à plugins : le cœur (`ritornello-core`) orchestre des plugins —
processus séparés communiquant par socket Unix — de trois genres : **Source**
(contenu à jouer : radio, CD), **Input** (télécommande) et **Display**
(affichage). Chaque genre a une interface stable ; ajouter un nouveau
plugin (ex. une source Bluetooth, un afficheur OLED) ne touche pas au cœur.
Voir la section [Plugins](#plugins).

## Portabilité

Rien dans le code n'est spécifique au Raspberry Pi : la télécommande
passe par `evdev` (l'API Linux générique d'entrée, pas du GPIO), le son par
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
parcours complets (Playwright, chromium).

`npm run e2e -w app` a besoin d'un cœur compilé (`cargo build --workspace`) et
de `mpv` sur la machine qui exécute les parcours (lecture réelle par le plugin
radio). Sous Windows — l'environnement où npm/node/Playwright tournent dans ce
projet —, le binaire du cœur est un ELF Linux compilé sous WSL : le harnais
(`web/app/e2e/serve.mjs`) le lance donc via `wsl.exe`, pas directement, et
l'arrêt (`web/app/e2e/teardown.mjs`) doit cibler explicitement le processus
côté WSL, un `taskkill` Windows ne tuant que l'arbre de processus Windows. Sous
Linux natif, le même harnais lance le binaire directement.

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

## Plugins

`ritornello-core` charge `/etc/ritornello/plugins.toml` au démarrage (voir
`deploy/plugins.example.toml`) : chaque entrée déclare un plugin (`source`,
`display` ou `input`), le chemin de son exécutable, et peut déclarer
`admin = true` pour exposer une page d'admin servie par le cœur, sous la même
origine, avec un lien affiché sur l'accueil du cœur (`http://<pi>:8080/`).

- `ritornello-plugin-radio` déclare `admin = true` : sa page de gestion des
  stations est servie par le cœur, sous l'origine unique, à
  `http://<hôte>:8080/plugins/radio/` (le plugin ne lie plus aucun port). Elle
  permet de saisir une station à la main (nom + URL du flux) **et** d'en
  ajouter une depuis l'annuaire communautaire en ligne
  [Radio Browser](https://api.radio-browser.info) : taper un nom, choisir un
  pays (France, États-Unis, tous), « Rechercher », puis « Ajouter » sur un
  résultat. C'est **le plugin** qui interroge l'annuaire — la page ne charge
  aucune ressource externe — et rien n'est écrit tant qu'« Enregistrer » n'a
  pas été cliqué. Les présélections sont numérotées **automatiquement par
  position** (1 à 9, les chiffres de la télécommande) : ajouter met en fin de
  liste, supprimer renumérote les suivantes ; au-delà de 9, l'ajout est refusé.
  Annuaire injoignable ⇒ message d'erreur sur la page, la lecture en cours et
  les stations déjà configurées ne bougent pas, et la saisie manuelle reste le
  repli. L'annuaire est interrogé sur **plusieurs serveurs essayés dans
  l'ordre** (`de1`, `de2`, `at1`, `nl1`, `fi1` de `api.radio-browser.info`)
  jusqu'à ce que l'un réponde : `all.api.radio-browser.info` est un
  enregistrement tournant, et le parc de miroirs bouge avec le temps — un hôte
  disparu échoue vite, le suivant est essayé, et chaque échec est journalisé.
  L'ensemble tient dans un **budget de 4 s** (2 s au plus par serveur) : la
  page d'admin passe par le protocole d'admin du cœur, qui abandonne toute
  requête au bout de 5 s, donc une recherche qui traîne est arrêtée d'elle-même
  avec un message d'erreur plutôt que de finir en timeout.
  Variables : `RITORNELLO_RADIO_STATIONS`, `RITORNELLO_RADIO_STATE`,
  `RITORNELLO_RADIO_DIRECTORY` (**épingle** un serveur d'annuaire : il devient
  le seul essayé, pour imposer son propre miroir sans recompiler ; non
  définie, la liste intégrée s'applique).
- La mort d'un plugin est tolérée : il est marqué indisponible sur la page de
  statut, les autres continuent de fonctionner.
- Aucun de ces plugins n'est spécifique au Pi : `ritornello-plugin-radio` et
  `ritornello-plugin-cd` sont du Rust portable pur, `ritornello-plugin-generic-input` et
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
- `ritornello-plugin-generic-input` déclare `admin = true` : il ouvre **tous**
  les périphériques evdev lisibles (non exclusif : le clavier continue de
  fonctionner normalement) et traduit les touches en commandes selon
  `/etc/ritornello/input-bindings.toml`. Sa page
  `http://<hôte>:8080/plugins/generic-input/` liste les périphériques
  détectés, permet d'apprendre une touche par action, de charger un preset
  livré (`mce`, `keyboard`) et d'enregistrer ; elle permet aussi d'importer un
  preset depuis un fichier `.toml` téléversé et d'exporter les bindings
  courants du périphérique sélectionné vers un tel fichier. Variables :
  `RITORNELLO_INPUT_BINDINGS`, `RITORNELLO_INPUT_PRESETS`, `RITORNELLO_LOCALE`.
  **Mise à jour d'une installation existante** (ancien `ritornello-plugin-mce`
  à clavier codé en dur) : dans `/etc/ritornello/plugins.toml`, remplacer
  l'entrée du plugin par `name = "generic-input"`, `exec =
  "/usr/local/lib/ritornello/plugins/ritornello-plugin-generic-input"` et
  **ne pas oublier `admin = true`** — sans elle le plugin démarre quand même
  (mode dégradé, moitié Input seule) mais sa page d'admin n'est pas servie.
  `deploy/deploy.sh` supprime automatiquement l'ancien binaire
  `ritornello-plugin-mce` sur la cible pour éviter qu'il continue de tourner
  après une mise à jour.

### IHM d'un plugin

Un plugin qui déclare `admin = true` peut livrer sa propre interface, sans
qu'une ligne du cœur change. Il répond à trois requêtes du protocole d'admin :

- `GetAsset("ui.js")` → un **module ESM** exportant `contract` (la version du
  contrat, voir `web/kit/src/contract.ts`) et, par défaut, un composant Vue ;
- `GetAsset("ui.css")` → la feuille de style du module (sa propre passe
  Tailwind, important : le CSS du cœur ne contient que les classes qu'il voit) ;
- `GetCatalog` → son catalogue i18n à plat, que la vue consomme via `t()`.

Le shell monte le composant par défaut du module en lui passant **deux props**,
qui sont l'intégralité du contrat côté données :

- `catalog` : le catalogue i18n à plat renvoyé par `GetCatalog`, à consommer via
  `createT(catalog)` ;
- `base` : le préfixe **absolu** sous lequel le cœur sert les routes de ce
  plugin, slash final compris (`/plugins/<nom>/`). Toute URL du module se
  construit à partir de lui — `api.get(`${base}api/data`)` — et **jamais** en
  relatif. Un `./api/data` est résolu contre l'URL du navigateur, pas contre le
  préfixe du plugin : sur `/plugins/<nom>` (sans slash final) il désigne
  `/plugins/api/data`, que le cœur interprète comme un plugin nommé « api »,
  donc un 404. Le routeur du shell canonise désormais l'URL, mais un module ne
  doit pas dépendre de cette forme : `base` est la garantie, l'URL affichée n'en
  est pas une. Les deux modules livrés déclarent `base` **requise**, sans valeur
  par défaut : le nom sous lequel un plugin est servi vient de `plugins.toml`,
  donc du déploiement, et un module qui reconstruirait `/plugins/<son-nom>/`
  serait faux — silencieusement — dès qu'un opérateur le déclare sous un autre
  nom.

Le module importe `vue` et `@ritornello/ui` **sans les embarquer** : le shell
les fournit par une import map, donc une seule instance de Vue et un seul jeu
de composants servent tout le monde. Un contrat incompatible est signalé dans
l'interface plutôt que de casser la page.

L'ESM natif ne demande aucune compilation : un plugin simple peut livrer un
`ui.js` **écrit à la main**. Les deux plugins livrés utilisent un build Vite
(voir `crates/ritornello-plugin-radio/ui/`) pour bénéficier des `.vue` et de
TypeScript — c'est un choix de confort, pas une exigence.

Quatre points appris pendant ce chantier, à connaître avant d'écrire l'IHM
d'un plugin tiers :

- `assets/vue.js` est le build **runtime-only** de Vue (pas de compilateur de
  template embarqué) : un module de plugin doit livrer des **templates
  précompilés** (SFC `.vue` passés par `@vitejs/plugin-vue`, ou `h()` à la
  main), jamais un `template: "<div>...</div>"` en chaîne évaluée à
  l'exécution — ça échouerait silencieusement à l'exécution, pas à la
  construction. `vue-router` n'est, quant à lui, **délibérément pas** dans
  l'import map : un module de plugin ne doit pas utiliser `useRoute` ni
  `RouterLink` — sa propre copie de `vue-router` embarquerait ses propres clés
  d'injection, incompatibles avec le routeur du shell.
- Le protocole d'admin ne transporte que du **texte** (`AdminResult::Asset {
  body: Option<String>, .. }`, voir `crates/ritornello-proto/src/admin.rs`) :
  un actif binaire (fonte, sprite, wasm) devrait être encodé en base64 par le
  plugin puis décodé côté module ESM. C'est un plafond assumé du relai, pas un
  oubli.
- Les actifs d'un plugin ne sont servis que sur **un seul segment de chemin**
  (`/plugins/<nom>/<fichier>`, sans sous-répertoire) : le build d'un plugin
  doit donc produire des noms de fichiers **plats**. Un chemin plus profond
  (ex. `/plugins/<nom>/assets/ui.js`) ne correspond à aucune route du cœur et
  répond **404**. Il tombait auparavant sur le repli de la SPA, qui renvoyait
  200 avec le shell HTML : un `import()` dynamique recevait du HTML, mode
  d'échec très déroutant puisque rien ne signalait l'erreur.
- Les polices déclarées par les thèmes du cœur (voir [Thème](#thème)) viennent
  d'un CDN, la seule ressource externe de toute l'interface ; un module de
  plugin qui voudrait ses propres polices devrait suivre la même logique de
  repli (police système hors ligne) plutôt que de bloquer le rendu.

## Télécommande web

L'accueil (`http://<hôte>:8080/`) embarque une télécommande : les 11
commandes du protocole (présélections 1-9, suivant/précédent, volume, muet,
lecture/pause, stop, éjecter, changement de source, veille).

`Next`/`Prev` sont interprétées par la source active : présélection pour la
radio, piste pour le lecteur CD — ce n'est pas deux paires de commandes
distinctes, seulement une sémantique qui varie selon la source. Un binding
qui référence encore `NextTrack` ou `PrevTrack` (ancien nom) n'est plus
valide : il doit être réécrit en `Next`/`Prev`.

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

## Thème

L'interface propose une bascule **clair/sombre** et un sélecteur ouvrant une
popin avec les **42 thèmes** de [tweakcn](https://tweakcn.com) (Apache-2.0).
C'est un réglage **de l'appareil**, comme la langue : il est persisté dans
`state.json` (champs `theme` et `mode`) et s'applique donc à tous les
navigateurs qui consultent l'interface. Défaut : `northern-lights`, mode clair.

Les polices déclarées par les thèmes sont chargées depuis un CDN — la seule
ressource externe de l'interface. Hors ligne, l'affichage retombe sur la police
système sans autre conséquence.

Régénérer les presets depuis l'amont :
`cd web/kit && node scripts/fetch-presets.mjs`.

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
    admin = true

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

Le plugin `generic-input` peut être ajouté au `plugins.toml` de `/tmp/rp` :

    [[plugin]]
    name = "generic-input"
    kind = "input"
    exec = "target/debug/ritornello-plugin-generic-input"
    admin = true

et les variables suivantes ajoutées à la ligne d'environnement :

    RITORNELLO_INPUT_BINDINGS=/tmp/rp/input-bindings.toml RITORNELLO_INPUT_PRESETS=deploy/input-presets

## Déploiement

    PI=pi@raspberrypi.local ./deploy/deploy.sh

`PI` désigne n'importe quel hôte SSH cible (Pi ou autre Linux), et `TARGET`
la cible de compilation (voir [Compiler](#compiler)) — les deux se
surchargent indépendamment, ex. `TARGET=x86_64-unknown-linux-gnu PI=user@host
./deploy/deploy.sh`.

Interface web : http://<hôte>:8080 — logs : `journalctl -u ritornello -f`.

## Télécommande

Si une touche ne répond pas, ouvrir `http://<hôte>:8080/plugins/generic-input/`,
choisir le périphérique dans la liste (bouton « Rafraîchir » s'il vient d'être
branché), cliquer « Apprendre » sur la ligne de l'action, appuyer sur la touche,
puis « Enregistrer ». Aucun redémarrage n'est nécessaire : la table est relue à
chaque appui. Pour partir d'une base, charger le preset `mce` ou `keyboard`.
