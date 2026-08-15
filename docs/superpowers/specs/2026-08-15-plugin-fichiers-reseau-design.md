# Lecture de fichiers audio depuis un partage réseau — design

**Date :** 2026-08-15
**État :** validé par le propriétaire, prêt pour le plan d'implémentation.
**Base :** `main` à `b3cca84` (onglet Système, « afficheurs, état structuré » et
i18n des messages serveur fusionnés).

**Convention héritée du chantier i18n**, à respecter partout ici : une
validation reste une **fonction pure rendant une erreur typée**, et c'est la
frontière HTTP qui résout `message(&Catalog)`. `Display` ne sert qu'aux
journaux, en anglais. Un test résout chaque variante contre le catalogue
anglais réellement embarqué et **refuse un message égal à sa propre clé** —
`Catalog::get` rendant la clé quand il ne la trouve pas, une faute de frappe
afficherait sinon `bad_share_name` à l'écran sans qu'aucun test ne bronche.

## Le besoin

Lire des fichiers audio rangés sur un NAS, atteint par un partage SMB **qui
demande une authentification**, et sur des supports locaux (disque USB,
répertoire de l'appareil). Constituer une liste de lecture en y ajoutant des
**répertoires entiers, récursivement**. **Enregistrer et charger** des listes,
au format **m3u**, dans le stockage interne comme sur le partage.

## Décisions

Prises avec le propriétaire avant rédaction :

1. Le partage se déclare **depuis la page web** du plugin — adresse,
   identifiants — et non par une étape d'installation à la main.
2. Les **chiffres de la télécommande désignent les pistes** de la liste en
   cours, comme le plugin cd, et non les listes enregistrées.
3. Entrent dans la première version : les titres issus des tags, la reprise
   après redémarrage, la recherche dans l'arborescence, et le parcours des
   **fichiers locaux** au même titre que le réseau.
4. N'entrent pas : lecture aléatoire et répétition.
5. Le **m3u** est le format d'échange, au minimum.

## 1. Ce que le banc d'essai a établi

Trois hypothèses de conception reposaient sur le comportement de mpv face à un
`.m3u`. Elles ont été mesurées (mpv 0.37, banc jetable : trois mp3 de deux
secondes, un m3u, dialogue JSON-IPC) plutôt que supposées, et **la principale
était fausse**.

| Question | Mesure |
|---|---|
| mpv déplie-t-il un `.m3u` de façon asynchrone ? | **Non.** `playlist-count = 3` immédiatement après `loadfile`. |
| `set playlist-pos n` enchaîné aussitôt après `loadfile` atterrit-il juste ? | **Oui**, sur la bonne piste. |
| L'avance de piste ressemble-t-elle à celle du disque ? | **Oui** : un `property-change` de `playlist-pos` par piste. |
| Que fait mpv en fin de liste ? | `end-file eof`, puis **`idle`** avec `playlist-pos = -1`. |
| La propriété `metadata` porte-t-elle les tags d'un fichier local ? | **Oui** : `{"title":…,"artist":…,"album":…}`. |

Deux conséquences directes :

- **il n'y a pas de course à contourner**. Un `Play` portant un index de départ
  suffit : pas de m3u pivoté, pas de seconde action, et surtout aucun
  assouplissement du principe « le cœur seul décide de ce qui se met en
  lecture » (`Notification` est volontairement sans action) ;
- **la fin de liste tombe dans le piège de la relance**. Le cœur pose
  `expecting_stream = !uri.starts_with("cdda://")` (`core.rs:815`) : pour un
  chemin de fichier, `expecting_stream` serait vrai, et l'inactivité de mpv en
  fin de liste déclencherait la relance exponentielle au lieu de l'arrêt
  propre. C'est mesuré, pas redouté.

## 2. Protocole Source : `Play` enrichi

`SourceAction::Play` gagne deux champs, tous deux strictement additifs — une
trame émise aujourd'hui par le plugin radio reste **identique octet pour
octet**.

```rust
pub enum SourceAction {
    Noop,
    Play {
        uri: String,
        /// Index de départ dans la liste que `uri` désigne, quand c'en est une.
        ///
        /// Absent = « commence au début », le comportement historique. Le cœur
        /// applique `playlist-pos` juste après `loadfile` : mesuré comme fiable,
        /// mpv résolvant un `.m3u` dès la commande (voir §1).
        ///
        /// C'est l'unique moyen pour une Source de reprendre une liste à la
        /// piste n — que ce soit sur un chiffre de la télécommande ou à la
        /// reprise après redémarrage.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<i64>,
        /// Ce que `uri` désigne a une **fin normale** : un disque, une liste de
        /// fichiers. Quand mpv devient inactif, c'est la fin du contenu, pas
        /// une coupure de flux à relancer.
        ///
        /// Absent (= `false`) veut dire « flux live », le comportement
        /// historique : c'est ce qui garde les trames de la radio inchangées.
        /// Remplace le reniflage `uri.starts_with("cdda://")` du cœur, qui
        /// devinait ce que seule la Source sait.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        finite: bool,
    },
    Stop,
    PlayerNext,
    PlayerPrev,
}
```

Côté cœur, dans `appliquer_action` :

```rust
SourceAction::Play { uri, start, finite } => {
    self.expecting_stream = !finite;          // le reniflage `cdda://` disparaît
    self.player.play(&uri).await?;
    if let Some(n) = start {
        self.player.set_playlist_pos(n).await?;
    }
}
```

**Conséquences sur l'existant**, à traiter dans le même lot :

- le plugin **cd** déclare `finite: true` sur ses `Play`, et le commentaire du
  cœur qui justifiait le reniflage est remplacé par la raison réelle ;
- les sites de construction de `Play` (radio, cd, tests du cœur) passent par
  un constructeur `SourceAction::play(uri)` ajouté au proto, avec
  `.starting_at(n)` et `.finite()`, pour qu'un champ ajouté plus tard ne
  provoque pas une vague de modifications ;
- rien à changer dans le SDK : `SourceOutcome` transporte l'action telle quelle.

**Attention au déploiement** : un binaire cd antérieur à ce changement
n'émettrait pas `finite`, donc rejouerait le disque en boucle à la fin.
`deploy.sh` installant tous les binaires ensemble, le cas ne se présente que
sur une installation partielle à la main.

## 3. Les unités livrées

| Nom | Genre | Rôle |
|---|---|---|
| `ritornello-plugin-files` | `source` + page d'admin | La Source, la page, le scan, les listes de lecture. |
| `ritornello-media-mount` | binaire **racine** | Second `[[bin]]` du même crate : monte et démonte ce que la conf déclare. |

Les tags ne donnent lieu à **aucun nouveau plugin** : le cœur les lit lui-même
(§7). Le chantier livre donc un seul crate, portant deux binaires.

Le binaire de montage est un second binaire du **même crate**, et non un crate
séparé : il partage ainsi le module de configuration et ses tests avec le
plugin, ce qui garantit que le côté privilégié et le côté qui écrit la conf
lisent exactement la même grammaire.

La Source suit le plan du plugin radio : **deux moitiés indépendantes** dans
deux `tokio::spawn` distincts, une panne de la page ne coupant jamais l'audio,
et une page d'admin construite seulement si `--admin-socket` a été fourni.

## 4. Les racines

Une **racine** est un répertoire nommé où le plugin a le droit de regarder. Un
disque USB, un dossier local et un partage SMB sont la même chose pour tout le
reste du plugin ; le montage n'est qu'un détail du genre `smb`. C'est ce qui
rend le parcours des fichiers locaux quasi gratuit.

`/etc/ritornello/media-roots.toml`, écrit par la page, relu par le binaire de
montage :

```toml
[[root]]
name = "nas"            # sert de composant de chemin ET de nom de fichier
kind = "smb"
host = "192.168.1.20"
share = "musique"
subpath = "Albums"      # optionnel
user = "steven"
domain = ""             # optionnel
writable = false        # défaut ; à vrai, le montage perd `ro` (voir §6)
# le mot de passe ne figure pas ici : voir le fichier d'identifiants

