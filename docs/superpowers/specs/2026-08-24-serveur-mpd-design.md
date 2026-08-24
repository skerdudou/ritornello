# Serveur MPD : l'appareil vu comme un lecteur MPD

Date : 2026-08-24

## Le but

Exposer l'appareil sur le réseau local comme un serveur MPD, pour qu'un client
du téléphone — M.A.L.P., sur le Play Store comme sur F-Droid, MPDroid étant
mort — serve de télécommande sans qu'on écrive d'application Android.

L'ambition est arrêtée : **façade de télécommande, plus les listes
enregistrées**. Le client voit ce qui joue, agit sur les touches, et voit
chaque source de l'appareil comme une liste de lecture enregistrée dont les
présélections sont les entrées. Pas de base de données parcourable, pas
d'ajout à la file, pas d'écriture de listes.

## Le nœud

Un greffon MPD est un **traducteur entre deux protocoles asymétriques**.

MPD est requête/réponse à valeurs absolues : `setvol 40`, `pause 1`, et
surtout « dans quel état es-tu ? ». Le protocole d'entrée de Ritornello est un
tir sans retour à touches relatives : `VolumeUp`, `PlayPause`, rien qui
revienne. Un greffon `input` seul ne peut donc répondre à aucune question, et
un greffon `display` seul ne peut agir sur rien.

Le greffon referme la boucle en portant **les deux genres dans le même
processus** : sa moitié `display` tient le dernier état connu en mémoire, sa
moitié `input` émet les touches. C'est exactement ce que le chantier
« rendez-vous des greffons » a rendu possible — un binaire chaîne
`.input()` et `.display()` sur le `Runtime`, et le cœur accepte désormais
**plusieurs** afficheurs. Sans cette correction, le greffon MPD aurait évincé
la console.

Il reste que l'état poussé aux afficheurs ne suffit pas à répondre à MPD. Trois
manques, plus deux pour les listes enregistrées : ce sont les cinq additions de
la section suivante, et c'est le vrai coût du chantier.

## Architecture

Un crate, `ritornello-plugin-mpd`, un binaire du même nom. Un processus, quatre
choses :

```
                 ┌─────────────────────────────────────────┐
   coeur ────────┤ moitie `display`  ──►  Etat partage      │
   (etat)        │                        (RwLock + Notify) │
                 │                             │  ▲         │
   coeur ◄───────┤ moitie `input`   ◄──────────┘  │         │
   (commandes)   │                                │         │
                 │ TcpListener 0.0.0.0:6600       │         │
                 │   └─ une tache par client ─────┘         │
                 │                                          │
   navigateur ───┤ moitie `admin` : adresse et port         │
                 └─────────────────────────────────────────┘
```

- **La moitié `display`** reçoit chaque trame, met à jour l'état partagé, et
  réveille les dormeurs par `Notify` en leur nommant les sous-systèmes qui ont
  changé (comparaison avec la trame précédente).
- **La moitié `input`** publie les commandes sur un canal `mpsc` que les
  sessions clientes alimentent. `next_command` ne fait que dépiler.
- **Chaque session cliente** est une tâche : elle lit des lignes, répond, et
  n'a jamais besoin d'attendre le cœur — toute question se répond depuis
  l'état partagé, toute action est un envoi sur le canal. **Aucune session ne
  peut donc bloquer une autre**, ni retenir le cœur.
- **La moitié `admin`** règle l'adresse d'écoute et le port. Un changement
  exige un redémarrage du greffon, et la page le dit.

L'état partagé est sous `RwLock` et non `Mutex` : les sessions ne font
presque que lire, et un client qui compose une longue réponse
(`listplaylistinfo` sur 51 stations) ne doit pas retarder les autres.

## Ce que le greffon ne fait pas

Franchement, et par `ACK [5@…] {…} unsupported` :

`lsinfo`, `listall`, `listallinfo`, `search`, `find`, `list`, `count`,
`update`, `add`, `addid`, `delete`, `deleteid`, `move`, `swap`, `shuffle`,
`clear`, `save`, `rm`, `rename`, `playlistadd`, `playlistdelete`,
`repeat`, `random`, `single`, `consume`, `crossfade`, `replay_gain_mode`,
`enableoutput`, `disableoutput`, `subscribe`, `sendmessage`, `kill`.

Deux d'entre elles méritent leur raison écrite :

- **`update`** n'a pas de sens : il n'y a pas de base de données à indexer.
- **`kill`** est refusée et non ignorée : arrêter l'appareil depuis le réseau
  sans authentification serait une capacité qu'aucune télécommande n'a.

