# Rendez-vous des greffons : annonce, multi-genres, afficheurs multiples

Date : 2026-08-24

## Le problème

Le cœur ne dispose d'aucun moyen de se faire dire « je suis prêt » par un
greffon. Il devine, de deux façons :

- `connect_with_retry` (`plugin-sdk/src/client.rs:15`) retente un `connect`
  100 fois espacées de 100 ms — **10 s de budget** — parce que le socket du
  greffon n'existe pas encore quand le cœur tente de s'y connecter.
- `plugins::attend_liaison` sonde le système de fichiers pendant **2 s** pour
  savoir si un greffon a une page d'admin. Ce délai se paie **au succès** :
  conclure « ce greffon n'a pas de page » exige la fenêtre entière, à chaque
  démarrage sain. Seuls `radio`, `files` et `generic-input` ont une page :
  cinq greffons sur huit n'en ont pas, donc ces 2 s sont sur le chemin
  critique de tout démarrage.

Trois limites s'y ajoutent :

- **Un greffon ne peut porter qu'un seul genre.** `PluginConfig.kind` est un
  `PluginKind` unique et le câblage est un `match p.kind`. Or le socket est
  nommé d'après le **nom** (`{runtime_dir}/{name}.sock`), donc déclarer deux
  fois le même binaire fait que le second `spawn` supprime le socket du
  premier — `plugins::spawn` fait `remove_file` avant de lancer.