[[root]]
name = "usb"
kind = "local"
path = "/media/usb"
```

Le point de montage d'une racine `smb` **n'est pas configurable** : il vaut
toujours `/mnt/ritornello/<name>`. Un chemin libre serait un chemin à valider.

## 5. Le montage, et où passe la frontière de privilège

Le service tourne en `NoNewPrivileges=true` (voir `deploy/ritornello.service`) :
`sudo` et tout chemin setuid sont **structurellement** hors d'atteinte. Le
plugin ne peut écrire ni dans `/etc/systemd/system` ni dans
`/run/systemd/system`, donc il ne peut pas fabriquer d'unité de montage — et
les unités `.mount` ne se templatisent pas, leur nom devant coder le point de
montage.

Le mécanisme est donc :

1. `deploy.sh` provisionne une unité fixe **`ritornello-media-mount.service`**
   (`Type=oneshot`, exécutée par root) et le binaire
   `/usr/local/lib/ritornello/ritornello-media-mount` ;
2. la page écrit `media-roots.toml` et, par partage, un fichier d'identifiants
   `/etc/ritornello/media-credentials/<name>.cred` en `0600` (`/etc/ritornello`
   est déjà dans `ReadWritePaths`) ;
3. le plugin lance **`systemctl start ritornello-media-mount.service`** en
   processus fils — c'est ainsi que l'onglet Système parle à systemd et à
   logind (`busctl`, `systemctl`), sans aucune dépendance D-Bus en Rust ;
4. une règle polkit autorise l'utilisateur `ritornello` sur `manage-units`,
   **restreinte à cette seule unité** ;
5. le binaire réconcilie : il monte ce qui est déclaré et absent, démonte ce
   qui n'est plus déclaré. Idempotent, donc rejouable sans précaution, y
   compris au démarrage (`WantedBy=multi-user.target`).

### La frontière, dite franchement

Une conf écrite par un processus **non privilégié** est consommée par un
binaire **root**. Autrement dit : qui atteint l'IHM web décide de ce que root
monte. C'est le point du chantier qui mérite le plus de tests, et la validation
vit du **côté privilégié** — celui qui écrit la conf valide aussi, mais sa
validation ne compte pas comme une garantie.

Le binaire de montage n'accepte que :

- `name` conforme à `^[a-z0-9][a-z0-9-]{0,31}$` — il devient un composant de
  chemin et un nom de fichier ;
- `kind = "smb"` (les racines locales ne se montent pas) ;
- `host` et `share` sans virgule, sans espace, sans `..` — **la virgule est
  l'injection à craindre**, les options de `mount.cifs` étant séparées par des
  virgules ;
- un point de montage imposé, `/mnt/ritornello/<name>`, jamais lu depuis la
  conf ;
- une liste d'options **fermée** : `ro`, `soft`, `credentials=<chemin>`,
  `uid`/`gid` du service, `iocharset=utf8`. Aucun passe-plat vers `mount -o`.

`ro` parce que le plugin ne modifie jamais le partage — sauf l'enregistrement
d'une liste, traité en §6. `soft` parce qu'un NAS endormi doit rendre une
erreur d'entrée-sortie plutôt que bloquer indéfiniment un processus ; le
montage étant en lecture seule, `soft` ne présente pas le risque de corruption
qui le déconseille en écriture.

Le mot de passe du NAS vit **en clair** dans un fichier lisible par le service.
C'est le même niveau de confiance que le reste de l'appareil — qui atteint
l'IHM peut déjà tout faire — mais autant l'écrire.

### La règle polkit

`deploy/50-ritornello-power.rules` affirme aujourd'hui en commentaire :
« Nothing else is granted: not `manage-units` ». Cette phrase devient fausse.
La nouvelle autorisation ira donc dans un fichier **séparé**,
`deploy/51-ritornello-media.rules`, avec sa propre justification, et le
commentaire devenu faux sera corrigé plutôt que laissé à mentir.

```javascript
polkit.addRule(function (action, subject) {
  if (subject.user === "ritornello" &&
      action.id === "org.freedesktop.systemd1.manage-units" &&
      action.lookup("unit") === "ritornello-media-mount.service") {
    return polkit.Result.YES;
  }
});
```

### Pas de sonde de capacité

L'onglet Système sonde logind (`CanPowerOff` → `"yes"`) pour griser son bouton
quand l'autorisation manque. systemd n'offre **pas** d'équivalent pour
`manage-units` : il n'existe pas de « CanStartUnit ». Le plugin tente donc, et
**rapporte la sortie d'erreur de `systemctl` telle quelle** dans la page — un
refus polkit y est explicite et actionnable (« installer
`51-ritornello-media.rules` »), là où un message maison la rendrait opaque. Une
ligne de journal au démarrage dit quelle unité sera employée.

## 6. Listes de lecture et m3u

**Deux objets distincts**, et les confondre serait une erreur :

- la **liste utilisateur** : ce qu'on édite dans la page, ce qu'on enregistre
  et recharge, au format m3u ;
- la **liste donnée à mpv** : un m3u *généré*, à chemins absolus locaux, écrit
  dans le répertoire d'état à chaque changement. Découplée, jamais montrée.

**Écriture** : `#EXTM3U`, une ligne `#EXTINF:<durée>,<titre>` par piste, chemins
**relatifs** au répertoire du fichier quand la destination est sous la même
racine — c'est ce qui rend la liste réutilisable par un autre lecteur, et
survivante à un changement de point de montage.

