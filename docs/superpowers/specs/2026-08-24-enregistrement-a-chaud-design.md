# Enregistrement à chaud des greffons

Date : 2026-08-24
Suite de : `2026-08-24-rendez-vous-greffons-design.md`

## Le problème

Le chantier du rendez-vous a remplacé deux attentes devinées par une annonce.
Mais il a gardé de l'ancien monde une propriété qu'il n'aurait pas dû : une
**échéance qui condamne**.

Aujourd'hui `register::gather` attend au plus 10 s, puis le socket
d'enregistrement cesse d'être lu. Un greffon qui s'annonce à t+12 s est perdu
**jusqu'au prochain redémarrage du service**. Sa source, son afficheur ou sa
télécommande manquent pour toute la session, et rien ne peut le rattraper.

C'était le premier risque identifié par la relecture finale pour un démarrage à
froid sur carte SD : huit binaires Rust démarrant ensemble, chacun montant un
runtime tokio et lisant ses catalogues, pendant que mpv démarre et que le
réseau se dénoue. Les deux candidats à manquer la fenêtre sont ceux qui
travaillent **avant** de s'annoncer : `files` (état, racines, catalogue, puis
`smbclient --version`) et `console` (ouverture de `/dev/tty1`).

Or rien ne justifie cette condamnation. Le cœur possède un socket
d'enregistrement : il peut écouter aussi longtemps qu'il vit.

## La décision

**Le socket d'enregistrement reste ouvert pour toute la vie du processus.**
L'échéance de 10 s change de rôle : elle ne ferme plus la porte, elle sert
uniquement à **ne pas bloquer le démarrage** et à **nommer un greffon figé**.

Trois états distincts remplacent les deux d'aujourd'hui :

| Situation | État rapporté |
|---|---|
| Annoncé et câblé | `connected: true` |
| Processus mort avant de s'annoncer | `connected: false` |
| **Processus vivant, muet à l'échéance** | `connected: false`, **`stalled: true`** |

Un greffon figé n'est pas un greffon mort : il tourne, il n'a rien dit, et il
peut encore parler. C'est cette différence que l'opérateur doit voir, et c'est
elle qui manquait.

## Architecture

### Le socket ne se ferme plus

`gather` garde exactement son rôle actuel — débloquer le démarrage — mais
n'est plus le seul lecteur du socket. Après son retour, `main` lance une tâche
qui continue d'accepter sur `register.sock` pour toute la vie du processus, et
pousse chaque annonce dans un `mpsc<Announcement>`.

**Ce canal est unique aux deux étages** : `main` le crée avant le rendez-vous,
`gather` l'emprunte, la tâche permanente en garde l'émetteur, et la boucle de
sélection consomme le reste. C'est ce qui rend une annonce inperdable. Avec un
canal propre au rassemblement, détruit à son retour, deux annonces s'évaporaient
en silence : celle qui était prête à l'instant de l'échéance — `tokio::select!`
tire au hasard entre deux bras prêts, une chance sur deux — et celle d'une
connexion acceptée dont la tâche de lecture n'aboutissait qu'après le retour. Le
SDK n'annonçant qu'**une seule fois**, le greffon se croyait enregistré et
attendait le prochain redémarrage du service. Avec un seul canal, le tirage ne
décide plus que du chemin : ce que `gather` ne consomme pas reste en file, et le
câblage à chaud le reprend un instant plus tard.

Cette tâche reprend la **tâche de lecture par connexion** de `gather` : une
connexion muette ne doit pas plus bloquer les annonces tardives qu'elle ne
bloquait les initiales. La lecture est **bornée dans le temps** (quelques
secondes), ce que l'échéance faisait pour le rendez-vous et que rien ne fait
pour une boucle sans échéance : sinon chaque connexion muette immobilise une
tâche et un descripteur pour la vie du processus, et un greffon qui reconnecte en
boucle sans écrire finit par rendre tout câblage impossible.

La boucle de sélection principale gagne une branche :

```
Some(annonce) = tardives_rx.recv() => { cabler_a_chaud(annonce).await }
```

### Le câblage à chaud, genre par genre

Chaque genre a déjà, dans le câblage de démarrage, une forme qui se rejoue
telle quelle :