- **Les afficheurs sont un singleton, et c'est un bug.** `main.rs:186` fait
  `display_connect = Some(...)` dans une variable simple : déclarer deux
  greffons `display` ne produit aucune erreur, les deux processus démarrent,
  les deux voient le cœur se connecter à leur socket, mais le cœur jette la
  première `JoinHandle` (en tokio, cela détache la tâche, ne l'annule pas) et
  ne garde que le client du dernier déclaré. Le premier afficheur attend des
  lignes qui n'arriveront jamais et n'apparaît même pas dans
  `plugin_statuses`.
- **Le genre est une propriété du binaire, déclarée par l'opérateur.** Le
  dépôt a déjà tranché ce débat une fois, pour la page d'admin : le champ
  `admin = true` a été supprimé parce que « c'était une propriété du binaire
  que l'opérateur devait connaître, et son oubli produisait un mode dégradé
  silencieux » (`plugins.rs`). Le genre est dans la même situation.

## La décision

Le cœur **lie un socket d'enregistrement avant tout lancement**. Chaque
greffon lie ses propres sockets de genre, puis s'y connecte et annonce en une
ligne ses genres et l'existence de sa page d'admin.

Le point qui fait tout fonctionner est l'**ordre** : le greffon lie ses
sockets *avant* de s'annoncer. L'annonce devient donc une **barrière de
disponibilité** — quand le cœur la lit, il sait à la fois quels genres
existent et que les sockets correspondants sont liés. Elle remplace d'un coup
la fenêtre de 2 s et la boucle de 10 s, qui n'ont plus rien à deviner.

Un délai se paie désormais **à l'échec** et non plus au succès.

### Ce qui est explicitement accepté comme perte

Ces points ont été discutés et arbitrés avant rédaction :

- **`plugins.toml` ne dira plus quelles entrées sont des `metadata`.** La
  chaîne d'arbitrage n'est plus lisible en configuration. L'ordre, lui,
  survit : la liste reste ordonnée et c'est elle qui arbitre.
- **Aucune rétrocompatibilité.** Bascule sèche : un greffon qui ne s'annonce
  pas n'est pas câblé, sans repli sur un `kind` de fichier. Justifié par
  l'absence d'auteurs tiers, une cible de déploiement unique, et le fait que
  cœur et greffons sont livrés ensemble depuis le même dépôt.

### Ce qui n'est pas fait ici

Le greffon MPD qui a motivé la discussion n'est pas dans ce chantier. Celui-ci
ne livre que ce qui le rend possible.

## Architecture

### Répertoire d'exécution par exécution

`{runtime_dir}/sockets/` (avec `runtime_dir` = `RITORNELLO_RUNTIME_DIR`,
défaut `/run/ritornello`) est **supprimé puis recréé au démarrage du cœur**,
avant tout lancement.

Cela rend les fichiers rances impossibles par construction, plutôt que de
reposer sur la pré-suppression au cas par cas de `plugins::spawn`. Le
nettoyage devient un `remove_dir_all`. Une seule instance du cœur par
`runtime_dir` — garanti par `RuntimeDirectory=` de systemd en service, et par
une variable d'environnement distincte en développement.

### Chronologie

```
t0  cœur    rm -r {runtime_dir}/sockets ; mkdir
t1  cœur    bind({sockets}/register.sock) ; accept() en boucle
t2  cœur    plugins.toml → (name, exec) dans l'ordre ; fork+exec de chacun
t3  greffon lie SES sockets de genre, puis celui d'admin s'il a une page
t4  greffon connect(register.sock) → réussit du premier coup
t5  greffon écrit UNE ligne, puis ferme
t6  cœur    lit → connaît les genres ET sait que les sockets sont liés
t7  cœur    connect sur chaque socket annoncé → réussit du premier coup
t8  cœur    Core::new
```

`t3` avant `t4` est l'invariant central. Il est **structurel dans le SDK**, pas
une consigne : les sockets sont liés par les méthodes du constructeur, et
l'annonce n'est écrite que par `run()`.

À `t7`, un `UnixStream::connect` nu suffit. `UnixListener::bind` de tokio fait
bind+listen, donc la connexion aboutit même avant le premier `accept()` du
greffon, grâce au backlog du noyau.

### La ligne d'annonce

Nouveau module `ritornello-proto/src/register.rs` :

```rust
pub struct Announcement {
    pub name: String,
    pub kinds: Vec<PluginKind>,
    #[serde(default)]
    pub admin: bool,
}
```

Exemple sur le fil :

```json
{"name":"radio","kinds":["source"],"admin":true}
```

**`PluginKind` déménage** de `ritornello-core::plugins` vers
`ritornello-proto` : le SDK doit pouvoir le sérialiser, et le cœur ne peut
plus en être le propriétaire exclusif.

**Le nom reste autoritaire côté fichier.** Le cœur passe `--name <name>` et le
greffon le renvoie tel quel. Le binaire n'invente pas son identité : sinon
deux greffons pourraient réclamer le même nom et collisionner sur les chemins
de sockets. Le champ `name` de l'annonce sert uniquement à **corréler** N
annonces arrivant sur un socket unique.

### Ligne de commande d'un greffon

Le cœur lance chaque greffon avec :

- `--register {sockets}/register.sock`
- `--name {name}`
- `--socket-prefix {sockets}/{name}`
- `RITORNELLO_LOCALE` en environnement (inchangé)

Le greffon suffixe le préfixe lui-même : `{prefix}-source.sock`,
`{prefix}-display.sock`, …, `{prefix}-admin.sock`. Le cœur garde ainsi la
maîtrise du répertoire et du préfixe ; le greffon n'a autorité que sur les
suffixes qu'il annonce. Combiné au répertoire neuf, aucun nettoyage au glob
n'est nécessaire.

Disparaissent : `--socket`, `--admin-socket`, `socket_path()`,
`admin_socket_path()`.

### Le rassemblement, côté cœur

Après les lancements, une boucle `select!` sur trois sources :

1. **nouvelle connexion sur `register.sock`** → lire une ligne → `Announcement`
2. **`plugin_waits` (le `FuturesUnordered` de `child.wait()`)** → ce greffon est
   mort avant de s'annoncer, cesser de l'attendre
3. **échéance globale de 10 s**

La boucle s'arrête dès que chaque entrée du manifeste est soit annoncée, soit
morte — donc en pratique bien avant l'échéance. Un greffon qui plante au
démarrage est diagnostiqué **plus vite qu'aujourd'hui**, où il fait tourner
les 10 s de `connect_with_retry` à vide.

L'échéance dépassée devient une **erreur imputable à un greffon nommé**, non
plus une déduction.

### Câblage multi-genres

`match p.kind` devient, dans l'ordre du manifeste :

```
pour chaque entrée du manifeste (ordre du fichier)
  si annoncée
    pour chaque genre annoncé → connect + câblage (identique à aujourd'hui)
    si admin → connect sur {prefix}-admin.sock
```

`PluginConfig.kind` est supprimé de la structure. Rien n'ayant jamais été
livré, aucune base installée n'est à ménager : le champ n'est pas traité comme
un héritage à tolérer, il est simplement absent du modèle.

### Afficheurs multiples

`display_client: Option<Arc<DisplayClient>>` devient
`display_clients: Vec<Arc<DisplayClient>>`.

Le relais d'état lance **une tâche par afficheur**, chacune avec son propre
`etat_rx.clone()` du canal `watch` — et non une tâche unique qui boucle sur N
clients. C'est ce qui empêche un afficheur lent de retarder les autres : la
contre-pression reste cloisonnée par socket, ce qui était l'argument même
retenu pour ne pas fusionner les sockets.

L'avertissement « no display plugin connected » n'est émis que si le vecteur
est vide.

### Statuts

`PluginStatus` garde `kind: String`, et un greffon multi-genres produit
**une ligne par (nom, genre)**. La clé de rendu de la page de statut devient
`nom + genre` et non le seul nom.

### Ordre d'arbitrage des métadonnées

Aujourd'hui `metadata_plugins: Vec<String>` est construit depuis le manifeste
**avant tout lancement**, précisément pour qu'il « ne change pas d'un
démarrage à l'autre » (`main.rs:126`).

Après : construit **après** le rassemblement, en parcourant
`manifest.plugins` dans l'ordre du fichier et en retenant les entrées dont
l'annonce contient `Metadata`. La garantie est préservée, mais elle passe
d'acquise-par-construction à maintenue-par-le-code : elle doit donc être
couverte par un test qui fait arriver les annonces dans l'ordre inverse du
manifeste et vérifie que l'arbitrage n'en dépend pas.

## Le SDK

Les cinq traits (`SourcePlugin`, `DisplayPlugin`, `InputPlugin`,
`MetadataPlugin`, `AdminPlugin`) **ne changent pas d'une ligne**, et les cinq
protocoles de fil non plus. C'est le cœur de la stratégie : le risque est
concentré dans le chemin d'enregistrement, qui est neuf, et non dans les
protocoles, qui sont éprouvés.

Chaque `run_*_plugin` est scindé en deux :

- `bind_*(path) -> Listener` — appelé par le constructeur
- `serve_*(listener, plugin)` — la boucle actuelle, inchangée

`run_*_plugin` subsiste comme mince enveloppe `bind` + `serve`, ce qui laisse
**les tests de protocole existants intacts** — leur non-modification est la
preuve que les protocoles n'ont pas bougé.

Nouveau constructeur :

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ritornello_plugin_sdk::Runtime::from_args()?
        .source(RadioSource::new()?)   // lie {prefix}-source.sock
        .admin(RadioAdmin::new()?)     // lie {prefix}-admin.sock
        .run()                          // annonce, puis sert tous les genres
        .await
}
```

`run()` déduit les `kinds` des méthodes appelées et `admin` de la présence
d'un `.admin(...)`. L'annonce ne peut donc pas mentir sur ce qui est lié.

Les huit binaires de greffons passent à cette forme ; pour un greffon
mono-genre c'est trois lignes.

## Gestion d'erreur et modes dégradés

| Situation | Comportement |
|---|---|
| Greffon ne s'annonce jamais | Échéance ; `connected: false` ; greffon **nommé** dans le journal ; le cœur continue |
| Greffon meurt avant de s'annoncer | Détecté par `child.wait()`, immédiatement, sans attendre l'échéance |
| Nom inconnu ou dupliqué dans une annonce | Avertissement nommant le nom reçu ; annonce ignorée |
| Ligne d'annonce illisible | Avertissement ; greffon traité comme non annoncé |
| Un socket de genre refuse la connexion malgré l'annonce | Ce genre est indisponible ; **les autres genres du même greffon continuent** (l'isolation par socket est préservée) |
| Aucune source | Comportement actuel inchangé |
| Aucun afficheur | Avertissement, le cœur continue |

L'invariant documenté « la panne d'un plugin metadata ne concerne que les
métadonnées ; **la lecture n'est jamais affectée** » (`main.rs:207`) est
conservé sans effort, puisque chaque genre garde son propre socket.

## Ce qui est supprimé

- `plugins::attend_liaison` et ses deux tests
- `client::connect_with_retry` — remplacé par un `connect` nu
- les arguments `--socket` / `--admin-socket` et leurs accesseurs
- `PluginConfig.kind`
- la fenêtre de 2 s et le budget de 10 s

## Tests

**SDK, unitaires**

- format de la ligne d'annonce (aller-retour serde)
- **les sockets de genre sont liés avant que l'annonce soit lisible** : le test
  lit l'annonce puis vérifie que chaque socket annoncé accepte une connexion
  immédiate, sans attente
- `Runtime` servant deux genres simultanément sur un même processus
- les tests de protocole existants, **non modifiés**

**Cœur, unitaires**

- rassemblement complet sur N annonces
- échéance : un greffon muet est rapporté `connected: false` et nommé
- mort précoce : `child.wait()` écourte l'attente
- nom inconnu, puis nom dupliqué
- **ordre des métadonnées indépendant de l'ordre d'arrivée des annonces**
- diffusion vers deux afficheurs ; un afficheur lent ne retarde pas l'autre
- une ligne de statut par (nom, genre)

**Non-régression**

`cargo test` sur l'espace de travail complet. `build.rs` du cœur écrit un
bouchon si `web/app/dist` manque, donc les tests Rust ne demandent pas de
construire l'IHM.

## Documentation à reprendre

- `docs/plugins.md` (902 lignes) : la section « Declaring the plugins », et les
  parties « Writing a `metadata` plugin » et « A plugin's UI » qui décrivent
  la découverte actuelle
- `deploy/plugins.example.toml` : retirer les `kind`, et reformuler le
  paragraphe sur l'ordre des métadonnées — l'ordre arbitre toujours, mais le
  fichier ne dit plus quelles entrées sont concernées
- `deploy/missing-plugins.awk` : **aucun changement**, il apparie les blocs par
  `name` et recopie les blocs tels quels

## Risques

1. **Huit binaires à migrer d'un coup.** Bascule sèche assumée : un greffon
   oublié ne s'annonce pas et n'est pas câblé. Atténuation : le rassemblement
   le nomme dans le journal, et `cargo test` de l'espace de travail ne
   compile pas un `main()` resté sur l'ancienne API.
2. **La garantie d'ordre des métadonnées change de nature** (voir plus haut) —
   couverte par un test dédié.
3. **`PluginKind` change de caisse**, ce qui touche les imports du cœur.
   Mécanique, mais large.