**Lecture**, dans cet ordre, chaque entrée étant résolue indépendamment :

1. relative au répertoire du m3u ;
2. absolue telle quelle, si elle existe ;
3. absolue d'un autre système (`/volume1/music/…`, `Z:\Musique\…`, chemin UNC) :
   on retire le préfixe et on tente sous la racine du m3u.

Ce qui reste irrésolu est **listé comme introuvable dans la page**, jamais
supprimé en silence : un m3u écrit par le NAS porte souvent des chemins qui
n'ont de sens que chez lui, et une liste qui rétrécit sans rien dire est un
défaut qu'on met des mois à attribuer.

**Enregistrer sur le partage** demande une écriture, alors que le montage est
en `ro`. Traité ainsi : la destination « partage » monte ce partage en lecture
seule comme les autres, et l'enregistrement échoue avec un message clair. Pour
écrire sur le NAS, la racine doit être déclarée avec `writable = true`, qui
retire `ro` des options — un choix explicite, par racine, plutôt qu'un partage
ouvert en écriture par défaut.

**Plafond : 2000 pistes** par liste, refus au-delà avec un message. La borne
protège trois choses à la fois : la charge utile JSON servie à la page,
l'écriture du m3u, et la playlist mpv.

**Extensions retenues** : `mp3`, `flac`, `ogg`, `oga`, `opus`, `m4a`, `aac`,
`wav`, `wma`, `aiff`, `ape`, `wv`, `mpc`. La comparaison est insensible à la
casse.