`repeat`, `random`, `single` et `consume` sont **rapportées à 0** par `status`
— les clients les lisent toujours — mais les écrire est refusé. C'est le seul
endroit où le greffon rapporte une valeur qu'il ne sait pas changer, et c'est
délibéré : omettre les champs fait mal se comporter les clients, alors que les
voir à zéro et grisés est exact.

## Réseau

`0.0.0.0:6600`, **sans mot de passe**, réglable par la page d'admin. C'est la
posture de tout serveur MPD domestique, et la même surface que le serveur web
de l'appareil expose déjà. Quiconque est sur le réseau local peut changer de
station et couper le son — ce que fait déjà n'importe quelle télécommande dans
la pièce.

La commande `password` est donc acceptée et rend `OK` sans rien vérifier : un
client configuré avec un mot de passe ne doit pas être rejeté pour autant.

## Le protocole MPD, ce qu'il faut en tenir

À la connexion, le serveur écrit `OK MPD 0.23.5\n` sans qu'on lui demande rien.
Ensuite une commande par ligne, arguments séparés par des espaces, un argument
qui en contient étant entre guillemets doubles où `"` et `\` s'échappent par une
contre-oblique. Une réponse est une suite de lignes `clé: valeur` terminée par
`OK\n`, ou une erreur unique `ACK [<code>@<indice>] {<commande>} <message>`, où
`<indice>` est le rang dans une liste de commandes (0 hors liste).

Trois codes suffisent ici :

| Code | Nom MPD | Quand |
|---|---|---|
| 2 | `ACK_ERROR_ARG` | argument absent, non numérique, hors bornes |
| 5 | `ACK_ERROR_UNKNOWN` | commande inconnue **ou** non gérée |
| 50 | `ACK_ERROR_NO_EXIST` | liste enregistrée nommée qui n'existe pas |

**Une erreur interrompt tout** : dans une liste de commandes, la première qui
échoue produit son `ACK` et les suivantes ne sont pas exécutées.

`command_list_begin` … `command_list_end` accumule, exécute dans l'ordre,
concatène les réponses et clôt par **un seul** `OK`.
`command_list_ok_begin` fait de même en insérant `list_OK\n` après chacune.
M.A.L.P. s'en sert beaucoup : ce n'est pas une commodité, c'est un passage
obligé.

`idle [<sous-système>…]` **ne répond pas** : la connexion reste muette jusqu'à
ce qu'un des sous-systèmes demandés change, puis rend `changed: player\nOK\n`.
Sans argument, tous comptent. `noidle` sur la même connexion annule l'attente et
rend `OK`. C'est par là que les clients apprennent tout ; un serveur sans `idle`
oblige à sonder, et M.A.L.P. ne sonde pas.

| Sous-système | Émis quand |
|---|---|
| `player` | lecture, pause, arrêt, changement de présélection, position |
| `mixer` | volume ou sourdine |
| `playlist` | la file d'attente change (donc : changement de source) |
| `stored_playlist` | le catalogue des sources ou de leurs présélections change |

Un client qui se connecte enchaîne typiquement `commands`, `tagtypes`,
`urlhandlers`, `decoders`, `stats`, `outputs`, puis `status`/`currentsong` et un
`idle`. Aucune n'est facultative en pratique : une qui rend `ACK` peut faire
renoncer le client avant qu'il n'affiche un écran.

**`commands` est la clé de l'honnêteté du greffon.** Un client correct y lit ce
qui existe et grise le reste de lui-même. C'est la différence entre « des
onglets vides » et « des onglets qui plantent ».

## Les cinq additions

Le greffon ne peut pas exister seul : cinq choses lui manquent ailleurs. Elles
sont décrites avec le code exact qu'elles touchent, parce que chacune passe par
un chemin déjà commenté et qu'il ne faut pas défaire le raisonnement qui s'y
trouve.

**Où vivent les types neufs**, pour qu'aucune tâche n'ait à le deviner : chacun
va dans le module de `ritornello-proto` qui porte déjà son protocole, et
`lib.rs` les réexporte comme tous les autres.

| Type | Module | Pourquoi là |
|---|---|---|
| `Playback` | `metadata.rs` | à côté de `PlayerState`, dont il est un champ |
| `Preset` | `source.rs` | c'est un fait sur une source, et `SourceMessage` le porte |
| `Catalogue`, `SourceCatalogue` | `display.rs` (neuf) | la charge utile d'un message d'affichage |
| `DisplayFrame` | `display.rs` (neuf) | l'enveloppe du protocole `display` |

`display.rs` est un module neuf plutôt qu'un ajout à `metadata.rs` : ce dernier
porte les métadonnées et l'état du lecteur, et l'enveloppe du transport n'est ni
l'un ni l'autre. `Catalogue` y importe `Preset` depuis `source.rs` — le même type
des deux côtés, jamais deux définitions jumelles.

### 1. `Command::SetVolume(u8)` — le volume absolu

`setvol 40` est absolu. Empiler huit `VolumeUp` serait faux (le pas est un
réglage, pas une constante) et visible : chaque pas écrit une incrustation.

`core.rs:653` devient deux fonctions, la relative appelant l'absolue :

```rust
/// Volume absolu, la seule voie pour un réglage qui ne vient d'aucune touche :
/// le `setvol` de MPD. Mêmes effets de bord que le pas relatif — mpv, disque,
/// incrustation — parce qu'un volume changé depuis le réseau doit s'annoncer à
/// l'écran comme celui changé depuis la télécommande.
async fn set_volume(&mut self, v: u8) -> Result<()> {
    self.volume = v.min(100);
    self.player.set_volume(self.volume).await?;
    self.persist();
    self.show_overlay().await;
    Ok(())
}