- **`input`** — `tokio::spawn(run_input_client(&socket, cmd_tx.clone()))`.
- **`display`** — `DisplayClient::connect` puis une tâche de relais avec son
  propre `etat_rx.clone()`. Une tâche par afficheur, comme au démarrage :
  c'est ce qui empêche un afficheur lent de retarder les autres. Le relais
  envoie l'**état courant d'abord**, avant toute attente de changement : un
  afficheur câblé à chaud montre ce qui joue tout de suite, y compris en veille
  où aucun tick n'est armé et où `publie_etat`, dédupliqué, ne réparerait rien.
- **`metadata`** — recalcul de l'ordre d'arbitrage et remplacement dans le cœur
  **d'abord**, `tokio::spawn(run_metadata_client(...))` ensuite. Jamais
  l'inverse : le client peut envoyer un enrichissement dès sa première trame, et
  le cœur rejette celui d'un greffon `metadata` non déclaré. L'ordre posé avant
  le lancement, la correction ne dépend plus de ce que la boucle principale peut
  ou ne peut pas drainer pendant ce câblage.
- **`source`** — `SourceClient::connect`, `Core::add_source`, puis la **langue
  courante** poussée à cette seule source. `resume` et `set_locale` ne servent
  que les sources présentes dans la table au moment de leur appel : sans cet
  envoi, une source arrivée après reste dans la langue de son lancement, et un
  `cd` relancé à la main sur un appareil en français revient en affichant
  `NO DISC`.
- **`admin`** — l'ancien dorsal **retiré d'abord**, `AdminClient::connect`
  ensuite, puis insertion dans la table des pages. Retirer après, ou seulement
  en cas de succès, laisse un dorsal qui pointe vers un socket disparu :
  `/api/admin/<nom>` répondrait une erreur au bout des 5 s du protocole d'admin
  — sériel, donc en retenant la page — là où un 404 franc dit tout de suite
  qu'il n'y a rien à cette adresse.

### L'ordre d'arbitrage des métadonnées, le point délicat

C'est l'invariant le plus facile à casser ici. L'ordre de priorité est celui de
`plugins.toml`, **jamais** celui d'arrivée des annonces — un greffon
`metadata` qui arrive en retard doit prendre sa place dans le fichier, pas la
dernière.

La règle : ne jamais ajouter en queue. `main` recalcule la liste **complète**
par `register::metadata_order(&ordre_manifeste, &rassemble)` — la fonction déjà
en place et déjà testée — et la remet au cœur par un `Core::set_metadata_order`
qui remplace la liste de `Metadonnees`. La logique d'ordre reste ainsi en un
seul endroit.

### `Core::add_source`

`Core` tient `sources: HashMap<String, Arc<dyn Source>>` et
`source_order: Vec<String>` trié. Ajouter à chaud :

```rust
/// Ajoute une source découverte après le démarrage.
///
/// `source_order` est **retrié** : le cycle de sources suit l'ordre
/// alphabétique, et une source arrivée en retard doit y prendre sa place
/// normale, pas la queue — sinon `SourceCycle` change de sens selon la
/// chronologie du démarrage.
///
/// Si aucune source n'était active — un démarrage où *aucune* n'avait
/// répondu — la nouvelle le devient : c'est le seul cas où l'arrivée d'un
/// greffon change ce qui joue.
pub fn add_source(&mut self, name: String, client: Arc<dyn Source>) -> bool {
    let premiere = self.sources.is_empty();
    let remplacement = self.sources.insert(name.clone(), client).is_some();
    if !self.source_order.contains(&name) {
        self.source_order.push(name.clone());
        self.source_order.sort();
    }
    if premiere {
        self.active_source = name;
    }
    remplacement
}
```

`add_source` **ne réveille rien** : elle n'affecte que la table et le nom de
l'active. Ce n'est donc pas ce que `main` appelle. Le câblage à chaud passe par
`Core::cable_source_a_chaud`, qui enchaîne les deux gestes que `add_source`
laisse en suspens :

- **Première source du cœur** (la table était vide) : le démarrage est **rejoué**
  par `resume`, donc `SetLocale` puis `Wake`, dans cet ordre. Sans cela, une
  source arrivée à t+30 s serait active et **muette** jusqu'à ce que
  l'utilisateur touche quelque chose : l'appareil aurait l'air en panne alors que
  tout est câblé.