## 7. Les tags : lus par le cœur

**Décidé** : le cœur exploite les tags que mpv lui envoie déjà. Aucun plugin
`metadata` n'est écrit pour ce chantier.

Le banc d'essai (§1) a montré que la propriété `metadata` de mpv porte déjà
`title`, `artist` et `album` d'un fichier local, et que **le cœur l'observe
déjà** : `mpv.rs:50` en extrait `icy-title` et rien d'autre. L'information
arrive jusqu'au cœur, personne ne la ramasse.

Concrètement : une vingtaine de lignes à côté d'`icy_title`, une variante
d'`Event`, et une couche d'arbitrage **plugin `metadata` > tags mpv > ICY**.
Aucun processus supplémentaire, aucune dépendance, aucune relecture du fichier
par le réseau — mpv l'a déjà lu.

Le banc a vérifié que la portée est bien générale : mp3 (ID3), flac, ogg et
opus (Vorbis comments), m4a (atomes iTunes) et wav (RIFF INFO) remontent
**tous** sous les mêmes clés `title` / `artist` / `album` — FFmpeg normalise,
le cœur n'a donc qu'une grammaire à connaître.

Deux précautions que la mesure a fait apparaître, et qui conditionnent
l'option :

- **piocher trois clés nommées, jamais absorber l'objet** : en m4a, `metadata`
  charrie aussi des clés de conteneur (`major_brand`, `handler_name`,
  `vendor_id`…) qui n'ont rien à faire dans un affichage ;
- **la couche ne s'applique que si aucune clé ICY n'est présente.** Certaines
  stations renseignent un `title` valant le nom de la station, à côté d'un
  `icy-title` qui porte le vrai morceau : préférer le premier serait une
  régression pour la radio. La présence d'une clé `icy-*` signe un flux, et
  le chemin ICY garde alors la main.

### L'alternative écartée, et pourquoi

Un plugin `metadata` dédié (`file-tags`, crate séparé, dépendance `lofty`)
avait d'abord été retenu, pour sa souplesse : « si demain une autre source lit
des mp3 ». La mesure a retourné l'argument — le cœur sert **toute** source
jouant un fichier taggé, y compris une future source Bluetooth ou UPnP, sans
qu'aucune n'ait rien à déclarer, là où un plugin ne servirait que ce qui expose
une identité `kind: "file"`. Il aurait par ailleurs rouvert par le réseau un
fichier que mpv venait d'ouvrir.

Ce que le plugin gardait pour lui, et qui pourra le faire revenir un jour :
l'indépendance vis-à-vis de ce que mpv expose (pochette, ReplayGain, tags
multivalués), et un fichier de correction pour rattraper des tags faux, sur le
modèle d'`ouifm-metas.toml`. Aucun de ces champs n'existe dans `Enrichment` ni
`Morceau` aujourd'hui : les ajouter demanderait du travail de protocole quelle
que soit l'option.