async fn step_volume(&mut self, up: bool) -> Result<()> {
    let v = self.volume as i16 + if up { 5 } else { -5 };
    self.set_volume(v.clamp(0, 100) as u8).await
}
```

Bras de commande : `Command::SetVolume(v) => self.set_volume(v).await?`. Aucun
`volume_deadline` à réarmer — ce n'est pas une touche, rien ne se maintient.

### 2. `Command::SelectSource(String)` — la source par son nom

`load "radio"` désigne une source par son nom ; `SourceCycle` ne sait
qu'avancer d'un cran.

Le corps de `SourceCycle` (`core.rs:905-957`) est **extrait tel quel** dans
`async fn basculer_vers(&mut self, cible: String) -> Result<()>` : arrêt du
lecteur, `Deactivate` en best-effort, oubli de l'identité, du compte de
présélections, du statut et de l'éjection, `persist()` **avant** `Activate`.
Chaque commentaire de ce bloc décrit une leçon payée et suit le code sans
retouche. Les deux commandes ne diffèrent plus que par le calcul de la cible :

```rust
Command::SourceCycle => {
    let idx = self.source_order.iter().position(|n| n == &self.active_source).unwrap_or(0);
    let suivant = (idx + 1) % self.source_order.len().max(1);
    if let Some(cible) = self.source_order.get(suivant).cloned() {
        self.basculer_vers(cible).await?;
    }
}
Command::SelectSource(nom) => {
    // Inconnue : ignorée en silence, comme une touche non liée. Le greffon MPD
    // a déjà répondu `ACK 50` de son côté — il ne propose que des noms reçus du
    // catalogue, donc arriver ici veut dire que la source a disparu entre-temps.
    if !self.source_order.iter().any(|n| n == &nom) {
        tracing::debug!("unknown source {nom} ignored");
        return Ok(());
    }
    // Déjà active : ne rien faire. Un `load` redondant ne doit pas couper ce
    // qui joue, et c'est exactement ce qu'un client envoie en rouvrant son écran.
    if nom != self.active_source {
        self.basculer_vers(nom).await?;
    }
}
```

### 3. `PlayerState.playback` — jouer, en pause, arrêté

Le champ le plus lu de `status`, et **personne ne le sait aujourd'hui** : le
cœur envoie `cycle pause` à mpv (`player/mpv.rs:364`) sans rien retenir.

```rust
/// Ce que fait le lecteur, en un mot. `Stopped` par défaut : ne rien savoir,
/// c'est ne rien jouer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Playback {
    #[default]
    Stopped,
    Playing,
    Paused,
}
```

Champ additif dans `PlayerState`, à l'idiome déjà employé pour
`InputMessage.held` et `PluginStatus.stalled` :

```rust
#[serde(default, skip_serializing_if = "Playback::est_arrete")]
pub playback: Playback,
```

Côté cœur, **un seul champ neuf** (`paused: bool`) et **un seul endroit** où le
remettre à faux : là où `lecture` passe à vrai, donc à chaque `Play` appliqué.
C'est la doctrine que `etat_lecteur` défend déjà en toutes lettres pour
`position_s` — « un point unique ne peut pas être oublié ; cinq appels
sprinkled le seraient au sixième chemin ajouté ». Les cinq chemins qui posent
`lecture = false` n'ont donc rien à toucher :

```rust
playback: if !self.lecture || self.standby {
    Playback::Stopped
} else if self.paused {
    Playback::Paused
} else {
    Playback::Playing
},
```

Le bras `PlayPause` (`core.rs:821`) bascule `self.paused = !self.paused` dans sa
branche « quelque chose joue » et rien dans l'autre — celle qui redemande un
`Play` remet `paused` à faux par le chemin normal.

**Aucune commande `SetPause(bool)` n'est ajoutée**, et c'est un choix. Le
greffon traduit `pause 0`/`pause 1` en `PlayPause` **seulement si l'état diffère
de la cible**, et ferme la course qui reste en tenant un état **optimiste** : il
acte la bascule dès qu'il l'émet, la trame suivante fait autorité. C'est ce que
fait n'importe quelle télécommande, et cela évite d'alourdir `Command` d'une
variante que seule une source réseau émettrait.

Bénéfice hors MPD : le bouton `PlayPause` de la SPA
(`web/app/src/views/remoteCommands.ts:41`) est aujourd'hui une icône fixe,
faute de savoir dans quel sens il va.

### 4. `SourceReq::ListPresets` — les présélections nommées

Le piège d'abord : **rien dans le tuyau des sources ne porte de liste.**
`SourceReq` se résout en exactement un `SourceAction` sur trois couches, et
`client.rs:70` exige `(Some(id), Some(action))` pour dénouer son `oneshot` — une
réponse sans `action` n'y arrive jamais et coûte les 5 s du délai avant
`"source plugin: request timeout"`.

Rien n'a besoin d'être élargi pour autant, parce que `preset_count` emprunte
déjà **une autre voie** : le prédicat de trame intéressante (`client.rs:75-80`)
la relaie en `SourceUpdate`, **hors corrélation**. Les présélections prennent le
même chemin. La réponse à `ListPresets` est un `Noop` — la corrélation est
satisfaite — et la liste voyage à côté :

```rust
// proto : SourceMessage
/// Les présélections nommées de cette source, quand elle sait les énumérer.
/// Hors corrélation, comme `preset_count` : c'est un fait sur la source, pas
/// une réponse à une question.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub presets: Option<Vec<Preset>>,
```

Trois conséquences heureuses :

- **Aucun type élargi** : ni `pending`, ni `Source::request`, ni
  `demande_active`.
- **Coût de démarrage nul.** Le cœur n'attend pas la réponse, puisqu'elle ne le
  renseigne pas ; il lance les demandes dans des tâches détachées et les listes
  arrivent par le canal de mises à jour. Aucune fenêtre de 5 s sur le chemin de
  démarrage — c'est exactement ce que le chantier précédent a passé son temps à
  supprimer.
  ```rust
  // Après le câblage : demander son catalogue à chaque source, sans attendre.
  // La réponse corrélée (`Noop`) n'apprend rien ; les présélections arrivent
  // par `source_update_rx`, comme `preset_count`.
  for (nom, client) in &source_clients {
      let (c, n) = (client.clone(), nom.clone());
      tokio::spawn(async move {
          if let Err(e) = c.request(SourceReq::ListPresets).await {
              tracing::debug!("list_presets for {n}: {e}");
          }
      });
  }
  ```
- **Une source peut aussi les offrir spontanément**, par `Notification` — la
  radio le fait déjà pour `preset_count` quand sa page d'admin enregistre
  (`radio/src/main.rs:180`). Renommer une station se propage donc sans qu'on
  redemande.

Le trait gagne une méthode **à corps par défaut**, comme `can_eject`, `wake`,
`stop`, `player_track` et `set_locale` avant elle :

```rust
/// Les présélections nommées, si cette source sait les énumérer. Défaut : la
/// liste vide, qui veut dire « je n'ai que des numéros ». Le cd est dans ce cas
/// par nature — une piste n'a pas de nom sans base de données — et les fichiers
/// y restent pour l'instant.
async fn list_presets(&mut self) -> Vec<Preset> {
    Vec::new()
}
```

Le bras de `serve_source` suit le précédent exact de `SetLocale`, seul autre cas
d'une méthode qui ne rend pas de `SourceOutcome` :

```rust
SourceReq::ListPresets => {
    let presets = plugin.list_presets().await;
    SourceOutcome::new(SourceAction::Noop).presets(presets)
}
```

**Seule la radio la sert** pour commencer, depuis `Station::name` qu'elle a déjà
(`radio/src/config.rs:8`). Le cd n'a que `total_tracks` et aucun nom ; les
fichiers ont bien `Entry::display_name()` mais leur liste **est** déjà la file
d'attente, pas un jeu de présélections nommées — les deux restent au défaut, ce
qui est la vérité pour eux.

**Le garde de `handle_source_update` doit être contourné, et c'est motivé.**
`core.rs:309-312` rend la main si la trame ne vient pas de la source active :

```rust
if self.standby || name != self.active_source { return; }
```

Or le catalogue décrit **toutes** les sources — `listplaylistinfo "radio"`
s'interroge pendant que le cd joue. Les présélections sont donc lues **avant**
ce garde, dans une table `HashMap<String, Vec<Preset>>` indexée par nom de
source, la raison écrite sur place : ce n'est pas un fait sur ce qui joue, c'est
un fait sur une source, et la veille n'y change rien non plus.

Dernier piège, celui qui fait le bug classique : **les présélections sont
creuses, les positions MPD sont denses.** `Stations::preset_count()`
(`radio/src/config.rs:133`) renvoie le **maximum** des numéros, pas la longueur —
des stations 1, 5 et 99 sont légales. La correspondance est donc celle-ci, et
pas une soustraction de 1 :

| MPD | Ritornello |
|---|---|
| `Id` / `songid` | l'indice de présélection tel quel (creux, stable, ≥ 1) |
| `Pos` / `song` | le **rang** dans la liste rendue (dense, base 0) |
| `play <POS>` | l'indice de la POS-ième entrée → `Select(indice)` |
| `playid <ID>` | `Select(ID)`, directement |
| `playlistlength` | la longueur de la liste, ou `preset_count` à défaut de liste |

Sans liste (le cd, les fichiers), le greffon synthétise les entrées
`1..=preset_count` : la suite est alors dense, `Pos = indice - 1`, et rien ne se
distingue du cas nominal.

### 5. La trame de catalogue sur le protocole `display`

La doc a tranché d'avance : *« adding `SetLocale` to the display protocol is a
new message a plugin can ignore until it cares about it — non-breaking »*
(`docs/plugins.md`). Le catalogue est ce deuxième message.

**Pas dans `PlayerState`.** `publie_etat` (`core.rs:565`) déduplique par égalité
et reconstruit la trame depuis `etat_lecteur()` à chaque appel : un champ
« présent seulement quand il a changé » n'est pas une fonction de l'état
courant, c'est un événement, et mettre un événement dans un instantané est le
motif à ne pas reproduire. Toujours présent, il ferait voyager 51 noms de
station sur chacune des trames par seconde de la lecture — mesurable sur un
Pi 2 B.

Donc **son propre canal**, et une enveloppe sur le fil :

```rust
/// Une ligne du protocole `display`. Étiquetage **adjacent** et non interne :
/// `PlayerState` contient un `serde(flatten)` (`Morceau`), et le croisement
/// flatten × internally-tagged est un angle mort connu de serde. Ici le `data`
/// d'une trame d'état est exactement le JSON qui voyageait avant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", content = "data", rename_all = "lowercase")]
pub enum DisplayFrame {
    State(PlayerState),
    Catalogue(Catalogue),
}

