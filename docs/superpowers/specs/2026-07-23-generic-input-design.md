# ritornello — Plugin `generic-input` configurable

Transformer le plugin Input, aujourd'hui figé sur une télécommande MCE et une
table de touches codée en dur, en un lecteur evdev générique : plusieurs
périphériques simultanés avec des bindings propres à chacun, des presets
livrés chargeables d'un clic, et un mode apprentissage pour attacher une touche
à une commande depuis le navigateur.

Date : 2026-07-23 — Statut : validé

## Contexte

`ritornello-plugin-mce` n'a de « mce » que le nom : c'est déjà un lecteur evdev
générique (sélection d'un périphérique par sous-chaîne de nom via
`RITORNELLO_MCE_INPUT_NAME`, ou chemin exact via `RITORNELLO_MCE_DEVICE`), suivi
d'une traduction des codes par une table **codée en dur** dans `keymap.rs`.
Trois limites en découlent : il ne lit qu'**un seul** périphérique à la fois,
le mapping n'est modifiable qu'à la recompilation, et rien n'est configurable
depuis l'interface web.

Le cœur sert désormais les pages d'admin des plugins sous une origine unique
(spec `2026-07-23-serveur-web-unique-design.md`) et l'i18n repose sur des packs
décentralisés par composant (spec `2026-07-23-i18n-design.md`) : les deux
mécanismes dont ce chantier a besoin existent déjà et sont éprouvés par le
plugin radio, qui sert sa page de gestion des stations exactement ainsi.

## Décisions de cadrage

| Sujet | Décision |
|---|---|
| Nom | `ritornello-plugin-generic-input`, déclaré `name = "generic-input"`, `kind = "input"`, `admin = true` |
| Périphériques écoutés | **Tous** les périphériques evdev lisibles sont ouverts ; la table de bindings est consultée **au moment de l'événement** |
| Clé d'un binding | Le **nom** du périphérique (stable au redémarrage), pas son chemin ; tous les nœuds portant ce nom sont écoutés |
| Persistance | `/etc/ritornello/input-bindings.toml` (surchargeable par `RITORNELLO_INPUT_BINDINGS`) |
| Presets | Fichiers livrés dans `/etc/ritornello/input-presets/*.toml` (depuis `deploy/input-presets/`) |
| Apprentissage | Sans extension du protocole d'admin : opérations dans `SetData`, état lu par sondage de `GetData` |
| Rafraîchissement | Bouton dédié : ré-énumère **et ouvre** les périphériques nouvellement détectés, sans recharger la page |
| Structure | Plugin bicéphale (Input + Admin) en **deux tâches `tokio::spawn` indépendantes** (leçon acquise sur le plugin radio) |
| Hors périmètre | Branchement à chaud automatique (udev), combinaisons de touches, appui long, axes/manettes |

## Renommage

`crates/ritornello-plugin-mce/` → `crates/ritornello-plugin-generic-input/`
(crate et binaire renommés). Surface à mettre à jour : le `Cargo.toml` racine
(membre du workspace), `deploy/plugins.example.toml`, `deploy/deploy.sh`
(le binaire copié), et le `README.md`.

Disparaissent : `keymap.rs` (sa table devient le preset `mce.toml`) et les
variables `RITORNELLO_MCE_INPUT_NAME` / `RITORNELLO_MCE_DEVICE`, remplacées par
le fichier de bindings.

## Écouter tous les périphériques, mapper à la volée

Le plugin n'élit plus un périphérique au démarrage. Il **ouvre tous les
périphériques evdev qu'il peut lire** et, à chaque événement de touche, cherche
un binding correspondant au couple (nom du périphérique, code de la touche).

Conséquences voulues :
- ajouter un binding pour un périphérique déjà branché ne demande **aucun**
  redémarrage ni relance de tâche — la table partagée est simplement relue ;
- le mode apprentissage fonctionne sur **n'importe quel** périphérique, y
  compris un qui n'a encore aucun binding ;
- la liste déroulante de l'IHM montre le matériel **réellement détecté**, pas
  seulement celui qui est configuré.

L'ouverture n'est **pas** exclusive (aucun `EVIOCGRAB`) : le système continue de
recevoir normalement les événements du clavier. Seules les touches liées à une
commande produisent quelque chose côté ritornello ; toutes les autres sont
ignorées silencieusement.

Un périphérique illisible (droits, disparu entre l'énumération et l'ouverture)
est logué en `warn` et ignoré — jamais fatal. La fin de lecture d'un
périphérique (débranchement) termine sa tâche, la loggue, et laisse les autres
intactes.

## Fichier de bindings

`/etc/ritornello/input-bindings.toml` :

```toml
[[device]]
name = "eHome Infrared Transceiver"

[[device.binding]]
code = 115
cmd = "VolumeUp"

[[device.binding]]
code = 2
cmd = "Select"
arg = 1
```

Le couple `cmd` / `arg` est **exactement la représentation sérialisée du type
`Command`** de `ritornello-proto` (`#[serde(tag = "cmd", content = "arg")]`) :
un binding porte donc un `Command` aplati, sans liste de commandes dupliquée, et
le même objet transite tel quel en JSON vers l'IHM.