Le genre `metadata` n'est pas remis en cause pour autant : `musicbrainz`
interroge une base en ligne à partir d'une TOC, `ouifm-metas` lit un flux
séparé — des choses que mpv ne peut structurellement pas connaître.

### Ce que la décision entraîne

- le badge `origin` vaut **`"tags"`**, et non `"mpv"` qui nommerait un détail
  d'implémentation ;
- `docs/plugins.md` est à compléter : sa section « deux couches se superposent »
  en compte désormais **trois**, et l'ordre de préséance y est à écrire ;
- l'arbitrage existant du cœur (« un plugin l'emporte sur ICY en toutes
  circonstances ») s'étend sans changer de forme : la nouvelle couche
  s'intercale, elle ne déplace rien.

Enfin, le nom de ce qui joue s'affiche **même sans aucune métadonnée** : la
Source déclare `preset_name` (titre `#EXTINF` du m3u, sinon nom du fichier sans
extension), champ apparu avec le chantier « afficheurs ». Les tags ne font
qu'enrichir par-dessus, et leur absence ne laisse jamais un écran muet.

## 8. La Source : cycle de vie

L'identité déclarée est `{"kind": "file", "path": "<chemin absolu local>"}` —
opaque pour le cœur, qui ne fait que la comparer et la relayer.

| Requête | Comportement |
|---|---|
| `activate` / `wake` | Liste vide : `Noop`, statut « AUCUNE LISTE ». Sinon `Play { uri: <m3u généré>, start: <index repris>, finite: true }`. |
| `select(n)` | `n` hors liste : statut éphémère « PISTE VIDE », **aucune déclaration d'identité** (ce qui joue continue). Sinon `Play { start: n-1, finite: true }`. |
| `next` / `prev` | `PlayerNext` / `PlayerPrev`. mpv marche dans sa propre liste. |
| `player_track(n)` | `n < 0` écarté (mpv dit `-1` en fin de liste). Sinon recalage : index, identité, `preset`, `preset_name`. |
| `stop` | Recale l'état interne : plus rien ne joue, `plays_nothing()`. C'est aussi ce que le cœur envoie **en fin de liste** (mpv inactif). |
| `deactivate` | `Stop`, `plays_nothing()`. |
| `eject` | `Noop`. Rien à éjecter. |

Champs déclarés à chaque trame utile : `preset` (index + 1), `preset_count`
(`min(len, 99)`), `preset_name`, `status`.

**Le piège à ne pas manquer** : `status` a la convention **inverse** de
`preset`. Absent veut dire « pas de statut », et non « garde le précédent » —
une Source doit le redéclarer à chaque trame. Uniformiser les deux produirait
un affichage qui s'efface tout seul. (Voir le commentaire de
`SourceMessage::status`.)

**Plafond des 99** : `preset` est un `u8` et les présélections vont de 1 à 99.
Au-delà de 99 pistes, les suivantes restent atteignables par `next`/`prev` et
par la liste de la page, mais aucun chiffre de la télécommande ne les désigne.
Ce n'est pas contourné.

## 9. La page web

Module ESM Vue servi sous `/plugins/files/`, avec les contraintes connues :
`vue` et `@ritornello/ui` fournis par l'import map (jamais empaquetés), `base`
**requis** sans valeur par défaut, gabarits précompilés (le Vue servi est
`runtime-only`), pas de `vue-router`, et des **noms de fichiers à plat** —
`/plugins/<nom>/<fichier>`, un seul segment.

Trois volets :

1. **Racines** — ajouter un dossier local ; déclarer un partage (hôte, partage,
   sous-chemin, utilisateur, mot de passe, domaine, écriture autorisée) ; état
   du montage ; bouton « monter maintenant » rapportant la sortie de
   `systemctl` en cas d'échec.
2. **Parcourir** — arbre **paresseux** (un niveau par requête), champ de
   recherche, « Ajouter ce dossier (récursif) », « Ajouter ce fichier »,
   progression du scan.
3. **Liste en cours** — pistes ordonnées, réordonner, retirer, vider,
   enregistrer (nom + destination : interne ou une racine), charger.

Le protocole admin ne transporte que du **texte** et ne pousse rien : le scan
est donc une **tâche asynchrone** côté plugin, dont la page interroge l'avancement
tant qu'elle tourne. Un scan porte un identifiant ; en lancer un second alors
qu'un premier tourne le remplace.