/// Ce qui est structurel et rarement changeant : les sources déclarées, dans
/// l'ordre de bascule de `SourceCycle`, et les présélections nommées de chacune
/// quand elle sait les énumérer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalogue {
    pub sources: Vec<SourceCatalogue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceCatalogue {
    pub name: String,
    /// Vide = cette source ne sait pas énumérer. Le consommateur retombe sur
    /// `preset_count`, qui reste la vérité du nombre.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<Preset>,
}

/// Une présélection nommée. `index` est **à base 1**, celui que
/// `Command::Select` attend, et la suite peut être creuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    pub index: u8,
    pub name: String,
}
```

Le trait gagne une méthode **à corps par défaut**, donc `console` et les quatre
bouchons de test ne sont pas touchés :

```rust
#[async_trait::async_trait]
pub trait DisplayPlugin: Send + 'static {
    async fn show(&mut self, state: PlayerState) -> Result<()>;
    /// Le catalogue des sources. Défaut : ignoré — un afficheur de vingt
    /// colonnes n'en a que faire.
    async fn catalogue(&mut self, _c: Catalogue) -> Result<()> {
        Ok(())
    }
}
```

Côté cœur, `DisplayClient` (`plugin-sdk/src/client.rs:148`) gagne
`send_catalogue`, et `relais_afficheur` (`main.rs:72`) attend sur **deux**
récepteurs :

```rust
fn relais_afficheur(
    nom: String,
    client: Arc<DisplayClient>,
    mut etat_rx: watch::Receiver<PlayerState>,
    mut catalogue_rx: watch::Receiver<Catalogue>,
) {
    tokio::spawn(async move {
        // Les deux valeurs courantes partent d'emblée : un afficheur câblé à
        // chaud doit connaître le catalogue sans attendre qu'il change.
        let etat = etat_rx.borrow_and_update().clone();
        let cat = catalogue_rx.borrow_and_update().clone();
        if let Err(e) = client.send(&etat).await {
            tracing::warn!("display plugin {nom} relay stopped: {e}");
            return;
        }
        if let Err(e) = client.send_catalogue(&cat).await {
            tracing::warn!("display plugin {nom} relay stopped: {e}");
            return;
        }
        loop {
            let envoi = tokio::select! {
                r = etat_rx.changed() => match r {
                    Ok(()) => {
                        let e = etat_rx.borrow_and_update().clone();
                        client.send(&e).await
                    }
                    Err(_) => break,
                },
                r = catalogue_rx.changed() => match r {
                    Ok(()) => {
                        let c = catalogue_rx.borrow_and_update().clone();
                        client.send_catalogue(&c).await
                    }
                    Err(_) => break,
                },
            };
            if let Err(e) = envoi {
                tracing::warn!("display plugin {nom} relay stopped: {e}");
                break;
            }
        }
    });
}
```

Un canal séparé plutôt qu'une charge utile élargie : élargir republierait l'état
à chaque changement de catalogue et l'inverse, ce que la déduplication par
égalité ne rattraperait pas — les deux valeurs changeraient ensemble par
construction.

Les tests qui écrivent ou lisent une ligne nue sur un socket d'affichage passent
à l'enveloppe : `server.rs:975`, `server.rs:1029`, `client.rs:791`,
`runtime.rs:254`, et le commentaire de `metadata.rs:457` qui nomme
`run_display_plugin` devient à corriger. Les vingt tests de mise en page de
`console/src/display.rs` ne voient aucun socket et ne bougent pas.

## Le greffon lui-même

### Les modules

Un module par responsabilité, chacun testable sans réseau sauf le dernier :

| Fichier | Responsabilité |
|---|---|
| `main.rs` | environnement, chargement de la config, **liaison du TCP**, câblage du `Runtime` |
| `config.rs` | l'adresse et le port : lecture, validation, écriture atomique |
| `etat.rs` | l'état partagé, le compteur de version, le calcul des sous-systèmes changés |
| `protocole.rs` | découpage d'une ligne de commande, mise en forme des réponses et des `ACK` |
| `commandes.rs` | une fonction par commande MPD, **pures** : état en entrée, lignes en sortie |
| `session.rs` | la tâche par client : lecture, listes de commandes, `idle` |
| `admin.rs` | la page d'admin |

`commandes.rs` est le cœur de la valeur et n'a **aucune E/S** : il prend une
référence à l'état et rend des lignes. C'est ce qui rend la table de
correspondance vérifiable au test unitaire, sans socket ni horloge.

### L'ordre de démarrage

**Le TCP est lié avant l'annonce.** C'est la même doctrine que le SDK défend
pour ses sockets Unix — « lier d'abord, annoncer ensuite » est une propriété du
type `Runtime`, pas une consigne — et elle donne ici un comportement utile :
si le port 6600 est déjà pris, le greffon échoue **sans s'être annoncé**, donc
le cœur le rapporte comme mort avant annonce, avec sa ligne dans la page de
statut. Un port occupé se voit dans l'IHM au lieu de se deviner dans les
journaux.

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let config = Config::charger(&chemin_config());
    // Lié avant l'annonce : un port pris fait échouer le greffon sans qu'il
    // s'annonce, et la page de statut le montre mort au lieu de figé.
    let ecoute = TcpListener::bind((config.listen.as_str(), config.port)).await?;
    let etat = Arc::new(Etat::new());
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    tokio::spawn(accepter(ecoute, etat.clone(), cmd_tx));
    Runtime::from_args()?
        .input(EntreeMpd { rx: cmd_rx })?
        .display(AfficheurMpd { etat: etat.clone() })?
        .admin(MpdAdmin::new(config, catalogue_i18n())?)?
        .run()
        .await
}
```

