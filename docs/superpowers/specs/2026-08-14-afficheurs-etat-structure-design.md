# Afficheurs libres : état structuré au lieu de trois lignes composées

**Date :** 2026-08-14
**État :** validé par le propriétaire, prêt pour le plan d'implémentation

## Le problème

Un plugin d'affichage reçoit aujourd'hui **une seule chose** : trois chaînes
déjà composées (`View { line1, line2, line3 }`), par une méthode unique
`DisplayPlugin::show(view)`, à sens unique.

La contrainte n'est pas le nombre de lignes, c'est que **la composition vit
dans le cœur** :

- c'est lui qui écrit « RADIO  P4 » et « CD 1/3 » (via les sources) ;
- c'est lui qui injecte l'album dans `line2` quand un plugin `metadata` le
  connaît (tout le sens de `line2_replaceable`) ;
- c'est lui qui **écrase les trois mêmes lignes** pour ses incrustations :
  « VOLUME 65 % », le `+NN`, les messages éphémères.

Conséquences : un afficheur qui reçoit « VOLUME 65 % » ne peut pas savoir que
c'est une incrustation passagère plutôt que ce qui joue ; un afficheur qui
reçoit « FIP » ne peut pas savoir si c'est un nom de station ou un album qui
l'a remplacé. Aucune déclaration de capacités n'existe : le cœur compose pour
un écran texte d'environ vingt colonnes, et rien ne permet d'annoncer autre
chose.

Un grand afficheur serait donc réduit à peindre trois chaînes courtes
calibrées pour un LCD.

L'asymétrie est documentée dans `core.rs` (« la SPA reçoit du structuré, les
afficheurs reçoivent des lignes déjà composées, chacun son chemin ») — c'est
elle qu'on lève ici.

**Pourquoi maintenant :** un seul plugin d'affichage existe (`console`), deux
sources (`radio`, `cd`), et le projet n'est pas publié. Le même changement
après publication casserait des plugins tiers.

## Décisions

Cinq choix tranchés en conception, à ne pas re-débattre en implémentation :

1. **La mise en page sort du cœur, l'arbitrage y reste.** `Metadonnees` et son
   arbitrage ICY / plugins `metadata` ne bougent pas. Seule la composition
   part.