La marche récursive filtre par extension et **garde contre les boucles de liens
symboliques** (ensemble des couples périphérique/inode visités).

## 10. Fichiers, variables, déploiement

| Chemin | Rôle | Variable |
|---|---|---|
| `/etc/ritornello/media-roots.toml` | Racines déclarées | `RITORNELLO_FILES_ROOTS` |
| `/etc/ritornello/media-credentials/<name>.cred` | Identifiants, `0600` | `RITORNELLO_FILES_CREDENTIALS` |
| `/var/lib/ritornello/plugin-files.json` | Liste courante, index, dernier dossier parcouru | `RITORNELLO_FILES_STATE` |
| `/var/lib/ritornello/playlists/` | Listes enregistrées en interne | `RITORNELLO_FILES_PLAYLISTS` |
| `/var/lib/ritornello/plugin-files.m3u` | Liste générée pour mpv | — |
| `/mnt/ritornello/<name>` | Points de montage | — |

`deploy.sh` gagne : le binaire de montage, l'unité, la règle polkit, la
création de `/mnt/ritornello`, et les deux entrées de `plugins.example.toml`
(`files` en `source` — une seule entrée, les tags ne passant par aucun plugin).
Comme pour les autres plugins, un `plugins.toml` existant **n'est jamais
écrasé** : une installation déjà en service ne verra pas la nouvelle source
tant que ses deux lignes n'auront pas été ajoutées à la main. À dire dans
`docs/installation.md`.

## 11. Tests

Dans le style du projet : fonctions pures éprouvées contre des données réelles,
chaque test encodant une régression plausible.

- **Validation du montage** (le côté privilégié) : noms refusés, hôte ou
  partage porteur d'une virgule, `..` dans un sous-chemin, `kind` inattendu,
  point de montage jamais lu depuis la conf. C'est le lot le plus important.
- **m3u** : écriture/relecture, `#EXTINF` conservés, chemins relatifs, et un
  fichier réel écrit par un NAS (chemins Windows et absolus étrangers) dont on
  vérifie que les entrées irrésolues sont **rapportées** et non jetées.
- **Marche récursive** : filtrage d'extensions, casse, boucle de liens
  symboliques, plafond de 2000.
- **Modèle de liste** : `select` hors bornes, `next`/`prev` aux extrémités,
  `player_track(-1)` écarté, recalage après fin de liste, `preset_count`
  plafonné à 99.
- **Statut** : une trame qui ne redéclare pas `status` l'efface — le test qui
  épingle la convention inverse.
- **Protocole** : `Play` sans `start` ni `finite` reste sérialisé à l'identique
  (compatibilité de la radio), et `finite: true` fait bien le tour.
- **Cœur** : fin de liste (mpv inactif) sur un `Play { finite: true }` déclenche
  `SourceReq::Stop` et **non** la relance — le test qui aurait attrapé le
  reniflage `cdda://`.
- **Couche tags** : les trois clés extraites d'une charge `metadata` réelle par
  format (m4a compris, dont les clés de conteneur doivent être ignorées) ; une
  charge portant `icy-title` **et** un `title` parasite laisse la main à ICY —
  le test qui épingle la régression radio ; un plugin `metadata` l'emporte
  toujours sur les tags.
- **Page** : un parcours Playwright — déclarer une racine locale, ajouter un
  dossier, enregistrer, recharger.

## 12. Hors périmètre

Lecture aléatoire et répétition ; base de données de bibliothèque et vues par
artiste/album ; pochettes ; transcodage ; écriture de tags ; plusieurs listes
actives à la fois ; découverte automatique des partages du réseau.

## 13. À vérifier sur la machine cible

- **Propagation du montage.** L'unité est durcie (`ProtectSystem=strict`,
  `ProtectHome=true`), donc elle tourne dans son propre espace de noms de
  montage. systemd le monte en `rslave`, ce qui *doit* faire apparaître les
  montages ultérieurs de l'hôte dans le service — cela se vérifie sur le Pi, et
  non sur parole. Si la propagation ne se fait pas, le recours est un
  `BindPaths=/mnt/ritornello` dans l'unité.
- **`mount.cifs`** est fourni par le paquet `cifs-utils`, à ajouter à la liste
  d'installation aux côtés de `mpv`, `cd-discid` et `eject`.
- **Dialecte SMB** : aucun `vers=` n'est imposé, la négociation du noyau étant
  préférable à une version figée. À confirmer sur le NAS visé.