### L'état partagé

```rust
pub struct Etat {
    inner: RwLock<Instantane>,
    /// Réveille les `idle` en attente. Chaque dormeur relit `version` pour
    /// savoir ce qui a changé depuis qu'il s'est endormi : un `Notify` seul
    /// perdrait un réveil arrivé entre deux attentes.
    reveil: Notify,
}

struct Instantane {
    etat: PlayerState,
    catalogue: Catalogue,
    /// Ce que le greffon **croit** de la lecture, y compris une bascule qu'il
    /// vient d'émettre et que la trame n'a pas encore confirmée. Voir la course
    /// de `pause` dans l'addition 3.
    playback_optimiste: Playback,
    /// Compteur de version de la file d'attente, celui que `status` publie sous
    /// `playlist`. Monotone, incrémenté au changement de source active ou de
    /// catalogue — jamais remis à zéro, sinon un client croirait n'avoir rien
    /// manqué.
    version_file: u32,
    /// Un compteur par sous-système, du même usage : un `idle` endormi compare
    /// et repart aussitôt si quelque chose a bougé pendant qu'il s'installait.
    versions: [u64; 4],
}
```

Le point délicat est le réveil manqué : un client qui envoie `idle` juste après
un changement doit repartir immédiatement, pas attendre le suivant. D'où des
compteurs et non un simple `Notify` : la session mémorise les versions au
moment où elle s'endort, et si elles diffèrent déjà de celles qu'elle avait
lues, elle répond sans attendre.

