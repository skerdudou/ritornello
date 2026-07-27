# Les plugins

Architecture à plugins : le cœur (`ritornello-core`) orchestre des plugins —
processus séparés communiquant par socket Unix (protocole JSON par ligne) —
de quatre genres : **source** (contenu à jouer : radio, CD), **input**
(télécommande), **display** (affichage) et **metadata** (métadonnées du
morceau en cours). Chaque genre a une interface stable ; ajouter un nouveau
plugin (ex. une source Bluetooth, un afficheur OLED) ne touche pas au cœur.

`ritornello-core` charge `/etc/ritornello/plugins.toml` au démarrage (voir
`deploy/plugins.example.toml`) : chaque entrée déclare un plugin (`source`,
`display`, `input` ou `metadata`), le chemin de son exécutable, et peut
déclarer `admin = true` pour exposer une page d'admin servie par le cœur,
sous la même origine, avec un lien affiché sur l'accueil du cœur
(`http://<hôte>:8080/`).

La mort d'un plugin est tolérée : il est marqué indisponible sur la page de
statut, les autres continuent de fonctionner. Aucun de ces plugins n'est
spécifique au Pi : `ritornello-plugin-radio` et `ritornello-plugin-cd` sont
du Rust portable pur, `ritornello-plugin-generic-input` et
`ritornello-plugin-console` dépendent seulement de matériel Linux générique
(respectivement un récepteur infrarouge USB reconnu par `evdev`, et une
console `/dev/ttyN`) — pas d'un GPIO ou d'un bus propre au Pi.

## `ritornello-plugin-radio` — la radio internet

Déclare `admin = true` : sa page de gestion des stations est servie par le
cœur, sous l'origine unique, à `http://<hôte>:8080/plugins/radio/` (le plugin
ne lie aucun port). Elle permet de saisir une station à la main (nom + URL du
flux) **et** d'en ajouter une depuis l'annuaire communautaire en ligne
[Radio Browser](https://api.radio-browser.info) : taper un nom, choisir un
pays, « Rechercher », puis « Ajouter » sur un résultat. C'est **le plugin**
qui interroge l'annuaire — la page ne charge aucune ressource externe — et
rien n'est écrit tant qu'« Enregistrer » n'a pas été cliqué.

Les présélections sont numérotées **automatiquement par position** (1 à 9,
les chiffres de la télécommande) : ajouter met en fin de liste, supprimer
renumérote les suivantes ; au-delà de 9, l'ajout est refusé. L'ordre se
change **en glissant une ligne** (ou par les flèches ▲▼, qui restent le
chemin accessible au clavier et au doigt) : déplacer une station change donc
son chiffre de télécommande.

Le **pays** de recherche se choisit dans une liste filtrable au clavier,
peuplée par l'annuaire lui-même (241 pays au dernier relevé, avec le nombre
de stations de chacun). Les noms sont rendus par le navigateur depuis le code
ISO — pas de table de pays à traduire dans les packs de langue. La liste
n'est demandée qu'à l'ouverture du sélecteur, jamais au chargement de la
page, et le choix est **retenu par le plugin** (dans `plugin-radio.json`, à
côté de la présélection courante) : il suit l'appareil et non le navigateur.

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

Sélectionner une présélection **vide** affiche « présélection vide » quelques
secondes, puis l'affichage revient à la station qui joue : rien n'a été
lancé, donc rien ne s'est arrêté, et le message ne doit pas décrire
durablement un état qui n'existe pas.

Variables : `RITORNELLO_RADIO_STATIONS`, `RITORNELLO_RADIO_STATE`,
`RITORNELLO_RADIO_DIRECTORY` (**épingle** un serveur d'annuaire : il devient
le seul essayé, pour imposer son propre miroir sans recompiler ; non définie,
la liste intégrée s'applique).

## `ritornello-plugin-cd` — le lecteur CD

Détection du disque par ioctl (`RITORNELLO_CD_DEV`, défaut `/dev/sr0`),
lecture de la TOC via `cd-discid`, pistes suivante/précédente, éjection
(paquet `eject`). La reconnaissance de l'album ne vit **pas** ici : elle est
l'affaire du plugin `metadata` MusicBrainz (voir plus bas) — un appel réseau
de plusieurs secondes n'a rien à faire dans le processus qui répond aux
commandes de piste.

## `ritornello-plugin-console` — l'affichage

Plugin d'affichage sur console HDMI (variable `RITORNELLO_CONSOLE_TTY`,
défaut `/dev/tty1`). Trois lignes composées par le cœur ; les caractères de
contrôle venus du contenu (titres ICY…) sont filtrés avant écriture sur le
tty. Un futur afficheur (OLED SSD1306 en SPI/I2C, par exemple) serait un
nouveau plugin du même genre, sans règle de repli à réimplémenter.