Types (indicatifs), dans un module `bindings.rs` du plugin :

```rust
#[derive(Serialize, Deserialize)]
pub struct Binding {
    pub code: u16,
    #[serde(flatten)]
    pub command: ritornello_proto::Command,
}

#[derive(Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    #[serde(default, rename = "binding")]
    pub bindings: Vec<Binding>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Bindings {
    #[serde(default, rename = "device")]
    pub devices: Vec<Device>,
}
```

**Risque connu et repli.** Le motif `#[serde(flatten)]` sur un enum à tag
adjacent est déjà éprouvé **en JSON** dans ce projet (`SourceRequest` du
protocole), mais pas **en TOML** — la combinaison `flatten` + enum à tag adjacent
est parfois capricieuse selon le format. Le premier test à écrire est donc
l'aller-retour TOML d'un `Binding`. S'il échoue, repli sans discussion :
remplacer le champ aplati par `cmd: String` + `arg: Option<u8>` dans la
structure persistée, avec deux conversions `From`/`TryFrom` vers `Command` — le
fichier et le JSON échangé gardent exactement la même forme, seule la mécanique
interne change.

Chargement/sauvegarde sur le modèle de `Stations` du plugin radio :
écriture atomique (fichier temporaire puis `rename`), fichier absent traité
comme une configuration vide (démarrage sans binding, avertissement loggé une
fois invitant à passer par la page d'admin), TOML invalide traité de même.

Validation : un même `code` ne peut être lié deux fois sur un même
périphérique ; un `Select` doit avoir un `arg` entre 1 et 9. Le message d'erreur
remonte en `422` sur la page d'admin, traduit via le catalogue (mécanisme
identique à `ValidationError` du plugin radio).

## Presets

Fichiers livrés dans `deploy/input-presets/`, installés vers
`/etc/ritornello/input-presets/` par `deploy.sh` (racine surchargeable par
`RITORNELLO_INPUT_PRESETS`). Un preset est une simple liste de bindings, **sans
nom de périphérique** :

```toml
[[binding]]
code = 115
cmd = "VolumeUp"
```

Deux presets au départ :
- **`mce.toml`** — la table aujourd'hui codée en dur dans `keymap.rs`, reprise
  à l'identique (chiffres 2-10 et 513-521 → `Select(1..9)`, 115/114/113 →
  volume et muet, 402/403 → présélection suivante/précédente, 164 →
  lecture/pause, 163/165 → piste suivante/précédente, 166 → stop, 161 →
  éjecter, 226 → changement de source, 116 et 356 → veille) ;
- **`keyboard.toml`** — un clavier ordinaire : chiffres 1-9 (codes 2-10) vers
  les présélections, flèches haut/bas (103/108) vers le volume, flèches
  droite/gauche (106/105) vers présélection suivante/précédente, espace (57)
  vers lecture/pause, `m` (50) vers muet, `s` (31) vers changement de source,
  `p` (25) vers veille.