### Ce que chaque commande devient

| Commande MPD | Traduction |
|---|---|
| `status` | l'instantané, plus `playlist`, `playlistlength`, `state`, `song`/`songid`, `elapsed`, `duration` |
| `currentsong` | `Artist`/`Title`/`Album`/`Time`/`duration`/`Pos`/`Id` depuis `Morceau` et `preset` |
| `playlistinfo [POS]` | les présélections de la source active |
| `plchanges <version>` | identique à `playlistinfo` si la version diffère, vide sinon |
| `listplaylists` | une ligne `playlist:` par source du catalogue |
| `listplaylistinfo <nom>` | les présélections de cette source ; `ACK 50` si le nom est inconnu |
| `load <nom>` | `SelectSource(nom)` ; `ACK 50` si inconnu |
| `play [POS]` | `Select(indice de la POS-ième)`, ou `PlayPause` sans argument si arrêté |
| `playid <ID>` | `Select(ID)` |
| `pause [0\|1]` | `PlayPause` **si** l'état diffère de la cible |
| `stop` / `next` / `previous` | `Stop` / `Next` / `Prev` |
| `setvol <n>` | `SetVolume(n)`, `ACK 2` hors `0..=100` |
| `volume <±n>` | `SetVolume(volume courant + n)`, borné |
| `seek <POS> <t>` / `seekid` / `seekcur` | `SeekTo(t)` ; `ACK 2` si `t` est négatif ou non numérique |
| `idle` / `noidle` | l'attente décrite plus haut |
| `commands` | la liste réelle, celle qui rend le greffon honnête |
| `tagtypes` | `Artist`, `Album`, `Title` — les trois seuls que `Morceau` porte |
| `outputs` | une sortie unique, `outputid: 0`, activée |
| `stats` / `decoders` / `urlhandlers` | des réponses minimales mais bien formées |
| `ping` / `close` / `password` | `OK` / fermeture / `OK` sans vérification |