La page de statut du cœur (`http://<hôte>:8080/status`) propose aussi un
sélecteur de **sortie audio**, basé sur les périphériques ALSA connus du
système (`aplay -L`) — une enceinte Bluetooth déjà appairée via
`bluetoothctl` y apparaîtra automatiquement une fois exposée par
`bluez-alsa`.

## `ritornello-plugin-generic-input` — les entrées

Déclare `admin = true` : il ouvre **tous** les périphériques evdev lisibles
(non exclusif : le clavier continue de fonctionner normalement) et traduit
les touches en commandes selon `/etc/ritornello/input-bindings.toml`. Sa page
`http://<hôte>:8080/plugins/generic-input/` liste les périphériques détectés,
permet d'apprendre une touche par action, de charger un preset livré (`mce`,
`keyboard`) et d'enregistrer ; elle permet aussi d'importer un preset depuis
un fichier `.toml` téléversé et d'exporter les bindings courants du
périphérique sélectionné vers un tel fichier. Variables :
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

## Métadonnées du morceau (genre `metadata`)

Un plugin `metadata` enrichit ce que joue la Source active **sans que
celle-ci le sache**. Le cœur lui annonce ce qui joue, il répond ce qu'il en
sait.

Deux couches se superposent, et la seconde gagne :

1. **Ce que le flux annonce lui-même.** Le cœur observe la propriété
   `metadata` de mpv et en lit l'en-tête ICY (`icy-title`), affiché **brut**,
   sans découpage sur `" - "` : la convention existe mais n'est pas
   garantie — les webradios OUI FM émettent d'ailleurs `Titre - ARTISTE`,
   dans l'ordre inverse de l'usage. Cette couche fonctionne sans aucun
   plugin, et sans que la Source ait à déclarer quoi que ce soit.
2. **Ce qu'un plugin `metadata` a appris**, s'il correspond à ce qui joue.

**Un plugin est prioritaire sur l'ICY en toutes circonstances**, tant que la
station ne change pas : ce qu'il a dit reste affiché même si le flux annonce
entre-temps un nouveau titre. L'ICY de ces flux est de moindre qualité —
ordre inversé (`Titre - ARTISTE`), parfois le seul nom de la station en
remplissage — et le laisser reprendre la main à chaque morceau faisait
changer la forme de l'affichage deux fois par morceau.

Compromis assumé : au changement de morceau, le titre précédent reste affiché
le temps que le plugin envoie sa trame — court en pratique, les deux venant
de la même automatisation de la station, mais durable si le plugin cesse de
répondre. Changer de station, en revanche, remet l'ardoise à zéro : c'est
l'identité qui change, et l'ICY reprend la main jusqu'à la première réponse
du plugin.

