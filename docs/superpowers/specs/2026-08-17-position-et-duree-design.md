# Où on en est dans la piste : durée, position, et deux touches pour s'y déplacer

**Date :** 2026-08-17
**État :** validé par le propriétaire, prêt pour le plan d'implémentation

## Le problème

L'appareil sait souvent combien dure ce qu'il joue, et où on en est — mais
presque rien de cela n'arrive à l'écran, et aucune touche ne permet de se
déplacer dans une piste.

L'état des lieux, mesuré dans le code plutôt que supposé :

- **La durée existe déjà** dans le protocole : `Enrichment.duration_s` →
  `Morceau.duration_s` → `PlayerState`, et la carte Player de la SPA
  l'affiche en petit à côté du bloc « En écoute ». Mais un seul plugin la
  remplit, `radiofrance-metas`, qui la calcule depuis les `startTime` /
  `endTime` du morceau annoncé. mpv, qui la connaît pour tout fichier et
  toute piste de disque, ne la remonte pas — `player/mpv.rs` écrit
  littéralement `duration_s: None`. Le plugin `files` lit des durées dans les
  `#EXTINF` de ses m3u et ne les transmet à personne.
- **La position n'existe nulle part.** Aucun champ du protocole ne la porte,
  le cœur ne sonde ni n'observe `time-pos`, et aucun afficheur ne peut donc
  en montrer une.
- **Aucune commande de déplacement.** `Command` compte douze variantes, pas
  une pour avancer ou reculer dans ce qui joue.

Deux conséquences concrètes : on ne sait pas, devant une piste de disque, s'il
reste dix secondes ou six minutes ; et on ne peut pas repasser les vingt
secondes qu'on vient de manquer.

**Pourquoi maintenant :** le champ `duration_s` traîne à moitié câblé depuis
le chantier métadonnées, et les deux sources de contenu fini (`cd`, `files`)
sont désormais en service — c'est-à-dire qu'il y a enfin quelque chose dans
quoi se déplacer.

## Décisions

Sept choix tranchés en conception, à ne pas re-débattre en implémentation.

1. **Deux fournisseurs de position, jamais en concurrence.** mpv pour un
   contenu fini, un plugin `metadata` pour un flux. Le cœur n'arbitre pas
   entre eux : le contexte décide lequel des deux a le droit de parler.
2. **Le `time-pos` d'un flux est écarté, pas affiché.** Il compte depuis le
   début de la connexion et n'a aucun rapport avec le morceau en cours.
3. **La durée mesurée par mpv l'emporte sur la durée annoncée.** Le disque
   réel prime sur ce que MusicBrainz en dit.
4. **`seekable` est un champ à part entière, pas une déduction.** Une durée
   connue ne veut pas dire un contenu parcourable : Radio France annonce la
   durée d'un morceau sur un direct qu'on ne peut pas rembobiner.
5. **Une trame par seconde pendant la lecture**, sans suspension. La charge
   utile unique reste le seul canal, et chaque afficheur y trouve la position
   *et* l'incrustation en cours — c'est lui qui sait s'il la met par-dessus,
   à côté, ou l'ignore.
6. **Le pas de déplacement vit dans le cœur**, comme les 5 % du volume : les
   touches ne portent aucune quantité.
7. **Rien de nouveau dans le protocole `source`.** mpv couvre tout contenu
   fini, les plugins `metadata` couvrent les flux ; aucune source n'a besoin
   d'un champ pour déclarer une durée ou une position.

## Ce qui voyage

### `PlayerState` — deux champs de plus

```rust
/// Où en est ce qui joue, en secondes, **à l'instant de la publication**.
/// `None` = inconnue (aucun des deux fournisseurs n'a de quoi répondre :
/// un flux sans plugin metadata qui le suive, un arrêt, la veille).
pub position_s: Option<u32>,

/// Ce qui joue accepte un déplacement. C'est le `finite` que la source a
/// déclaré à son `Play`, rendu visible aux consommateurs.
pub seekable: bool,
```