`seekcur` accepte la forme relative (`+10`, `-10`) : le greffon la résout depuis
`position_s` avant d'émettre un `SeekTo` absolu, puisque c'est la seule forme que
`Command` porte.

### La sourdine, un cas à ne pas rater

MPD n'a pas de sourdine : les clients coupent le son en posant `setvol 0`, et le
rétablissent en remontant le volume. Le greffon **ne traduit pas** `setvol 0` en
`Mute` — ce serait deviner. Il émet `SetVolume(0)`, et `status` rapporte
`volume: 0` quand `muted` est vrai, quel que soit le volume mémorisé : c'est ce
que le client attend de voir, et la sourdine reste pilotable depuis la
télécommande et la SPA sans que MPD la contredise.

## Emballage

Rien d'inhabituel, mais rien d'optionnel non plus :

- `Cargo.toml` de la racine : ajouter `crates/ritornello-plugin-mpd` aux
  `members`. Pas de table de dépendances de l'espace de travail — chaque crate
  épingle ses versions, on suit.
- `crates/ritornello-plugin-mpd/ui/` : paquet npm pris par le glob
  `crates/*/ui` de `package.json`, donc **aucune modification à la racine**.
  `vite.config.ts` recopié à l'identique de `generic-input` : `vue` et
  `@ritornello/ui` **externes** (carte d'import du shell, instance unique de
  Vue), `cssCodeSplit: false`, sortie plate `ui.js` + `ui.css` — le cœur ne sert
  qu'un seul segment de chemin, un `assets/chunk.js` serait un 404.