- **Source supplémentaire, ou cœur en veille** : seule la langue courante est
  poussée. Réveiller ici rallumerait un appareil volontairement éteint — la
  veille est un état voulu — et changerait ce qui joue parce qu'un greffon a fini
  de démarrer.

L'état est publié dans les deux cas : le nom de la source vient d'apparaître dans
la trame, et l'IHM comme les afficheurs annonçaient jusque-là « aucune source ».

### Démarrer sans aucune source

`main` refusait de démarrer quand aucune source n'avait répondu à l'échéance.
C'était la dernière échéance qui condamne, et elle contredit tout le reste :
refuser à t+10 s nie qu'une source puisse arriver à t+30 s, systemd reboucle sans
rien réparer, et le refus supprime la page de statut **précisément** quand on
voudrait y voir le greffon figé. Il n'y aura rien à lire, mais on peut déjà voir
ce qui se passe.

Il reste **un** refus, qui n'est pas une lenteur mais une erreur de
configuration : plus aucun processus de greffon vivant — `plugins.toml` vide,
exécutables introuvables, ou tous morts avant l'échéance. Là, personne ne
s'annoncera jamais (`register::un_greffon_vivant`). Sans source mais avec au
moins un processus vivant, le cœur démarre et le journalise en `warn`.

Le vrai travail n'était pas la ligne de refus mais `Core::active()`, qui
paniquait sur une table vide : lever le refus sans elle aurait échangé un message
d'erreur propre contre un `panic!` au démarrage, avant même la première page
servie. Elle devient `demande_active(req) -> Result<Option<SourceAction>>`, et
ses treize appelants se répartissent en deux formes : ceux qui **pilotent l'état**
(`if let Some(action) = …? { self.apply(action).await? }`) et ceux qui sont au
mieux-effort (`let _ =`, `if let Err(e) =`). Aucun ne panique, aucun n'échoue :
sans source, une commande de télécommande ne fait rien, et le dit en `debug` — ce
n'est pas une anomalie.

### Dire « aucune source » à l'écran

Le protocole ne change pas : `source: ""` **est** l'absence, et c'est au rendu de
la nommer. L'IHM affiche la clé `no_source` (en/fr, parité vérifiée par un test
Rust) plutôt qu'un vide qu'on prend pour une panne d'affichage — en distinguant
le vide du protocole du « l'état n'est pas encore arrivé » d'avant la première
trame SSE. Le greffon `console`, qui ne traduit rien (tout ce qu'il écrit lui
arrive déjà traduit du cœur), écrit un tiret : sans lui ses trois lignes étaient
vides, indistinguables d'un afficheur mort.

### Ré-annonce d'un greffon déjà câblé

Un greffon relancé à la main, hors du cœur, se réannonce. Le traitement est le
même que pour une annonce tardive : on recâble. `sources.insert` remplace le
client, et les tâches de relais précédentes sortent d'elles-mêmes : un relais
d'afficheur **quitte sa boucle au premier échec d'envoi**, après l'avoir
journalisé une fois en nommant le greffon. C'est ce qui rend la ré-annonce
légitime. Un relais qui journaliserait sans sortir survivrait à son socket mort
— l'erreur y est permanente (EPIPE) — et écrirait une ligne par trame d'état,
donc une par seconde en lecture et par relais zombie : deux relances à la main
suffiraient à écraser le tampon de 500 lignes de la popin d'erreurs en moins de
quatre minutes, et à y noyer le vrai diagnostic.

Seule précaution, mais elle est impérative : la mise à jour des statuts doit
**remplacer** toutes les lignes du nom, jamais y ajouter — sinon un greffon
qui se réannonce accumulerait des lignes dans la page de statut. Remplacer par
une liste **vide** le laisse visible en genre inconnu : une annonce à
`kinds: []` vient d'un binaire mal compilé, et le faire disparaître de la page
juste après qu'il a parlé serait l'inverse de ce que ce chantier donne à voir.

### `admin_backends` doit devenir mutable