Les deux sont `#[serde(default, skip_serializing_if = …)]` : une trame
produite avant ce chantier reste lisible, et une trame qui n'a rien à en dire
reste identique à l'octet près sur le fil. Même convention additive que
`held` sur `InputMessage`.

`seekable` mérite d'être un champ plutôt qu'une déduction de
`duration_s != null` : c'est lui qui décide si la barre de la SPA est
cliquable, et les deux notions divergent exactement là où ça compte — Radio
France annonce une durée sur un contenu non parcourable, un fichier sans tag
de durée reste parcourable.

### `duration_s` ne bouge pas de place

Le champ reste dans `Morceau`, donc aplati au premier niveau du JSON : **la
forme de la charge utile ne change pas**. Ce qui change est qu'il gagne une
seconde source, mpv, et une précédence documentée :

> Quand mpv connaît la durée de ce qu'il joue, c'est elle qui est publiée ;
> celle qu'un plugin `metadata` a annoncée ne sert que faute de mieux.

Les deux ne coexistent en pratique que sur le CD (mpv mesure la piste,
MusicBrainz la connaît aussi). `origin` continue de désigner qui a fourni
**le morceau** — artiste, titre, album — et non qui a fourni la durée : c'est
une imprécision assumée plutôt qu'un second champ d'origine pour une seule
valeur numérique.

### `Enrichment` — un champ de plus

```rust
/// Écoulé dans le morceau **au moment de l'émission**, en secondes.
/// Le cœur l'ancre à la réception et l'avance lui-même ensuite.
pub position_s: Option<u32>,
```

Un écoulé relatif plutôt qu'un horodatage absolu : rien à synchroniser entre
deux horloges, et c'est la même convention que `duration_s` juste à côté.
`radiofrance-metas` le remplit — il connaît déjà `startTime` — les autres
plugins `metadata` le laissent à `None` sans avoir à changer.

L'écho d'identité qui protège déjà les enrichissements périmés protège la
position sans rien de plus : un enrichissement dont l'identité ne correspond
plus à ce qui joue est jeté, ancre comprise.

### `Command` — trois variantes de plus

```rust
/// Avancer d'un pas dans ce qui joue. Le pas vit dans le cœur (réglage
/// `seek_step_s`), exactement comme les 5 % du volume : la touche ne porte
/// aucune quantité, donc changer le pas ne demande pas de reprogrammer
/// une télécommande.
SeekForward,
SeekBackward,
/// Positionnement absolu, en secondes. Sert la barre cliquable de la SPA ;
/// aucune touche physique ne l'émet.
SeekTo(u32),
```

Les trois sont **ignorées en silence** quand ce qui joue n'est pas
déplaçable. Pas de message, pas de trame : une touche sans effet sur le
contenu courant se comporte comme une touche non liée, ce que la
télécommande sait déjà faire.

## D'où vient la position

### mpv, pour un contenu fini

Le cœur **sonde** `time-pos` et `duration` une fois par seconde, plutôt que
de les `observe_property`. L'observation émettrait plusieurs événements par
seconde — mpv ne les cadence pas — pour une information qu'on ne publie
qu'une fois par seconde de toute façon. Le sondage donne exactement le rythme
voulu, sur le chemin requête/réponse déjà en place (`MpvIpc::command`).

La garde est le `finite` de la dernière action `Play`, que le cœur mémorise
déjà sous la forme `expecting_stream`. Sur un flux, `time-pos` compte depuis
le début de la connexion : il est lu et jeté, jamais publié.