- `build.rs` + `src/placeholder.rs` recopiés : `ui/dist/` est ignoré par git,
  donc `include_str!` casserait un clone frais. Le `placeholder` exporte
  `contract = -1` et le shell affiche son message « greffon à reconstruire ».
  Attention au détail qui coûte une compilation : `#[cfg(test)] mod placeholder;`
  — le déclarer inconditionnellement déclenche `dead_code` sous `-D warnings`.
- `deploy/deploy.sh:14` : ajouter `mpd` au tableau `PLUGINS`. **Le script refuse
  de déployer** si ce tableau et `deploy/plugins.example.toml` ne nomment pas le
  même ensemble — la garde est explicite, donc les deux vont ensemble.
- `deploy/plugins.example.toml` : un bloc `[[plugin]]` `name`/`exec`, sans
  `kind` (le binaire l'annonce).
- `deploy/mpd.example.toml` : l'adresse et le port par défaut.
- i18n : `src/locales/en.toml` embarqué et `deploy/locales/mpd/fr.toml` livré
  (l'arbre `deploy/locales` est copié en entier, aucun script à toucher), plus
  les **deux** tests de parité qui existent partout ailleurs —
  `parite_des_cles_entre_len_embarque_et_le_pack_fr` côté Rust et
  `i18nKeysUsed.test.ts` côté vitest.

Aucune modification du cœur ni de la SPA pour la navigation : `App.vue:65`
construit ses liens depuis `/api/status` avec un `[...new Set(...)]` dont le
test de régression nomme déjà `mpd` (`App.test.ts:87`) — deux lignes de statut,
un seul lien.

## Tests

Trois niveaux, et **aucun** qui repose sur une marge d'horloge murale : c'est la
classe de flake que le chantier précédent a passé son temps à éliminer, et la
leçon est écrite — une propriété juste prouvée par une durée casse sous la
charge des binaires de test concurrents.

1. **`protocole.rs`, sans réseau.** Découpage d'une ligne : arguments simples,
   guillemets, espaces dans les guillemets, `\"` et `\\` échappés, guillemet non
   fermé (erreur), ligne vide. Mise en forme : `ACK` aux trois codes, avec
   l'indice de liste correct.
2. **`commandes.rs`, sans réseau ni horloge.** Un instantané construit à la main,
   puis les lignes attendues pour chaque commande. C'est là que vivent les
   assertions qui comptent : les positions denses face aux indices creux,
   `playlistlength` égal à la longueur de la liste et non au maximum,
   `volume: 0` quand `muted`, `state` dans les trois cas, `song` absent à
   l'arrêt, `ACK 50` sur un nom de liste inconnu.
3. **`session.rs`, sur une vraie boucle locale.** `TcpListener::bind("127.0.0.1:0")`
   puis `local_addr()` — le motif « le test lie, le serveur reçoit l'écouteur »
   de `register.rs:333`, qui évite la boucle de reprise. Dialogue complet :
   bannière `OK MPD`, une commande, une liste de commandes avec `list_OK`, une
   liste dont la deuxième commande échoue (les suivantes ne s'exécutent pas), un
   `idle` réveillé par une trame poussée, un `noidle`, et **deux clients
   simultanés** dont l'un dort quand l'autre agit.

Côté cœur et SDK : la pause suivie et publiée dans les trois états, l'enveloppe
d'affichage aux deux formes, `ListPresets` sur la radio et son défaut vide sur
le cd, et le catalogue qui traverse jusqu'à un afficheur bouchon.

Un test de non-régression mérite son nom : **le catalogue ne doit pas voyager
avec chaque trame d'état**. Un afficheur bouchon compte ses appels à
`catalogue()` pendant que dix trames d'état passent, et n'en attend qu'un.

## Ce qui reste non vérifié

- **Aucun client MPD réel n'a encore parlé à ce greffon**, et c'est le seul juge
  qui compte. M.A.L.P. est la cible ; l'essai se fait sur l'appareil, pas en
  test.
- **Rien n'a jamais tourné sur le Pi**, ce chantier comme les précédents. Le port
  6600 n'est ouvert par aucune règle de pare-feu du dépôt — il n'y en a aucune,
  donc a priori rien à faire, mais c'est à constater et non à supposer.
- La correspondance présélections ↔ listes enregistrées est cohérente mais
  **inhabituelle** : un client qui suppose qu'une file d'attente s'édite trouvera
  toutes ses commandes d'écriture refusées. `commands` le lui dit, encore
  faut-il qu'il le lise.

</content>