2. **Donnée *et* texte déjà résolu, jamais de mise en page.** Chaque
   information voyage sous ses deux formes utiles : la valeur brute (pour un
   afficheur qui veut dessiner une jauge de volume) et le mot déjà traduit
   (pour qu'aucun afficheur n'ait besoin d'un catalogue).
3. **Le texte arrive par morceaux, pas en trois lignes.** Aucun champ ne porte
   une disposition. Garder les trois lignes composées « pour le cas simple »
   serait un faux cadeau : elles encodent une largeur d'écran, et laisseraient
   le cœur propriétaire d'une décision de mise en page pour toujours.
4. **Le même principe s'applique aux sources.** Elles cessent aussi de mettre
   en page : `view` et `line2_replaceable` disparaissent du protocole, un
   `status` résolu par leur propre catalogue les remplace.
5. **Une seule charge utile pour la SPA et les afficheurs.** Une structure à
   documenter et tester, deux transports.

Le cœur reste seul maître des **échéances** d'incrustation ; la trame annonce
le temps restant à titre indicatif, pour qu'un afficheur puisse animer sans
jamais décider de la fin.

### Nommage

Les clés JSON sont **en anglais** (`status`, `overlay`, `remaining_ms`), par
cohérence avec la charge utile existante que la SPA consomme déjà
(`preset_name`, `preset_count`, `duration_s`). Les commentaires restent en
français, les doc `///` suivent le fichier où elles vivent.

## La charge utile

`PlayerState` garde son nom : il sert déjà les deux publics, et le renommer
ferait du bruit pour rien. Il gagne deux champs.

**Il change en revanche de crate.** Il vit aujourd'hui dans
`crates/ritornello-core/src/metadata.rs`, or le SDK devra le **désérialiser**
pour le passer aux plugins d'affichage — et le SDK ne peut pas dépendre du cœur
sans créer un cycle. `PlayerState` et `Morceau` déménagent donc dans
`crates/ritornello-proto/src/metadata.rs`, aux côtés d'`Enrichment` et
d'`IdentityUpdate` qui y sont déjà. Un réexport depuis le cœur garde valides les
`use crate::metadata::PlayerState` existants. C'est un déplacement pur, sans
changement de comportement, et il gagne `Deserialize` au passage (le cœur ne
faisait que sérialiser).

### `status: Option<String>`

La phrase d'état du moment, **déjà traduite**. Elle vient de la source
(« PAS DE DISQUE », « audio CD », « présélection vide ») ou du cœur pour la
veille. Un seul emplacement : il n'y a jamais deux statuts à la fois.

Ce champ absorbe ce que les remplissages de `line2` faisaient, mais comme une
**affirmation** et non comme un emplacement à écraser. Le « audio CD » du
plugin cd devient un statut permanent (« un disque joue, je n'en sais pas
plus ») ; l'afficheur choisit entre lui et l'album.

**Convention, différente de celle de `preset` et à ne pas uniformiser par
réflexe :** dans une trame de source, `status` **absent signifie « aucun
statut »**, et non « garder le précédent ». C'est ce qui reproduit exactement le
comportement actuel — une source recompose sa vue entière à chaque trame, donc
le cd redéclare « audio CD » chaque fois — et c'est la seule convention qui
permette d'effacer un statut : avec « absent = garder », « PAS DE DISQUE »
resterait affiché après l'insertion d'un disque, sans aucune façon de
l'annuler. `preset` peut se permettre l'autre convention parce que le cœur
l'efface de lui-même quand plus rien ne joue.

**Une trame éphémère (`transient: true`) ne touche pas au statut mémorisé :**
son `status` alimente l'incrustation `Message`, et le statut permanent reparaît
à l'échéance. C'est le comportement actuel, où le message éphémère occupe
l'emplacement d'incrustation et laisse la vue permanente intacte.

**En veille, le statut du cœur gagne** sur celui de la source : l'appareil dort,
ce que raconte la source n'a plus cours.

### `overlay: Option<Overlay>`

Un **enum étiqueté**, et non une grappe d'optionnels — pour qu'aucune
combinaison impossible n'existe (un niveau de volume sur un message éphémère) :

```rust
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Overlay {
    /// Incrustation volume/muet. `level` permet la jauge, `text` le repli.
    Volume { level: u8, muted: bool, text: String, remaining_ms: u32 },
    /// Décalage `+NN` en cours de saisie.
    Tens { offset: u8, text: String, remaining_ms: u32 },
    /// Message éphémère d'une source (« présélection vide »).
    Message { text: String, remaining_ms: u32 },
}
```

Étiquetage **interne** (`tag = "kind"`) et non adjacent comme `SourceAction` :
les variantes portent des champs nommés, et un objet plat se lit mieux côté
web que `{"kind":…,"data":{…}}`.

### Le piège de `remaining_ms`

C'est **`Overlay`** qui reçoit un `PartialEq` écrit à la main, excluant
`remaining_ms` — et non `PlayerState`, qui continue de dériver le sien. Écrire
l'égalité au niveau de `PlayerState` obligerait à comparer à la main tous ses
autres champs pour ne traiter spécialement qu'un champ imbriqué dans un enum
sous une `Option` : chaque champ ajouté plus tard serait un oubli en puissance.

Le commentaire dit pourquoi : deux incrustations ne différant que par leur
temps restant décrivent le même écran.

Sans cela, la garde `send_if_modified` cesserait d'avaler les rafraîchissements
redondants — plusieurs chemins peuvent rafraîchir l'affichage pour un même
événement — et chaque afficheur réimprimerait la même chose. Un test verrouille
cette égalité.

Le reste de `PlayerState` ne change pas : `source`, `preset`, `preset_name`,
`preset_count`, `volume`, `muted`, `standby`, et le `morceau` aplati avec son
`origin`.

## Protocole des sources

`SourceMessage` (`crates/ritornello-proto/src/source.rs`) :

**Perd** `view: Option<View>` et `line2_replaceable: bool`.
**Gagne** `status: Option<String>` — le mot que la source résout elle-même.
**Garde** `id`, `action`, `identity`, `transient`, `preset`, `preset_name`,
`preset_count`.

`transient` garde son sens, appliqué au `status` : c'est le cas « présélection
vide », un mot éphémère alors que la station précédente continue de jouer.

`line2_replaceable` **meurt de sa mort naturelle** : il n'existait que pour
négocier la permission d'écrire dans une ligne. Sans ligne, la question ne se
pose plus — `status` et `morceau` arrivent côte à côte, et l'afficheur décide
lequel montrer.

**`View` disparaît du protocole**, donc `crates/ritornello-proto/src/view.rs`
est supprimé et retiré de `lib.rs`. Plus personne ne l'échange.

« RADIO  P4 » et « CD 1/3 » n'ont plus besoin d'exister : `source` + `preset` +
`preset_count` les redonnent, et l'afficheur décide de les écrire ainsi. Le
plugin cd **déclare déjà** `preset` (numéro de piste) et `preset_count`
(total) : aucune donnée nouvelle n'est à inventer de ce côté.

`SourceOutcome` (SDK, `server.rs`) et `SourceUpdate` (SDK, `client.rs`) suivent
les mêmes champs, ainsi que le chemin des **notifications spontanées** —
`Notification` reçoit au passage `preset_name`, qui lui manquait.

**Attention**, comme pour `preset_name` : la condition qui décide qu'une trame
porte quelque chose (`client.rs`, vers la ligne 86) doit inclure `status`,
sinon une trame ne portant qu'un statut serait silencieusement jetée.

## Protocole d'affichage

`DisplayPlugin::show(view: View)` devient `show(state: PlayerState)`. Toujours
à sens unique, aucune réponse attendue.

`SetLocale` n'est **pas** ajouté : tout ce qu'un afficheur a à écrire lui
arrive déjà traduit. C'est un ajout **non cassant** le jour où un afficheur
voudra ses propres mots (un message de plus, qu'un plugin peut ignorer), donc
YAGNI s'applique — contrairement au reste du chantier, où l'argument « avant
publication » vaut justement parce que ce serait cassant ensuite.

## Le cœur

**Part :**

- `metadata::composer` et `ligne_titre` — vers le plugin console, avec leurs
  tests des quatre replis ;
- `standby_view()` : la veille devient un `status` résolu, plus une `View` ;
- le **second canal** : `view_tx` disparaît de `Cablage` et de `main.rs`, le
  commentaire « chacun son chemin » avec lui. Son unique consommateur était la
  boucle de relais vers les afficheurs (`main.rs` vers la ligne 280), qui lit
  désormais le même `watch<PlayerState>` que le flux SSE.

**Reste, intact :** `Metadonnees` et son arbitrage, les échéances
d'incrustation, et `expire_overlay` qui efface l'incrustation **et** le
décalage ensemble.

**Change de type :** `overlay: Option<(View, Instant)>` devient
`Option<(Overlay, Instant)>`. Le cœur résout ses propres mots (libellé de
volume, texte `+NN`, veille) depuis son catalogue, comme aujourd'hui.

**Gain structurel :** il existe aujourd'hui **deux** chemins de publication aux
déclenchements différents, `push_view` pour les afficheurs et `publie_etat`
pour la SPA. C'est de cette dualité qu'était né un défaut corrigé le même jour
(une trame éphémère écrasait l'incrustation tout en laissant le décalage armé).
Il n'en reste qu'un, avec une seule garde de déduplication : une classe de
bugs disparaît.

## Le plugin console

Gagne une **fonction pure de mise en page** — reçoit la trame, rend ses trois
lignes — qui porte :

- les quatre replis artiste/titre (`ligne_titre`, déplacé avec ses tests) ;
- la reconstitution de « RADIO  P4 » depuis `source` et `preset` ;
- le choix entre `status` et album quand les deux existent ;
- la veille et les incrustations.

`render_console` la consomme. Testable seule, comme elle l'est déjà.

## Le web

`PlayerPayload` (`web/app/src/types.ts`) gagne `status` et `overlay`.

La carte Lecteur **affiche le `status`** quand il est présent. C'est le
corollaire du besoin d'origine : « PAS DE DISQUE » est invisible sur le web
aujourd'hui, pour exactement la même raison que le nom de station l'était.

La SPA **ignore** `overlay` : elle affiche déjà le volume en clair et a ses
propres toasts. Le champ voyage parce que la charge utile est unique ; rien
n'oblige à le montrer.

## Tests

Le déplacement des tests est le contrôle de cohérence du chantier : **les tests
d'arbitrage (`Metadonnees`) ne bougent pas d'une ligne.** S'ils doivent être
touchés, c'est que le changement a dérivé hors de son périmètre.

- **Plugin console** : les quatre replis artiste/titre arrivent avec leur
  fonction, et s'enrichissent des cas neufs — `status` contre album quand les
  deux existent, « RADIO  P4 » reconstitué, veille, chaque genre
  d'incrustation.
- **Cœur** : le `PartialEq` qui ignore `remaining_ms` (deux trames n'en
  différant que par lui se comparent **égales**, donc la garde les avale) ; le
  cycle de l'incrustation transposé de `View` vers `Overlay`, échéance qui
  efface incrustation **et** décalage ; la publication unique.
- **Protocole** : aller-retour JSON du `status` et des trois variantes
  d'`Overlay`. L'absence de `view` et de `line2_replaceable` est vérifiée par
  la compilation, pas par un test.
- **Sources** : les tests de vue de `radio` et `cd` deviennent des tests de
  statut — « PAS DE DISQUE », « audio CD », « présélection vide ».
- **SDK** : une trame ne portant qu'un `status` est bien relayée (le piège de
  la condition de trame non vide).
- **Web** : la carte affiche le statut quand il est là, rien quand il manque.

## Migration

**Aucune compatibilité, et c'est délibéré.** Pas de champ de version, pas de
double chemin, pas de `#[serde(default)]` « au cas où » sur ce qui disparaît.
Le projet n'est pas publié, il y a un afficheur et deux sources, tous dans ce
dépôt. Un plugin non mis à jour ne compile plus — c'est le signal qu'on veut,
plutôt qu'un plugin qui n'afficherait silencieusement plus rien.

Les nouveaux champs optionnels gardent `#[serde(default, skip_serializing_if)]`
comme leurs voisins : c'est le style du fichier, pas une concession à la
compatibilité.

## Documentation

Réécriture des sections concernées de `docs/plugins.md` (protocoles source et
affichage) et `docs/interface.md` (charge utile de `/api/player`).

## Hors périmètre

Noté ici pour que ce ne soit pas redécouvert comme un oubli :

- `SetLocale` pour les afficheurs — ajout non cassant, à faire quand un
  afficheur voudra ses propres mots ;
- négociation de géométrie ou de capacités — sans objet dès que la mise en page
  appartient au plugin ;
- animation des incrustations — la trame la rend possible, personne ne la fait ;
- afficheur riche de démonstration — le plugin console reste le seul.