**À mesurer sur le Pi avant de figer le code :** un `cdda://` ouvert en
disque entier expose ses pistes comme des *chapitres* (`player/mpv.rs` le
documente déjà, à propos de l'avance de piste). `time-pos` est-il alors
relatif au disque ou à la piste ? S'il est relatif au disque, la position
publiée doit retrancher le début du chapitre courant, et la durée être celle
du chapitre et non du disque. La mesure décide ; le plan d'implémentation
doit la programmer avant l'étape qui en dépend, comme le montage SMB l'a été
au chantier précédent.

### Un plugin `metadata`, pour un flux

`Enrichment.position_s` arrive avec l'enrichissement ; le cœur retient le
couple *(position, instant de réception)* et publie, à chaque tick,
`position + écoulé depuis l'ancre`. Un nouvel enrichissement ré-ancre. Un
changement d'identité efface l'ancre — sinon la position du morceau
précédent continuerait d'avancer sous le titre du suivant.

`radiofrance-metas` interroge le direct à son propre rythme
(`delayToRefresh`, souvent plusieurs dizaines de secondes) : entre deux
interrogations, c'est bien l'avance du cœur qui fait bouger la barre.

Borne : si l'ancre dépasse la durée annoncée, la position est plafonnée à
celle-ci plutôt que de la franchir — un morceau qui finit avant que la
station ne l'annonce ne doit pas afficher « 4:31 / 4:14 ».

## La cadence de publication

Un bras de plus dans le `select!` de `main.rs` : un tick d'une seconde,
**armé seulement quand une position est connue et que la lecture est en
cours**. À l'arrêt, en veille, sur un flux que personne ne suit : pas de
tick, et la déduplication des trames reprend tous ses droits — l'appareil au
repos ne produit pas une trame par seconde pour rien.

À chaque tick : rafraîchir la position (sondage mpv ou avance de l'ancre),
puis `publie_etat()`. Le champ ayant changé, la trame franchit la
déduplication et part vers la SPA et vers les afficheurs.

### Les messages éphémères continuent de passer

Le tick **ne suspend rien et ne masque rien**. Chaque trame emporte
l'`overlay` en cours intact, avec son `remaining_ms` rafraîchi comme
aujourd'hui ; le cœur reste seul maître de l'échéance, et le tick ne la
raccourcit ni ne la réarme. Un afficheur reçoit donc, dans une même trame, ce
qui joue *et* l'incrustation passagère, et c'est lui qui décide de la mettre
par-dessus, à côté, ou de l'ignorer — exactement ce que le chantier
« afficheurs, état structuré » a mis en place.

Le coût sur l'afficheur console a été vérifié plutôt que supposé : sa branche
incrustation ne rend que le `text` de l'overlay, invariant pendant toute sa
durée, et sa garde `dernieres_lignes` compare les lignes composées avant
d'écrire quoi que ce soit. Une trame par seconde pendant un message produit
donc des lignes identiques, donc **aucune réimpression et aucun
clignotement**. Le même raisonnement vaut hors incrustation, puisque cet
afficheur n'affiche pas la position (décision ci-dessous).

## Les touches et le déplacement

### Le pas, réglable

Un cinquième réglage dans `Settings` :

```rust
/// Pas des touches « avancer » / « reculer », en secondes.
pub seek_step_s: u32,
```

Défaut **10 s**, borné **1 à 120 s**. Hors bornes, le `PUT /api/settings` est
refusé par le catalogue i18n, comme les autres bornes du même écrit —
« les bornes ne peuvent pas mentir ». Persisté dans `state.json`, servi par
`GET /api/settings`, réglé par une carte de plus sur `/config`, avec son
entrée dans la table des matières collante.

Une pression = un pas : `held` reste ignoré sur ces commandes comme sur
toutes celles qui ne sont pas le volume. Pour balayer une longue piste, la
barre de la SPA est le bon outil.

### Web

Deux boutons de plus dans la rangée transport de la télécommande, **dans le
sens du geste** — reculer avant avancer, comme les autres paires depuis le
dernier chantier d'IHM. Deux clés i18n de plus.

### Télécommande physique

Deux actions apprenables de plus dans `generic-input`, et leur entrée dans
les presets `mce` et `keyboard` s'il existe une touche évidente à leur donner
(à vérifier sur les tables réelles ; à défaut, elles restent apprenables sans
défaut, ce que la page d'apprentissage sait déjà présenter).

## Rendu dans la carte Player

Une barre fine et un couple « écoulé / durée » sous le bloc « En écoute ».

- **Durée inconnue** → l'écoulé seul, **sans barre** : une barre sans fin
  n'apprend rien.
- **Position inconnue** → rien de nouveau ; la durée seule continue de
  s'afficher comme aujourd'hui.
- **Contenu non déplaçable** → barre en lecture seule, sans curseur ni clic,
  mais la position s'affiche quand même : c'est le cas Radio France, où
  savoir qu'on est à 1:27 d'un morceau de 4:14 a de la valeur même sans
  pouvoir s'y déplacer.
- **Contenu déplaçable** → clic et glissement vers `SeekTo`, et **le clavier
  fait la même chose** (`role="slider"`, flèches = un pas, Home/End =
  extrémités). Sans cela, la barre serait la seule commande de la page hors
  d'atteinte sans souris, sur une page dont toutes les autres sont des
  boutons.

`formateDuree` refuse aujourd'hui les valeurs `<= 0`, ce qui est juste pour
une durée et faux pour une position : `0:00` est un instant parfaitement
légitime. Un second formateur, plutôt qu'un assouplissement du premier qui
ferait réapparaître des « 0:00 » là où le refus servait.

Le tout dans un petit composant local à la SPA, pas dans le kit : seule la
carte Player s'en sert, et le kit est le contrat des pages de plugins.

## Ce qui ne change pas

- **L'afficheur console n'affiche pas la position.** Trois lignes d'une
  vingtaine de colonnes déjà pleines (source/présélection, nom, titre), et
  une horloge y coûterait un effacement d'écran par seconde. Le champ voyage
  quand même jusqu'à lui : tout autre plugin d'affichage peut s'en servir,
  c'est bien le sujet de la demande.
- **Le protocole `source` ne gagne aucun champ.** Le plugin `files` garde ses
  durées de m3u pour son propre usage : mpv mesure le fichier joué, ce qu'une
  étiquette ne fait pas toujours honnêtement.
- **L'arbitrage des métadonnées ne change pas.** La position et la durée
  suivent l'enrichissement gagnant ; aucun nouveau tour d'arbitrage.
- **`Overlay` ne change pas.** Le tick le republie tel quel.

## Tests

**`ritornello-proto`** — sérialisation des nouveaux champs ; une trame écrite
sans eux se relit (compatibilité ascendante) ; une trame qui n'a rien à en
dire ne les sérialise pas.

**Cœur** — le tick ne s'arme pas à l'arrêt ni en veille ; le tick ne touche
pas à l'échéance d'un overlay ; une trame publiée pendant une incrustation la
porte toujours ; le `time-pos` d'un flux est écarté ; l'ancre d'un
enrichissement avance entre deux enrichissements ; l'ancre est effacée au
changement d'identité ; la position est plafonnée par la durée annoncée ; la
durée de mpv l'emporte sur celle d'un plugin ; `SeekForward` / `SeekBackward`
/ `SeekTo` sont sans effet sur un contenu non déplaçable ; `seek_step_s` hors
bornes est refusé, et le refus porte une clé de catalogue.

**`ritornello-plugin-console`** — une trame qui ne change que la position ne
produit aucune écriture sur le tty.

**`radiofrance-metas`** — `position_s` calculé depuis `startTime` sur les
captures réelles déjà en place ; `None` quand la tranche n'est pas un
morceau, comme `duration_s`.

**SPA** — barre absente sans durée ; barre inerte sur un contenu non
déplaçable ; clic → la bonne commande avec la bonne seconde ; flèches et
Home/End au clavier ; `0:00` s'affiche.

**e2e** — le parcours existant vérifie que la barre apparaît et avance.

## Ce qui reste à mesurer sur le matériel

1. **`time-pos` sur un `cdda://` ouvert en disque entier** : relatif au
   disque ou à la piste ? (voir plus haut — décide d'un retranchement de
   début de chapitre).
2. **`duration` sur un flux** : mpv en annonce-t-il une, absurde, qu'il
   faudrait écarter au même titre que `time-pos` ? La garde `finite` la
   couvre déjà par construction ; à confirmer plutôt qu'à supposer.