Sans plugin `metadata` déclaré, il n'y a donc pas d'enrichissement — c'est
assumé, ce n'est pas une régression. **La lecture n'est jamais affectée** par
un plugin `metadata`, et son échec est silencieux à l'écran. Un plugin dont
le processus meurt est marqué indisponible sur la page de statut ; en
revanche, un plugin qui démarre puis ne sert jamais sa socket y reste affiché
comme connecté (même comportement que le genre `input`, dont la connexion
n'est pas attendue au démarrage).

**L'ordre de déclaration compte**, et c'est le seul genre pour lequel c'est
le cas : entre deux plugins qui répondent pour le même morceau, le premier
déclaré dans `plugins.toml` gagne, et un plugin déclaré plus bas ne l'écrase
jamais. Le critère retenu est la prévisibilité pour qui débogue : « premier
arrivé » dépendrait de la latence réseau, donc la même installation
afficherait autre chose d'un démarrage à l'autre.

**Mise à jour d'une installation existante.** `deploy/deploy.sh` installe
les nouveaux binaires mais ne touche pas à `/etc/ritornello/plugins.toml` :
sans ajout manuel des deux entrées `kind = "metadata"` (voir
`deploy/plugins.example.toml`), un appareil déjà en service **perd les titres
de piste du CD**, que le plugin cd fournissait lui-même avant cette version.
Le reste de l'affichage est inchangé.

### Les deux plugins livrés

- `ritornello-plugin-musicbrainz` reconnaît un disque auprès de MusicBrainz.
  C'est le code qui vivait dans `ritornello-plugin-cd`, où un appel réseau de
  plusieurs secondes partageait le processus devant répondre aux commandes de
  piste. Aucune variable à régler.
- `ritornello-plugin-ouifm-metas` lit le flux de métadonnées des webradios
  OUI FM. **Rien à configurer** : la table des 21 flux est embarquée dans le
  binaire (`src/webradios.toml`), relevée de la source de vérité du site — la
  variable JavaScript `apidata` de sa page de lecteur, où chaque flux porte
  son identifiant de flux et son identifiant de métadonnées.
  `scripts/fetch-webradios.mjs` la régénère depuis cette même source (avec
  `--verifier`, il signale une dérive sans rien écrire).

  La reconnaissance porte sur un **fragment de l'URL** et non sur l'URL
  entière : celle qu'OUI FM sert comporte un jeton signé et un paramètre de
  format variables, alors que l'identifiant de flux, lui, est stable. Les
  **deux formes d'URL** d'une même webradio sont reconnues : celle de
  `streams.lesindesradios.fr` et le mount Icecast historique
  (`ouifm3.ice.infomaniak.ch/ouifm3.mp3`) — c'est cette seconde forme qu'on
  rencontre en pratique, publiée de longue date, donc référencée par les
  annuaires et recopiée par les utilisateurs.

  Le fichier optionnel `/etc/ritornello/ouifm-metas.toml` (variable
  `RITORNELLO_OUIFM_METAS`, exemple dans `deploy/`) sert le jour où OUI FM
  change quelque chose : ses entrées sont consultées **avant** la table
  embarquée, ce qui permet de corriger une correspondance devenue fausse ou
  d'en ajouter une, sans recompiler.

### Où cela s'affiche

Sur les afficheurs, le cœur compose : `line3` porte `artiste — titre` (avec
repli sur l'un des deux seul — une information partielle vaut mieux que
rien), et `line2` reçoit l'album **uniquement si la Source a déclaré sa
propre `line2` remplaçable**, c'est-à-dire l'a écrite faute de mieux. Le
plugin cd s'en sert : il écrit « audio CD », l'album prend la place quand un
plugin le rapporte, et l'étiquette revient dès qu'il ne le sait plus. Le
critère est cette déclaration explicite et non le fait que la ligne soit
vide : sinon une Source demanderait l'album en se taisant, et celle qui veut
une ligne vide n'aurait aucun moyen de le dire. Le cœur ne détruit jamais une
information que la Source seule possède, et le protocole Display reste
inchangé : un futur afficheur n'a aucune règle de repli à réimplémenter.

Dans l'IHM web, la page d'accueil porte un encart **Lecteur**, au-dessus de
la télécommande : source active, volume, et deux pastilles pour le muet et la
veille. Le morceau **s'y ajoute** quand on le connaît — avec une pastille
indiquant son **origine** (`icy`, ou le nom du plugin gagnant), la première
question qu'on se pose devant un titre faux. Rien de tout cela n'est sondé :
l'encart se met à jour en flux poussé, donc le volume suit la télécommande
infrarouge et les autres onglets.

**Avance automatique de piste.** Quand un CD passe seul à la piste suivante,
mpv en informe le cœur, qui le relaie à la Source
(`SourceReq::PlayerTrack`) : c'est elle qui se recale et renvoie vue et
identité, le cœur ne pouvant pas modifier une identité qu'il a pour principe
de ne jamais interpréter. L'affichage et les métadonnées suivent donc
l'avance sans qu'aucune touche soit pressée. La **fin du disque** suit le
même principe en sens inverse : le cœur signale l'arrêt à la Source, qui
recale son état — sans quoi la dernière piste resterait affichée
indéfiniment.

### Écrire un plugin `metadata`

Implémenter `MetadataPlugin` du SDK (`now_playing` / `next_enrichment`) et
appeler `run_metadata_plugin`. Deux points de contrat :

- l'**identité** de ce qui joue est un JSON **opaque** produit par la Source,
  que le cœur ne fait que comparer et relayer. Le plugin radio y met
  `{"kind":"stream","url":…}`, le plugin cd
  `{"kind":"disc","toc":…,"track":…}`. Un plugin qui ne reconnaît pas la
  forme reçue se contente de se taire ;
- chaque enrichissement doit **réécho l'identité** concernée. C'est le
  garde-fou de péremption : le cœur jette celui qui ne correspond plus à ce
  qui joue, ce qui empêche une réponse lente d'écraser le morceau suivant. Un
  enrichissement dont tous les champs textuels sont vides compte comme une
  non-réponse, et laisse donc gagner un plugin moins prioritaire.

`next_enrichment` doit être **annulable sans perte** : son futur est
abandonné dès qu'un `NowPlaying` arrive, donc tout état durable (connexion
HTTP ouverte, cache, file d'attente) doit vivre dans le plugin, pas dans les
variables locales du futur. (La même exigence vaut pour le
`poll_notification` des Sources, et pour la même raison — voir la doc du
SDK.)

## IHM d'un plugin

Un plugin qui déclare `admin = true` peut livrer sa propre interface, sans
qu'une ligne du cœur change. Il répond à trois requêtes du protocole
d'admin :

- `GetAsset("ui.js")` → un **module ESM** exportant `contract` (la version du
  contrat, voir `web/kit/src/contract.ts`) et, par défaut, un composant Vue ;
- `GetAsset("ui.css")` → la feuille de style du module (sa propre passe
  Tailwind, important : le CSS du cœur ne contient que les classes qu'il
  voit) ;
- `GetCatalog` → son catalogue i18n à plat, que la vue consomme via `t()`.

Le shell monte le composant par défaut du module en lui passant **deux
props**, qui sont l'intégralité du contrat côté données :

- `catalog` : le catalogue i18n à plat renvoyé par `GetCatalog`, à consommer
  via `createT(catalog)` ;
- `base` : le préfixe **absolu** sous lequel le cœur sert les routes de ce
  plugin, slash final compris (`/plugins/<nom>/`). Toute URL du module se
  construit à partir de lui — `api.get(`${base}api/data`)` — et **jamais** en
  relatif. Un `./api/data` est résolu contre l'URL du navigateur, pas contre
  le préfixe du plugin : sur `/plugins/<nom>` (sans slash final) il désigne
  `/plugins/api/data`, que le cœur interprète comme un plugin nommé « api »,
  donc un 404. Le routeur du shell canonise désormais l'URL, mais un module
  ne doit pas dépendre de cette forme : `base` est la garantie, l'URL
  affichée n'en est pas une. Les deux modules livrés déclarent `base`
  **requise**, sans valeur par défaut : le nom sous lequel un plugin est
  servi vient de `plugins.toml`, donc du déploiement, et un module qui
  reconstruirait `/plugins/<son-nom>/` serait faux — silencieusement — dès
  qu'un opérateur le déclare sous un autre nom.

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
  `RouterLink` — sa propre copie de `vue-router` embarquerait ses propres
  clés d'injection, incompatibles avec le routeur du shell.
- Le protocole d'admin ne transporte que du **texte** (`AdminResult::Asset {
  body: Option<String>, .. }`, voir `crates/ritornello-proto/src/admin.rs`) :
  un actif binaire (fonte, sprite, wasm) devrait être encodé en base64 par le
  plugin puis décodé côté module ESM. C'est un plafond assumé du relai, pas
  un oubli.
- Les actifs d'un plugin ne sont servis que sur **un seul segment de chemin**
  (`/plugins/<nom>/<fichier>`, sans sous-répertoire) : le build d'un plugin
  doit donc produire des noms de fichiers **plats**. Un chemin plus profond
  (ex. `/plugins/<nom>/assets/ui.js`) ne correspond à aucune route du cœur et
  répond **404**. Il tombait auparavant sur le repli de la SPA, qui renvoyait
  200 avec le shell HTML : un `import()` dynamique recevait du HTML, mode
  d'échec très déroutant puisque rien ne signalait l'erreur.
- Les polices déclarées par les thèmes du cœur (voir
  [interface.md](interface.md)) viennent d'un CDN, la seule ressource externe
  de toute l'interface ; un module de plugin qui voudrait ses propres polices
  devrait suivre la même logique de repli (police système hors ligne) plutôt
  que de bloquer le rendu.