La liste des presets disponibles est découverte en lisant le répertoire (nom de
fichier sans l'extension). Charger un preset **remplace** l'intégralité des
bindings du périphérique sélectionné.

## Protocole d'admin (aucune extension nécessaire)

Le plugin implémente `AdminPlugin` (`GetPage` / `GetData` / `SetData`), servi
par le cœur sur un second socket, comme la radio.

`GetData` renvoie l'état complet, en lecture pure :

```json
{
  "devices": ["eHome Infrared Transceiver", "USB Keyboard"],
  "bindings": { "devices": [ /* structure Bindings ci-dessus */ ] },
  "presets": ["keyboard", "mce"],
  "learning": { "device": "USB Keyboard", "captured": 115 }
}
```
`devices` liste les périphériques **actuellement ouverts** ; `learning` vaut
`null` hors mode apprentissage, et son `captured` reste `null` tant qu'aucune
touche n'a été pressée.

`SetData` porte une opération discriminée par un champ `op` :

| `op` | Charge utile | Effet |
|---|---|---|
| `save` | `{ "bindings": {...} }` | Valide, persiste, remplace la table partagée |
| `learn` | `{ "device": "..." }` | Entre en apprentissage pour ce périphérique |
| `cancel_learn` | — | Sort de l'apprentissage sans rien retenir |
| `load_preset` | `{ "device": "...", "preset": "mce" }` | Remplace en mémoire les bindings de ce périphérique par ceux du preset (l'utilisateur enregistre ensuite) |
| `rescan` | — | Ré-énumère et **ouvre** les périphériques nouvellement détectés |

## Mode apprentissage

Séquence complète, sans push serveur → client :

1. L'utilisateur clique « Apprendre » sur la ligne d'une commande. L'IHM envoie
   `SetData{op:"learn", device}`.
2. Le plugin note l'état d'apprentissage. **Tant qu'il dure, les événements de
   ce périphérique ne produisent plus de commande** — sinon apprendre
   « Volume + » déclencherait un volume +. Les autres périphériques continuent
   de fonctionner normalement.
3. Le premier événement de touche (appui, `value == 1`) est retenu comme
   `captured` et l'apprentissage se termine.
4. L'IHM interroge `GetData` toutes les ~300 ms jusqu'à obtenir `captured`,
   avec un abandon automatique au bout de ~10 s et un bouton « Annuler »
   (`op:"cancel_learn"`).
5. Le code obtenu s'inscrit dans la ligne de la commande, **côté navigateur
   seulement** ; rien n'est persisté tant que l'utilisateur n'a pas cliqué
   « Enregistrer » (`op:"save"`), ce qui lui laisse le droit de se raviser.

L'apprentissage est **exclusif** : une nouvelle demande `learn` remplace la
précédente. Il est également abandonné si le périphérique visé disparaît.

## L'interface

Page servie par le cœur à `http://<hôte>:8080/plugins/generic-input/`, dans le
style dépouillé de la page des stations (HTML simple, JS en ligne, aucune
ressource externe) :

- une **liste déroulante** des périphériques détectés, avec à côté un bouton
  **« Rafraîchir »** qui envoie `op:"rescan"` puis refait un `GetData` — la
  liste se met à jour **sans recharger la page**, et le périphérique fraîchement
  branché est immédiatement écouté, donc apprenable ;
- un **tableau des 21 actions** (présélections 1 à 9, puis présélection
  suivante/précédente, volume +/−, muet, lecture/pause, stop, piste
  suivante/précédente, éjecter, changement de source, veille), chaque ligne
  affichant le code assigné pour le périphérique sélectionné, un bouton
  **« Apprendre »** et un bouton **« Effacer »** ;
- un **sélecteur de preset** et son bouton « Charger », qui remplace tous les
  bindings du périphérique courant ;
- un bouton **« Enregistrer »**, et une zone de message affichant le succès ou
  l'erreur de validation renvoyée en `422`.

Tous les libellés passent par des jetons `{{clé}}` remplacés par `page()` selon
la langue courante, avec un anglais embarqué dans le crate et un pack
`deploy/locales/generic-input/fr.toml` — exactement le mécanisme i18n
décentralisé déjà en place.

## Structure du plugin

```
crates/ritornello-plugin-generic-input/src/
  main.rs      — deux tâches spawn indépendantes (Input + Admin), état partagé
  bindings.rs  — types Bindings/Device/Binding, chargement, sauvegarde, validation
  presets.rs   — découverte et chargement des presets
  devices.rs   — énumération, ouverture, boucle de lecture evdev, résolution
  admin.rs     — implémentation d'AdminPlugin (page, get_data, set_data)
  index.html   — gabarit à jetons
  locales/en.toml
```

État partagé entre les deux moitiés : les bindings et l'état d'apprentissage,
dans des `Arc<RwLock<…>>` (`std::sync::RwLock` pour ce que `page()`, qui est
synchrone, doit lire — comme dans le plugin radio ; jamais de garde tenue à
travers un `.await`).

Les deux moitiés tournent en **tâches `tokio::spawn` séparées**, jointes sur
leurs `JoinHandle` : une panique ou une erreur d'un côté ne tue pas l'autre.

## Erreurs et dégradation

Tout est best-effort, dans l'esprit du reste du projet : fichier de bindings ou
de presets absent ou illisible → configuration vide et `warn`, jamais de
panique ; périphérique illisible → ignoré et logué ; périphérique débranché →
sa tâche se termine, les autres continuent ; opération `SetData` inconnue →
erreur de validation renvoyée à l'IHM.

## Tests

La logique est séparée des I/O evdev pour être testable sans matériel :

- `bindings.rs` : aller-retour TOML, fichier absent → vide, validation (code en
  double, `Select` hors bornes 1-9), et **résolution** d'un couple (nom,
  code) → `Command`, y compris l'absence de binding.
- `presets.rs` : découverte des noms depuis un répertoire temporaire, chargement
  d'un preset, preset inconnu → erreur ; test que les presets **livrés**
  (`mce.toml`, `keyboard.toml`) se chargent et sont non vides.
- Machine à états de l'apprentissage (fonction pure) : `learn` puis capture d'un
  code, `cancel_learn`, remplacement d'un apprentissage par un autre, et
  **suppression de l'émission de commande** pendant l'apprentissage du
  périphérique concerné alors qu'un autre périphérique reste actif.
- `admin.rs` : `page()` substitue tous les jetons ; `get_data` reflète l'état ;
  chaque `op` de `set_data` produit l'effet attendu, `save` invalide → `Err`
  sans persister.
- Le pack anglais embarqué se charge et n'est pas vide (invariant déjà appliqué
  aux autres composants), et parité des clés entre `en.toml` et le pack
  français.

## Hors périmètre

- Détection **automatique** du branchement à chaud (surveillance udev) : le
  bouton « Rafraîchir » couvre le besoin sans démon supplémentaire.
- Combinaisons de touches, appui long, répétition.
- Axes, manettes, souris : seuls les événements de touche (`EV_KEY`, appui) sont
  traités.
- Réattribution des commandes du cœur ou nouvelles commandes : le jeu des 13
  commandes du protocole est inchangé.
