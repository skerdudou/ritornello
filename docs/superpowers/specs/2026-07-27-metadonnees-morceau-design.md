# ritornello — Métadonnées du morceau en cours

Afficher l'artiste et le titre du morceau joué, pour la radio comme pour le CD,
par un **quatrième genre de plugin** (`metadata`) qui enrichit ce que joue la
Source active sans que celle-ci le sache. Le cœur lit en base les métadonnées
que mpv reçoit du flux (ICY) ; un plugin `metadata` peut les surcharger.

Date : 2026-07-27 — Statut : **implémenté** (voir « Écarts assumés à
l'implémentation » en fin de document)

## Contexte

Aujourd'hui, aucune des deux sources n'expose proprement le morceau en cours :

- La **radio** n'affiche rien : le cœur ne lit pas les métadonnées ICY du flux,
  alors que mpv les reçoit déjà et les expose dans sa propriété `metadata`
  (clé `icy-title`).
- Le **CD** affiche des titres, mais c'est le plugin cd qui interroge
  MusicBrainz lui-même : `musicbrainz.rs` (94 lignes), un seul point d'appel
  (`main.rs:79`, `musicbrainz::lookup(&toc, n)`), résultat remonté par
  `poll_notification()`. Un plugin qui pilote du matériel porte donc un appel
  réseau lent dans le processus qui doit aussi répondre aux commandes de piste.

### Ce qui a été mesuré avant d'écrire cette spec

| Flux | `icy-metaint` | `StreamTitle` réellement émis |
|---|---|---|
| OUI FM `ouifm-high.mp3` | 16000 | `'Now Playing info goes here'` (texte de remplissage) |
| SomaFM Groove Salad *(témoin)* | 45000 | `'Mandrillus Sphynx - Bikwix'` |
| Radio Nova `novazz-128` | 16000 | vide |
| FIP `fip-midfi.mp3` / `fip-hifi.aac` | absent | — |

Sur cinq flux, **un seul** donne un artiste/titre exploitable par l'ICY, et
c'est une webradio étrangère. L'ICY seul ne suffit donc pas pour les stations
françaises courantes.

OUI FM expose en revanche un `text/event-stream` de première main :
`https://www.ouifm.fr/ws/metas?id=<id-de-webradio>`. Mesuré : HTTP/2 200,
**aucune authentification** (ni cookie, ni referer), `artist` et `title` **déjà
séparés**, plus `durationInSeconds`, `coverId`, `appleMusicId`, `mdsId`,
`type`, `origin:"mds"`. Le morceau courant est poussé **dès la connexion**
(pas de démarrage à froid), et 25 s de flux pèsent 536 octets. L'`id` est
propre à chaque webradio.

Deux contraintes structurelles relevées dans le code existant :

- `SourceAction::Play { uri }` **traverse** le cœur (`core.rs:245-247`, il la
  passe à mpv) mais n'est **pas retenue** : `Core` n'a aucun champ d'URL.
- `View` est `{ line1, line2, line3 }` : de la **présentation**. Des
  métadonnées structurées ont besoin de leur propre chemin, pas d'être tassées
  dans `line3`.

### Pourquoi ni la configuration par station ni un plugin Source dédié

Deux conceptions ont été examinées et écartées :

- **Un descripteur de flux de métadonnées par station dans `stations.toml`** :
  incompatible avec le parcours réel d'ajout d'une station. Depuis l'annuaire
  Radio Browser (spec `2026-07-23-annuaire-radio-design.md`), une station
  arrive avec nom, URL, codec, débit et pays — **et rien d'autre**. Un champ de
  métadonnées devrait être rempli à la main pour chaque station, ce qui annule
  l'intérêt de l'annuaire. De plus, rien ne garantit que le JSON d'une autre
  radio ait la même forme que celui d'OUI FM.
- **Un plugin Source dédié `ouifm`** : couperait la liste de présélections en
  deux Sources à faire défiler, dupliquerait la logique de lecture du plugin
  radio, et ne généraliserait pas — chaque station à flux propre demanderait un
  nouveau plugin Source.

## Décisions de cadrage

| Sujet | Décision |
|---|---|
| Mécanisme | **Quatrième genre de plugin** : `metadata`, avec son socket dédié, à côté de Source, Input et Display. |
| Forme du protocole | **Bidirectionnel non corrélé** : cœur → plugin (« voici ce qui joue »), plugin → cœur (« voici ce que j'en sais »). Ni requête/réponse par `id` comme Source, ni sens unique comme Display et Input. |
| Identité de ce qui joue | **Opaque** (`serde_json::Value`), produite par la Source, **jamais interprétée par le cœur** — même principe que le JSON opaque du protocole `admin`. |
| Péremption | Tout enrichissement **réécho l'identité** ; le cœur jette celui qui ne correspond pas à l'identité courante. |
| Couche de base | Le cœur observe `icy-title` de mpv et l'affiche **brut**, sans découpage heuristique. |
| Surcharge | Un enrichissement de plugin, s'il correspond à l'identité courante, **gagne** sur l'ICY. |
| MusicBrainz | **Extrait** du plugin cd vers un plugin `metadata`, donc interchangeable. |
| Déclaration | Un plugin `metadata` doit être **déclaré** dans `plugins.toml` : sans lui, pas d'enrichissement. Assumé, ce n'est pas traité comme une régression. |
| Chemin vers la SPA | Une route **`GET /api/player`** en `text/event-stream`, alimentée par un canal `watch` du cœur. **Pas** de champ dans `/api/status`. |
| Composition de l'affichage | **Le cœur** compose `View`. Le protocole Display reste inchangé, les afficheurs restent passifs. |
| Arbitrage entre plugins | **L'ordre de déclaration** dans `plugins.toml` : le premier déclaré qui répond gagne, un plugin plus bas ne l'écrase jamais. |
| Périmètre | Affichage seulement. Aucune pochette, aucun historique, aucune recherche. |

## Le protocole

Nouveau module `ritornello-proto/src/metadata.rs`. Trames JSON par ligne, comme
les autres genres, mais **sans corrélation par `id`** : chaque côté émet quand
il a quelque chose à dire.

```rust
/// Cœur → plugin. Émis à chaque changement de ce qui joue, et à l'arrêt
/// (`identity: None`) pour que le plugin cesse son travail.
#[derive(Serialize, Deserialize)]
pub struct NowPlaying {
    /// Nom de la Source active (`"radio"`, `"cd"`…), pour qu'un plugin
    /// puisse se taire d'emblée sur une source qu'il ne traite pas.
    pub source: String,
    /// Identité opaque, produite par la Source. `None` = plus rien ne joue.
    pub identity: Option<serde_json::Value>,
}

/// Plugin → cœur. Émis quand le plugin apprend quelque chose.
#[derive(Serialize, Deserialize)]
pub struct Enrichment {
    /// **Écho** de l'identité concernée : c'est le garde-fou de péremption.
    pub identity: serde_json::Value,
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub duration_s: Option<u32>,
}
```

Le cœur compare deux `Value` par égalité ; il ne sait pas qu'une identité de
flux contient une URL ni qu'une identité de disque contient une empreinte.

## L'identité, produite par la Source

C'est la Source qui sait ce qu'elle joue, donc c'est elle qui décrit son
identité. Le radio y met par exemple
`{"kind":"stream","url":"https://ouifm.ice.infomaniak.ch/ouifm-high.mp3"}`, le
cd `{"kind":"disc","discid":"…","track":3}`.

**Point de conception résolu.** Un CD change de morceau **sans nouveau
`Play`** : `PlayerNext` fait avancer mpv, l'identité change, mais aucun
`SourceAction::Play` n'est émis. L'identité ne peut donc pas voyager
uniquement dans `Play`. Elle voyage **à côté de la vue** :

```rust
pub struct SourceOutcome {
    pub action: SourceAction,
    pub view: Option<View>,
    /// Identité de ce qui joue après cette action. `Some(None)` = plus rien.
    pub identity: Option<Option<serde_json::Value>>,
}
```

et `poll_notification()` renvoie désormais une mise à jour portant les deux
(`view` et `identity`) au lieu d'une `View` seule. Conséquence : toute occasion
où une Source rapporte une vue est aussi une occasion de corriger l'identité —
ce qui couvre exactement le changement de piste d'un disque, la sélection d'une
présélection, et l'arrivée différée d'une TOC.

## Les deux couches et la résolution

Le cœur tient trois éléments d'état :

- `current_identity: Option<Value>` — ce qui joue ;
- `icy_title: Option<String>` — dernière valeur d'`icy-title` vue de mpv ;
- `enrichment: Option<Enrichment>` — dernier enrichissement **correspondant**.

Résolution, dans cet ordre : l'enrichissement s'il correspond à
`current_identity`, sinon l'`icy_title` brut, sinon rien.

**Au changement d'identité, les deux sont vidés immédiatement.** Ne pas laisser
le morceau précédent à l'écran pendant qu'on attend le suivant est un
comportement, pas un détail d'implémentation.

Cette règle de priorité résout du même coup le cas mesuré d'OUI FM : son ICY
vaut `'Now Playing info goes here'`, et la surcharge du plugin l'écrase, donc le
texte de remplissage n'apparaît jamais. Pour une station sans plugin
correspondant, on affiche ce qu'elle émet, **tel quel** — y compris son propre
nom ou ses jingles. Aucun découpage sur `" - "` : le témoin SomaFM montre que la
convention existe, mais elle n'est pas garantie, et un enrichissement de plugin
fournit de toute façon des champs déjà séparés.

## Côté cœur

- `Core` gagne les trois champs ci-dessus, plus l'envoi de `NowPlaying` aux
  plugins `metadata` déclarés à chaque changement d'identité.
- **Lecture de l'ICY** : le pilote mpv (`player/mpv.rs`) ajoute un
  `observe_property` sur `metadata` et remonte les `property-change` par le
  canal d'événements qu'il possède déjà. Une valeur vide ou inchangée ne
  déclenche rien.
- L'enrichissement retenu alimente la `View` poussée aux plugins Display **et**
  un état structuré diffusé à la SPA (voir les trois décisions ci-dessous).
- **Précision sur la lecture de l'ICY.** `Event::Title` existe déjà dans le
  pilote mpv (il vient de `media-title`), mais le cœur **en jette le
  contenu** : `core.rs:215` filtre `Event::Title(_) | Event::PlaybackActive`
  d'un motif `_` et ne s'en sert que comme signal de vivacité du flux. Le titre
  connu de mpv n'atteint donc aujourd'hui aucun affichage. La boucle
  d'événements existe, seul l'acheminement du contenu reste à écrire.
  On observe `metadata` et on lit la clé `icy-title`, **plutôt que de réutiliser
  `media-title`** : ce dernier retombe sur l'URL du flux quand la station
  n'envoie rien, ce qui afficherait une URL en guise de titre.
- Un plugin `metadata` injoignable est marqué indisponible comme n'importe quel
  autre plugin ; **la lecture n'est jamais affectée**.

## Décision — le chemin jusqu'à la SPA : une route `text/event-stream`

**`GET /api/player`, un flux poussé, alimenté par le canal `watch` que le
cœur possède déjà.** Pas de champ dans `/api/status`.

Ce que fait la SPA aujourd'hui a été vérifié avant de trancher : elle **ne
sonde rien**. Aucun `setInterval`, aucun `EventSource`, aucun WebSocket dans
`web/app/src`. `/api/status` est lue **une seule fois par montage**, à deux
endroits — `App.vue:17` pour construire la navigation à partir des plugins
`admin`, et `HomeView.vue:16` pour la source active.

C'est ce qui condamne le champ dans `/api/status` : cette charge utile est le
**contrat de la navigation**, structurellement stable — elle change quand un
plugin meurt, pas toutes les trois minutes. Y greffer un état volatil impose
l'un de deux mauvais résultats : soit la SPA se met à sonder `/api/status`
toutes les quelques secondes, donc à retélécharger la liste des plugins en
permanence pour afficher un titre malgré tout périmé de N secondes ; soit le
champ n'est jamais vu, ce qui est exactement ce qui se produirait aujourd'hui
puisque rien ne sonde.

Le cœur diffuse en revanche **déjà** chaque changement de vue par un
`tokio::sync::watch` (`view_tx`, envoyé dans `push_view` à `core.rs:317`, dont
la tâche du plugin Display est consommatrice via `main.rs:75`). Une route SSE
qui s'abonne à un canal `watch` est de quelques lignes, n'ajoute aucun état, et
la sémantique de `watch` est précisément la bonne : seule la dernière valeur
compte, les intermédiaires n'ont aucune valeur, et un client lent ne peut pas
bloquer le producteur. Un état structuré `NowPlayingState` (source, artiste,
titre, album, durée, origine `icy` ou nom du plugin) est diffusé sur son propre
canal `watch`, à côté de `view_tx` — la SPA reçoit du structuré, le plugin
console reçoit des lignes, chacun son chemin.

Bénéfice de symétrie : on consomme du SSE d'OUI FM et on en produit vers le
navigateur, donc un seul modèle mental ; et `EventSource` se reconnecte tout
seul, ce qui évite toute logique de reprise côté client.

Options écartées :

- **Champ dans `/api/status` plus sondage** : couple le volatil au structurel,
  retélécharge un contrat de navigation stable pour rien, et reste en retard
  d'un intervalle de sondage.
- **WebSocket** : de la machinerie bidirectionnelle pour un flux à sens unique,
  plus de code sur un Pi 2 sans contrepartie.
- **Nouvelle route sondée `GET /api/player`** : plus simple à écrire, mais
  échange de la latence contre des requêtes inutiles sur un appareil le plus
  souvent inactif — et le canal `watch` déjà présent rend le poussé moins
  coûteux que le tiré.

## Décision — la composition des lignes : par le cœur

**Le cœur compose, le protocole Display reste inchangé.**

C'est déjà lui qui compose : `push_view`, la logique d'overlay et
`handle_source_view` sont dans `core.rs`. Les plugins Display sont
délibérément passifs — le protocole est à sens unique cœur → plugin, et le
plugin console se contente d'imprimer. Déplacer la composition chez eux
obligerait chaque afficheur futur (l'OLED SSD1306 est prévu) à réimplémenter
les mêmes règles de repli, et exigerait de leur envoyer des métadonnées
structurées, donc de changer le protocole Display — ce que cette spec veut
justement éviter.

**Quelle ligne.** L'occupation actuelle a été relevée :

| Source | `line1` | `line2` | `line3` |
|---|---|---|---|
| radio | `RADIO  P<preset>` | nom de la station | **vide** |
| cd | album | `artiste — album` | titre de la piste |

`line3` est donc libre sur la radio, et occupée sur le CD par une information
qui, **après la migration de MusicBrainz hors du plugin cd, reviendra
précisément sous forme d'enrichissement**. `line3` devient la ligne des
métadonnées pour les deux sources, sans conflit : c'est une conséquence de la
migration, pas une coïncidence.

**Règles de repli**, à écrire une fois pour toutes :

| Connu | `line3` |
|---|---|
| artiste et titre | `artiste — titre` (tiret cadratin, déjà la convention de `line2` du CD) |
| titre seul | le titre |
| artiste seul | l'artiste — une information partielle vaut mieux que rien |
| ni l'un ni l'autre | **inchangée** : le cœur ne vide jamais une ligne que la Source a écrite |

Options écartées :

- **Composition par le plugin Display** : duplication de ces règles dans chaque
  afficheur, et changement du protocole Display.
- **Envoyer le structuré aux Display en plus des lignes** : deux
  représentations du même état à maintenir cohérentes, pour un plugin console
  qui n'en ferait rien.

## Décision — l'arbitrage entre deux plugins : l'ordre de déclaration

**Le premier plugin `metadata` déclaré dans `plugins.toml` qui répond pour une
identité gagne, et un plugin déclaré plus bas ne l'écrase jamais.**

Le critère retenu est la **prévisibilité pour qui débogue**, pas l'élégance :

- **« Premier arrivé »** est temporellement non déterministe — le gagnant
  dépend de la latence réseau, donc la même installation donne des résultats
  différents d'un démarrage à l'autre. C'est la pire propriété possible pour
  diagnostiquer un affichage douteux.
- **« Dernier arrivé »** laisse un plugin lent écraser silencieusement un
  plugin rapide et correct, et l'affichage peut osciller entre deux valeurs
  tant que les deux continuent d'émettre.
- **L'ordre de déclaration** est écrit dans un fichier que l'opérateur
  contrôle, visible sans instrumentation, et stable d'une exécution à l'autre.
  La règle se lit dans `plugins.toml` seul, sans rien savoir des temps de
  réponse.

Le cœur retient donc, **par identité**, l'enrichissement du plugin de plus
haute priorité ayant répondu, et journalise en `debug` lequel a gagné. Deux
corollaires à inscrire :

- un enrichissement dont **tous** les champs sont vides compte comme une
  non-réponse, sinon un plugin qui reconnaît l'identité mais n'a encore rien
  appris bloquerait un plugin moins prioritaire qui, lui, sait ;
- au changement d'identité, l'ardoise est remise à zéro pour tous les plugins,
  comme le reste de l'état de résolution.

## Côté SDK

Nouveau trait et exécuteur, sur le modèle de `run_admin_plugin` :

```rust
#[async_trait]
pub trait MetadataPlugin: Send + 'static {
    /// Ce qui joue a changé. Le plugin décide s'il sait faire quelque chose.
    async fn now_playing(&mut self, np: NowPlaying);
    /// Prochain enrichissement disponible. Ne se termine jamais s'il n'y a
    /// rien à dire (même convention que `poll_notification`).
    async fn next_enrichment(&mut self) -> Enrichment;
}
```

## Les deux plugins livrés

- **`ritornello-plugin-ouifm-metas`** : reconnaît l'hôte et le mount d'une
  identité de flux, en déduit le `mdsId` d'une table qu'il embarque, ouvre le
  `text/event-stream`, émet un enrichissement par événement. URL non reconnue :
  il se tait. La table de correspondance est la pièce fragile — les mounts et
  les identifiants d'OUI FM peuvent changer — donc son échec est **silencieux**.
- **`ritornello-plugin-musicbrainz`** : reçoit une identité de disque, interroge
  MusicBrainz (le code de `musicbrainz.rs` déménage ici), émet un enrichissement
  par piste.

## Impact sur le plugin cd

Il devient du CD pur (~220 lignes) : TOC, pistes, éjection. Il perd
`musicbrainz.rs` et la partie de `poll_notification()` qui attendait le
résultat réseau ; il **garde** `poll_notification()` pour la piste courante et
l'arrivée de la TOC, et gagne la production de son identité. Ses tests de
lecture de TOC et de bornage de piste sont conservés ; ceux qui portaient sur
MusicBrainz déménagent avec le code.

## Déploiement

`deploy/plugins.example.toml` déclare les deux nouveaux plugins et
`deploy/deploy.sh` installe leurs binaires. Le README documente le genre
`metadata`, le fait qu'un plugin doit être déclaré pour obtenir un
enrichissement, et la couche ICY brute par défaut.

## Tests

- **proto** : roundtrip JSON de `NowPlaying` (avec et sans identité) et
  d'`Enrichment`.
- **SDK** : `run_metadata_plugin` bout à bout sur socket réel en tempdir —
  réception d'un `NowPlaying`, émission d'un `Enrichment`.
- **cœur** : un enrichissement dont l'identité **ne correspond pas** est
  ignoré ; un changement d'identité vide `icy_title` **et** `enrichment` ; la
  résolution respecte l'ordre enrichissement → ICY → rien ; un plugin
  `metadata` mort n'empêche pas la lecture.
- **arbitrage** : deux plugins répondant pour la même identité, le premier
  déclaré gagne quel que soit l'ordre d'arrivée des réponses (donc un test qui
  fait répondre le second **en premier**) ; un enrichissement entièrement vide
  compte comme une non-réponse et laisse gagner le suivant.
- **composition** : les quatre cas de repli de `line3` (artiste et titre, titre
  seul, artiste seul, aucun des deux — cette dernière laissant la ligne
  **inchangée**), en fonction pure pour être testée sans routeur ni socket.
- **route SSE** : `GET /api/player` répond en `text/event-stream`, émet
  l'état courant **dès la connexion** (même propriété que le flux d'OUI FM, pour
  qu'un onglet ouvert en cours de morceau ne reste pas vide), puis un événement
  par changement ; deux clients connectés reçoivent tous les deux ; un client qui
  se déconnecte ne perturbe ni le canal `watch` ni les autres.
- **mpv** : analyse pure d'un `property-change` portant `icy-title`, sur une
  capture réelle ; valeur vide ignorée.
- **ouifm-metas** : analyse pure d'une ligne `data: {…}` réelle (capture en
  fixture, comme le fait déjà `directory.rs` du plugin radio) ; identité non
  reconnue → aucun enrichissement. **Aucun test ne touche le réseau.**
- **musicbrainz** : les tests existants déménagent tels quels.

## Sécurité et robustesse

L'`event-stream` d'OUI FM est un point d'entrée **privé et non documenté** : il
peut changer, exiger une authentification ou disparaître sans préavis. D'où
trois règles :

- la récupération de métadonnées ne doit **jamais** bloquer ni retarder la
  lecture — elle vit dans son propre processus, et son échec est silencieux ;
- la reconnexion se fait avec un **recul progressif**, jamais en boucle serrée :
  un appareil qui tourne sans surveillance ne doit pas marteler le serveur d'un
  tiers ;
- rien n'est mis en cache sur disque.

Le websocket de `myradioenligne.fr` a été examiné et **écarté** : c'est
l'infrastructure d'un tiers qui relaie une donnée récupérée ailleurs, sans
engagement envers nous, et la solliciter en permanence depuis un appareil
serait discourtois. Le flux de première main est strictement préférable.

## Écarts assumés à l'implémentation

Quatre décisions prises à l'écriture du code, qui s'écartent de la lettre de
cette spec. Chacune est testée et documentée dans le README.

1. **L'identité voyage dans un enum `IdentityUpdate`, pas un
   `Option<Option<Value>>`.** La spec voulait trois états : « cette trame ne dit
   rien de l'identité », « voici l'identité », « plus rien ne joue ». Serde
   ramène `null` et l'absence d'un champ à la **même valeur** pour un `Option`,
   donc les deux premiers états auraient été indistinguables sans un
   `deserialize_with` sur mesure. `Playing` / `Nothing` dit la même chose en se
   lisant sur le fil.

2. **Le plugin cd place la TOC brute dans l'identité**, et la mise au format
   MusicBrainz (`1+N+leadout+offsets`) part avec le code MusicBrainz. La spec
   laissait entendre que le plugin cd fournirait le paramètre déjà formaté, ce
   qui lui aurait fait connaître le format de requête d'un fournisseur
   particulier — exactement ce que l'extraction cherchait à défaire. Un futur
   plugin (Discogs, freedb) lit la même TOC brute.

3. **`line2` reçoit l'album si la Source a déclaré cette ligne remplaçable.** La
   spec ne prévoyait que `line3`. Appliquée seule, elle aurait fait **perdre au
   CD l'album** qu'il affichait avant l'extraction de MusicBrainz, alors que
   l'enrichissement le rapporte.

   Première tentative, corrigée en revue : « remplir `line2` si la Source l'a
   laissée vide ». Elle reposait sur une négociation par l'absence — une Source
   demandait l'album en se taisant, et celle qui voulait une ligne vide (une
   entrée auxiliaire sobre) se serait vu imposer un album sans recours. Elle
   faisait surtout perdre « audio CD » dans des cas **ordinaires** et non dans le
   seul cas annoncé : disque absent de MusicBrainz, requête en échec, TOC
   illisible, appareil hors ligne — l'afficheur montrait alors `CD 3/12` et deux
   lignes vides, ce qui a l'air d'une panne.

   Forme retenue : la Source **déclare** sa `line2` remplaçable
   (`line2_replaceable`). Le plugin cd y écrit « audio CD », l'album prend la
   place quand un plugin le rapporte, et l'étiquette revient dès qu'il ne le sait
   plus — le remplacement est réversible, et le cœur ne détruit jamais une
   information que la Source seule possède.

4. **Le canal vers les plugins `metadata` est un `watch`, et le cœur ne leur
   parle jamais directement.** La spec décrivait « l'envoi de `NowPlaying` aux
   plugins ». Un appel direct depuis la boucle du cœur l'aurait exposé au
   blocage : un plugin vivant mais qui ne lit plus sa socket remplit le tampon,
   et l'écriture ne rend plus la main — l'appareil entier se figerait pour une
   histoire de métadonnées. C'est déjà la raison pour laquelle les vues passent
   par un `watch`.

5. **Deux notifications ajoutées au protocole Source**, absentes de la spec, dont
   la revue a montré qu'elles manquaient :

   - `SourceReq::Stop` — `Command::Stop` était la seule commande à changer l'état
     de lecture **sans traverser la Source**. Sans être prévenue, une Source qui
     tient un état de lecture propre (le cd) le gardait faux et annonçait plus
     tard des métadonnées pour un morceau arrêté. Aucun état local au plugin ne
     pouvait être juste tant que cette notification n'existait pas.
   - `SourceReq::PlayerTrack(n)` — quand un CD passe seul à la piste suivante,
     mpv en informe le cœur, mais celui-ci ne peut pas corriger une identité
     qu'il a pour principe de ne jamais interpréter. Il la fait donc corriger par
     la Source. Sans cela, l'affichage **et** les métadonnées restaient sur la
     piste précédente jusqu'à la prochaine commande de l'utilisateur — le défaut
     le plus visible au quotidien, et il existait déjà avant cette branche pour
     les titres de piste.

   Les deux ont une implémentation par défaut vide côté SDK : aucune Source
   existante ne change.

Deux points de produit tranchés par le propriétaire pendant l'implémentation :

- **on affiche toute information disponible, même partielle** — artiste seul,
  titre seul, album seul ; côté afficheurs comme côté IHM web ;
- **la route SSE n'a pas d'authentification**, comme toutes les autres routes de
  l'appareil : en protéger une seule donnerait l'illusion d'une protection alors
  que `/api/command` pilote déjà la lecture sans rien demander.

### La table des webradios OUI FM

La spec la donnait pour « la pièce fragile », à embarquer en dur. Elle **est**
embarquée, et elle n'est pas fragile pour autant, parce qu'elle est relevée d'une
source de vérité et non devinée : la variable JavaScript `apidata` de
`https://www.ouifm.fr/player` liste les 21 flux, chacun avec son identifiant de
flux (`id`) et son identifiant de métadonnées (`idMds`).
`scripts/fetch-webradios.mjs` la régénère depuis cette source, et rend la
provenance exécutable plutôt que racontée.

Trois choses ont été **mesurées** et ont changé la conception :

- **Ce sont deux identifiants distincts.** `?id=<idMds>` renvoie artiste, titre
  et durée ; `?id=<identifiant de flux>` renvoie une trame dégénérée
  (`{"mdsId":"…"}`), sans le moindre champ utile et sans erreur HTTP. Les
  confondre aurait donné un plugin muet sans aucun signe. Un test refuse
  désormais toute entrée où les deux se ressemblent.
- **`durationInSeconds` est une chaîne** (`"245"`), pas un nombre. L'analyseur ne
  lisait que les nombres : la durée était silencieusement perdue à chaque
  morceau. Les deux formes sont acceptées.
- **La reconnaissance doit porter sur un fragment de l'URL**, pas sur l'URL
  entière : celle qu'OUI FM sert comporte un jeton signé et un paramètre de
  format variables (`?format=hd|sd|hls`).
- **Une même webradio se diffuse sous deux formes d'URL**, et la première version
  n'en connaissait qu'une. `apidata` ne donne que les URL
  `streams.lesindesradios.fr` ; or les URL qu'on rencontre en pratique sont les
  mounts Icecast historiques (`ouifm3.ice.infomaniak.ch/ouifm3.mp3`), publiés de
  longue date, donc référencés par les annuaires et recopiés par les
  utilisateurs. **Aucune station OUI FM ajoutée normalement n'était reconnue** —
  défaut trouvé à l'essai réel, pas en revue. Les quatre mounts nommés ont été
  relevés par leur en-tête `icy-name`, et la correspondance prouvée par
  recoupement : le titre ICY d'`ouifm5` et le flux de métadonnées de Rock Indé
  annonçaient le même morceau au même instant. Seule l'entrée principale reste
  déduite (hôte et mount sans numéro, et seul flux dont l'ICY ne porte qu'un
  texte de remplissage).
- **L'ordre de l'ICY est inversé** sur ces flux : `Titre - ARTISTE`. Le choix de
  la spec — afficher l'ICY brut, sans découpage — s'en trouve confirmé : la
  convention `Artiste - Titre` n'est décidément pas une garantie.

Un dernier défaut, trouvé au même essai : **la garde de péremption de la couche
ICY, ajoutée en revue, la rendait dépendante de la Source.** Refuser un titre
faute d'identité courante privait de titres toute installation dont la Source ne
déclare pas d'identité — un plugin tiers, ou simplement un binaire pas encore mis
à jour — alors que cette couche est précisément celle qui doit fonctionner sans
rien. Et elle se taisait **en silence**. La garde s'appuie désormais sur ce que le
cœur sait de lui-même de la lecture (`expecting_stream`), ce qui protège
exactement autant contre le titre en retard après un arrêt, sans rien exiger de
la Source.

Le fichier de configuration reste consulté **avant** la table embarquée : il
permet de corriger une entrée devenue fausse ou d'en ajouter une sans recompiler.

La liste n'est **pas** relue au démarrage depuis le site, bien qu'elle y soit :
elle ne vit que dans une page HTML (l'endpoint GraphQL du site n'expose aucune
requête connue pour les flux), et une extraction par expression régulière sur une
page qu'un tiers refond quand il veut est trop fragile pour un appareil qui doit
démarrer sans surveillance — son échec serait silencieux, et l'appareil perdrait
les titres sans rien dire. Une table embarquée échoue de façon reproductible, se
diffe, et se corrige par un fichier.

## Hors périmètre

- Pochettes d'album (`coverId` est reçu mais ignoré), historique des morceaux,
  recherche, marquage de favoris.
- Toute API *now playing* par station autre qu'OUI FM : un plugin par
  fournisseur, dans sa propre spec.
- Le DAB+ (qui transporte le titre par DLS) : demande un tuner, hors sujet pour
  un appareil qui lit des URL.
- Découpage heuristique de l'ICY en artiste/titre.
- Le plugin `console` n'est pas retouché : il reçoit la `View` déjà composée.