Aujourd'hui `Arc<HashMap<String, Arc<dyn AdminBackend>>>`, partagé en lecture
seule avec l'état du routeur web. Il devient
`Arc<RwLock<HashMap<...>>>` : deux lectures à adapter dans `admin.rs`, la
définition dans `status.rs`, et quatre fixtures de test.

C'est le seul changement de type de ce chantier, et il est mécanique.

## Ce que cela change pour l'opérateur

Un greffon qui rate la fenêtre n'est plus perdu : il apparaît **figé** dans la
page de statut, et s'il finit par s'annoncer, il est câblé sans redémarrage.
Un greffon relancé à la main revient tout seul.

Le dimensionnement des 10 s cesse d'être un pari : la valeur ne décide plus de
ce qui marche, seulement du moment où l'on cesse d'attendre pour démarrer.

## Gestion d'erreur

| Situation | Comportement |
|---|---|
| Annonce tardive d'un nom inconnu du manifeste | Avertissement nommant le nom, annonce ignorée (identique au démarrage) |
| Annonce tardive illisible | Avertissement, ignorée |
| Un genre annoncé à chaud dont le `connect` échoue | Ce genre reste indisponible, les autres genres du même greffon sont câblés ; ligne de statut `connected: false` |
| Ré-annonce d'un greffon déjà câblé | Recâblage, journalisé, lignes de statut **remplacées** |
| Ré-annonce d'un greffon rapporté **mort** | Recâblage, **avertissement** : sa `child.wait()` a été consommée par le rendez-vous, `plugin_waits` ne reverra jamais sa sortie, et son `connected: true` ne se démentira plus tout seul |
| Greffon figé qui meurt plus tard | `plugin_waits` le voit, `mark_plugin_disconnected` bascule ses lignes |

## Tests

- `PluginStatus.stalled` : sérialisation, et absent du JSON quand faux.
- À l'échéance, un greffon **vivant** non annoncé est marqué `stalled`, un
  greffon **mort** ne l'est pas.
- Une annonce arrivée **après** le retour de `gather` atteint la boucle
  principale.
- Une annonce déposée à l'instant **exact** de l'échéance n'est jamais perdue :
  ou `gather` la consomme, ou elle reste en file pour le câblage à chaud. Sous
  horloge simulée, pour que la course se produise vraiment — avec l'horloge
  réelle, le rendez-vous gagne toujours et le test ne prouverait rien.
- Une connexion muette est **lâchée** au bout du délai de lecture, et son
  descripteur rendu.
- Une source câblée à chaud reçoit la **langue courante**, et elle seule.
- Une annonce à `kinds: []` laisse le greffon **visible** en genre inconnu.
- Une annonce tardive de genre `metadata` prend sa place **du manifeste** dans
  l'ordre d'arbitrage, et non la dernière : c'est le test qui protège
  l'invariant.
- `Core::add_source` : la source arrive, `source_order` reste trié, et devient
  active si et seulement si aucune source n'existait.
- Une ré-annonce **remplace** les lignes de statut du greffon au lieu d'en
  ajouter.
- Une annonce à `kinds: []` **et** `admin: true` garde son drapeau `admin` : la
  ligne de repli vient de `genre_inconnu`, dont le drapeau est faux par
  construction, et sans lui le dorsal était câblé sans rien pour y mener.
- `resume` sur un cœur **sans aucune source** publie l'état au lieu de paniquer,
  et chacune des commandes de la télécommande ne fait rien sans échouer.
- Une **première** source câblée à chaud reçoit `SetLocale` **puis** `Wake`, dans
  cet ordre, et le `Play` renvoyé est appliqué.
- La même source câblée à chaud pendant la **veille** ne reçoit que la langue :
  rien ne se met à jouer.
- `un_greffon_vivant` : aucun lancé, tous morts, et un figé parmi des morts.
- Parité en/fr de la nouvelle clé i18n (test Rust déjà en place dans le dépôt).

## Hors périmètre

Rien n'est tenté sur la **relance** d'un greffon mort : le cœur ne redémarre
pas ses enfants, et ce chantier ne change pas cela. Il rend seulement possible
qu'un greffon relancé par ailleurs — à la main, ou par un futur mécanisme de
supervision — soit repris sans redémarrer le cœur.
