# Découper le titre ICY : un motif appris par station

Date : 2026-08-26

## Le but

Une radio annonce ce qui joue dans un seul champ de texte : `StreamTitle`, la
convention de fait de Shoutcast/Icecast. En pratique `Artiste - Titre`, parfois
`Titre - ARTISTE` (OUI FM), souvent rien d'exploitable du tout. Le cœur ne le
découpe **pas exprès** : découper serait supposer, et une supposition qu'on
affiche est un mensonge.

Le greffon `musicbrainz` peut faire mieux, parce qu'il peut **vérifier**. Il pose
une hypothèse de découpage, l'éprouve contre MusicBrainz, et se tait quand ça ne
colle pas. Ce n'est pas revenir sur la décision du cœur, c'est en respecter la
raison : le cœur refuse d'afficher une supposition, le greffon a de quoi
transformer la supposition en fait.

L'appareil y gagne deux choses : un artiste et un titre séparés là où il n'y
avait qu'une chaîne, et une **pochette** pour une radio — que le chemin
générique actuel ne peut pas trouver, puisqu'il exige un album que jamais aucune
radio n'annonce.

## Le nœud

Le format ICY est une propriété de **la station**, pas du morceau. Une station
qui émet `Titre - ARTISTE` l'émettra pour tous ses morceaux. Toute la conception
tient dans cette observation : sonder plusieurs découpages à chaque morceau
serait coûteux et bruyant ; sonder une fois par **station**, retenir le motif
gagnant, puis l'appliquer localement, ne coûte presque rien.

La clé de ce souvenir existe déjà dans le protocole. Une station déclare son
identité comme `{"kind":"stream","url":"…"}` (`plugin-radio/src/main.rs:62`,
figé par un test) et le greffon reçoit cette identité dans **chaque** trame
`NowPlaying`. C'est l'URL absolue du flux, et c'est la bonne clé — pas le numéro
de présélection, qui est propre à chaque source, remappable, et peut désigner la
même station à deux endroits.

## Hors périmètre

- **Corriger l'encodage.** Certaines stations émettent du latin-1 là où le client
  suppose de l'UTF-8. On le *diagnostique* (voir plus bas), on ne le répare pas.
- **Une pochette forcée à la main** par station. La page règle le découpage, pas
  l'image.
- **Migrer le fichier d'état** entre deux versions du format. Le fichier est
  rejetable : s'il ne se relit pas, on repart d'un état vide et on réapprend.
- **Toucher au chemin disque** du greffon (`kind: "disc"`), inchangé.

## La découverte qui contraint tout : le protocole doit porter la chaîne brute

Le déclencheur naturel semble être « un titre est connu, pas d'artiste ». **Il ne
marche pas**, et la raison est structurelle plutôt qu'accidentelle.

`Metadonnees::ajoute` refuse un enrichissement dont l'identité ne correspond plus
à ce qui joue — c'est le garde-fou de péremption. Mais l'identité d'une radio est
l'**URL du flux**, qui ne change pas entre deux morceaux. Et `set_icy` n'efface
délibérément **pas** les enrichissements : décision du propriétaire, documentée
sur place, pour éviter un affichage qui passerait par la forme ICY brute une
seconde avant chaque correction.

Conséquence : dès que le greffon aurait écrit un artiste, cet enrichissement
resterait valide au morceau suivant. `known.artist` serait renseigné, le
déclencheur ne se déclencherait plus, et `known.title` porterait la sortie du
greffon lui-même — jamais la nouvelle chaîne ICY. **La fonctionnalité tirerait
une fois par session, puis serait morte.**

Le greffon a donc besoin de voir ce que le flux a annoncé, indépendamment de ce
que la composition en a fait. Le cœur détient déjà exactement ça, séparément
(`Metadonnees::icy`) ; il ne le publie simplement pas.

## Le protocole : un champ de plus dans `Known`

```rust
pub struct Known {
    // … champs existants …
    /// Ce que le **flux lui-même** a annoncé, brut, sans découpage ni
    /// composition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_title: Option<String>,
}
```

Additif et `skip_serializing_if`, le même idiome que `known` et `covers` : une
trame écrite par un binaire antérieur se relit, un greffon qui ignore le champ
fonctionne comme avant, et la trame la plus courante ne grossit pas d'un octet.

Rempli depuis `Metadonnees::icy`, verbatim, dans `Metadonnees::known()`. Ce n'est
pas une redite de `title` : `title` est le résultat d'un arbitrage entre plusieurs
contributeurs, `stream_title` est un fait brut d'un seul émetteur. Seule la forme
brute peut être redécoupée, et seul le brut permet de remarquer que le morceau a
changé sur une station dont l'identité ne bouge pas.

**Bénéfice non prévu :** avec ce champ, le découpage d'un morceau devient une
opération **locale** dès que le motif est connu. Aucune requête réseau n'est
nécessaire pour séparer artiste et titre — seulement pour la pochette.

## Le motif, et ce qu'on en retient

```rust
enum Motif {
    /// Découper sur ce séparateur, dans cet ordre.
    Separe { separateur: String, artiste_en_premier: bool },
    /// Cette station n'annonce pas de morceau exploitable.
    NePasDecouper,
}

enum Origine {
    /// Sondée, et c'est bien `Artiste - Titre`.
    StandardConfirme,
    /// Sondée, et c'est autre chose — ordre inversé, séparateur inhabituel,
    /// ou rien d'exploitable.
    DeviationApprise,
    /// Posé à la main depuis la page d'admin.
    Manuel,
}
```

Deux énumérations et non une : `Motif` dit **ce que c'est**, `Origine` dit
**comment on l'a su**. Les confondre — mettre `NePasDecouper` parmi les origines —
ferait qu'un « ne pas découper » posé à la main serait indistinguable d'un « ne
pas découper » appris, et la règle d'invalidation ci-dessous a précisément besoin
de cette distinction.

**Un invariant les relie**, et il doit être tenu à l'écriture plutôt que supposé
à la lecture : `StandardConfirme` ne s'apparie qu'avec
`Separe { separateur: " - ", artiste_en_premier: true }`. Toute autre
combinaison est une `DeviationApprise` — c'est la définition du mot. La
construction passe donc par un constructeur qui dérive l'origine du motif au lieu
de laisser les deux champs se contredire.

### Une entrée par station sondée, y compris les conformes

`Artiste - Titre` est bien l'hypothèse par défaut — c'est le premier candidat du
sondage — mais **il ne faut pas l'exprimer par l'absence d'entrée**. L'absence
confondrait deux états qu'il faut distinguer : « jamais sondée » et « vérifiée
conforme ». Et elle ne peut de toute façon pas signifier uniformément
« standard », puisqu'une entrée explicite est déjà nécessaire pour
`NePasDecouper` — sinon une station parlée se ferait resonder à chaque titre,
indéfiniment. Dès que l'absence veut dire deux choses selon le cas, la logique de
resondage devient un arbre au lieu d'une question.

L'invariant est donc : **une entrée existe ⟺ cette station a été sondée.** La vue
« exceptions seulement » que veut le propriétaire devient un **filtre de la
page**, pas un trou dans le stockage.

Deux raisons de plus, côté page : sans les conformes, les colonnes « dernier
usage » et « titres découpés » n'existeraient que pour les fautives, donc la page
ne pourrait jamais dire « ces quarante-là vont bien », et un écran vide serait
ambigu — tout va bien, ou rien n'a jamais marché ? Et forcer un motif sur une
station d'apparence conforme passerait par une *création* au lieu d'une édition :
deux chemins de code pour une même action. Le stockage ne pèse rien — une
cinquantaine de stations, une ligne courte chacune.

### Où ça vit

`/var/lib/ritornello/plugin-musicbrainz.json`, chemin réglable par
`RITORNELLO_MUSICBRAINZ_STATE`, écriture atomique par fichier temporaire puis
`rename`. C'est exactement le schéma de `plugin-radio/src/state.rs`, et pour la
même raison : ce sont des **motifs appris**, de l'état runtime, pas une
configuration que l'administrateur écrit à la main dans `/etc`. Un fichier
illisible ou d'un format inconnu n'est pas une erreur fatale : journal, état vide,
on réapprend.

```json
{
  "stations": [
    {
      "url": "http://icecast.radiofrance.fr/franceinter-midfi.mp3",
      "motif": { "separateur": " - ", "artiste_en_premier": true },
      "origine": "standard_confirme",
      "dernier_usage": "2026-08-26T14:12:03Z",
      "titres_decoupes": 214
    }
  ]
}
```

`titres_decoupes` n'est pas décoratif : un motif à 200 succès et un à 1 ne
méritent pas la même confiance quand on choisit lequel supprimer.

## Le sondage d'une station inconnue

### Nettoyer avant de découper

Le bruit accolé est retiré **avant** toute tentative de découpage : réclame de la
station (`| Radio X`, `- Radio Y`), durée entre crochets (`[00:03:45]`), mentions
de version en fin de chaîne. Sans cette étape, une station qui accole son nom
ferait échouer *tous* les candidats et serait classée `NePasDecouper` à tort —
c'est-à-dire définitivement, puisque rien ne resonde une station ainsi classée.

### Les candidats se dérivent de la chaîne, pas d'une liste fixe

Une liste fixe de séparateurs × deux ordres donnerait dix candidats et dix
requêtes. Au lieu de ça, on regarde **quels séparateurs sont réellement présents**
dans la chaîne nettoyée, et on ne construit des candidats que pour ceux-là, dans
cet ordre de priorité : `" - "`, `" – "` (demi-cadratin), `" — "`, `" / "`,
`" : "`. Chaque séparateur présent donne deux candidats (les deux ordres).

En pratique une chaîne contient un seul type de séparateur, donc deux candidats.
Plafond dur : **quatre** candidats, et le journal dit ce qui a été écarté du fait
du plafond — un plafond silencieux se lirait comme « on a tout essayé ».

Aucun séparateur présent ⟹ un seul candidat, « le tout est le titre, pas
d'artiste », qui ne peut pas être validé faute d'artiste à contraindre ⟹
`NePasDecouper`.

### La validation : le prérequis qui manque aujourd'hui

Pour chaque candidat, une recherche d'**enregistrement** :

```
https://musicbrainz.org/ws/2/recording/?query=artist:"A" AND recording:"T"&fmt=json&limit=1
```

en réutilisant l'échappement existant de `requete_release` — Lucene à l'intérieur
des guillemets, puis l'URL par-dessus (`musicbrainz.rs`, dont la doc explique
pourquoi les deux étages sont nécessaires).

Un candidat est **accepté** si les deux conditions tiennent :

1. `score >= SEUIL_RECORDING` (90) ;
2. le titre de l'enregistrement rendu **égale** le titre du candidat après
   normalisation (minuscules, diacritiques retirés, ponctuation et espaces
   ramenés).

La seconde condition porte le poids. Le score seul est trop généreux : la
recherche MusicBrainz rend presque toujours quelque chose de plausible, et c'est
exactement ce qui rendrait « le premier candidat qui marche » toujours vrai. Une
égalité de chaînes après normalisation, elle, ne se laisse pas convaincre.

L'artiste n'est pas revérifié par égalité, et c'est délibéré : la requête le
contraint déjà, et les orthographes de station varient plus sur l'artiste (`The
Beatles` / `Beatles`) que sur le titre.

### D'où sort la pochette

La même réponse porte les releases de l'enregistrement. On prend le **premier**
`releases[].id` et on construit l'URL par `url_caa`, la fonction qui sert déjà au
chemin disque. Un enregistrement sans release, ou une release sans image, ne
change rien au découpage : le couple artiste/titre est acquis, l'appareil affiche
le texte sans image, et le cœur traite déjà une pochette absente en silence (voir
la doc d'`url_caa`).

Pas de choix « intelligent » de la release — original contre compilation contre
remaster : ce serait une heuristique de plus, sur une information que MusicBrainz
ne classe pas par pertinence, pour un carré de 500 pixels. Le premier suffit, et
c'est explicitement le compromis retenu.

### Le meilleur, pas le premier

Le propriétaire a demandé « le premier qui marche ». On prend **le meilleur score
parmi les acceptés**, et la différence compte : l'ordre inverse d'un vrai couple
rend souvent lui aussi un résultat au-dessus du seuil, avec un score plus faible.
Comparer les scores est précisément ce qui distingue `Artiste - Titre` de
`Titre - Artiste`, là où s'arrêter au premier accepté choisirait selon l'ordre
d'essai. Le coût est identique — les candidats sont sondés de toute façon, et
seulement au premier morceau d'une station.

Aucun candidat accepté ⟹ `NePasDecouper`, en `DeviationApprise`.

## Le régime établi : une requête par morceau, qui valide en continu

Station connue, motif `Separe` :

1. Découper localement selon le motif. **Aucun réseau.**
2. Chercher la pochette par une recherche d'enregistrement sur le couple obtenu —
   requête nécessaire de toute façon, puisqu'une radio n'annonce pas d'album et
   que le chemin générique existant ne peut donc rien trouver.
3. Cette requête **est** la validation continue. Elle valide ⟹ émettre artiste,
   titre et pochette, incrémenter `titres_decoupes`, et remettre à zéro le
   compteur d'échecs consécutifs.
4. Elle échoue ⟹ **ne rien émettre pour ce morceau**, et incrémenter le compteur
   d'échecs consécutifs. Le resondage n'a lieu qu'à partir de
   `ECHECS_AVANT_RESONDAGE` (3) échecs **d'affilée**.

Ce seuil de trois n'est pas de la prudence décorative, il corrige un défaut réel
de la version simple de cette règle. Un morceau obscur que MusicBrainz ne connaît
pas est un échec **parfaitement légitime sur un motif juste** : resonder au
premier échec ferait donc partir un sondage sur chaque titre rare, et — puisque
l'ordre inverse rend parfois lui aussi un résultat acceptable — pourrait
remplacer un bon motif par un mauvais sur un seul coup de chance. Trois échecs
d'affilée, en revanche, décrivent une station qui a changé de forme et non un
titre que le catalogue ignore.

Quand le resondage a lieu : les autres candidats sont éprouvés une fois. Si un
autre motif gagne, il remplace — **sauf `Origine::Manuel`**, jamais écrasé, et
c'est là que la distinction des deux énumérations paie. Si aucun ne gagne, le
motif est **gardé** : un passage à vide ne doit pas faire désapprendre une station
qui a déjà découpé deux cents titres.

Le compteur d'échecs consécutifs vit en mémoire et n'est **pas** persisté : il
décrit une suite d'événements en cours, pas un fait acquis sur la station, et le
redémarrage du greffon est une remise à zéro légitime.

Motif `NePasDecouper` : rien du tout, aucune requête. Le coût d'une station
parlée est nul.

Une station ainsi classée n'est **jamais** resondée automatiquement. C'est
assumé : la resonder périodiquement dépenserait du réseau sur la station qui en
mérite le moins. Le geste de reprise est la suppression de l'entrée depuis la
page d'admin — et c'est précisément à ça que sert le bouton.

Une pochette est cherchée par couple `(artiste, titre)` avec mémorisation des
échecs, comme le chemin générique le fait déjà par `(artiste, album)` : deux
morceaux identiques d'affilée ne font pas deux requêtes.

## Le défaut latent à corriger d'abord

`premier_release_id` prend le **premier** résultat de recherche, aveuglément, en
ignorant le `score` que MusicBrainz renvoie. Aujourd'hui, un fichier dont l'album
est mal orthographié reçoit donc une pochette fausse avec aplomb.

C'est le même défaut que celui qui rendrait le découpage sans valeur, il est
indépendant de cette fonctionnalité, et il se corrige en trois lignes : un seuil
sur le chemin `release` existant. **À faire en premier**, pour que le reste de la
conception s'appuie sur un client qui sait dire non.

## L'émission et l'arbitrage

Le chemin ICY émet un `Enrichment` avec `fill_only: false` — il doit **remplacer**
le titre affiché, la chaîne ICY brute étant précisément ce qu'on corrige. C'est
la première fois que ce greffon écrase sur le chemin générique, et ça se justifie
par la validation : il n'écrase que ce que MusicBrainz a confirmé.

Deux conséquences à connaître, toutes deux déjà satisfaites :

- `bloc_de_texte` rend le bloc gagnant **en entier**, jamais composé champ par
  champ, et parcourt les greffons dans l'ordre déclaré de `plugins.toml`. Il faut
  donc que `musicbrainz` soit déclaré **après** les greffons de station. C'est
  déjà le cas dans `deploy/plugins.example.toml` (`ouifm-metas` puis
  `radiofrance-metas` puis `musicbrainz`), et cet ordre devient une **exigence**
  à écrire dans le fichier d'exemple plutôt qu'une coïncidence.
- L'arbitrage se fait par priorité déclarée, **pas** par ordre d'arrivée : un
  greffon de station qui répond après `musicbrainz` gagne quand même. Il n'y a
  donc pas de course à gérer.

`origin` vaudra `musicbrainz` sur le badge de la carte du lecteur, ce qui est
exactement l'information utile — on voit qui a affirmé le titre.

## La page d'admin

Le greffon est `metadata` **seul** aujourd'hui. La machinerie à reprendre est
celle du greffon MPD, à l'identique : `AdminPlugin` du SDK
(`asset` / `catalog` / `get_data` / `set_data`), `.admin(…)` dans le chaînage du
`Runtime`, un paquet `ui/` en build Vite de bibliothèque avec `vue` et
`@ritornello/ui` en externes, `ui.js` et `ui.css` embarqués par `include_str!`, et
un `build.rs` qui écrit un bouchon si `ui/dist` manque plutôt que de lancer `npm`.

Le tableau : URL du flux, motif, origine, dernier usage, titres découpés. Trié par
dernier usage décroissant. Filtre « exceptions seulement » **actif par défaut**,
qui masque les `StandardConfirme`.

Trois actions : supprimer une entrée, tout vider, et éditer un motif.

**Le champ d'édition n'est pas une expression rationnelle libre.** Ce serait le
plus puissant et le pire choix : il ferait débuguer des regex à
l'utilisateur, et une mauvaise casserait tous les titres de la station. Le jeu est
fermé — un séparateur (choisi dans la liste, ou saisi comme littéral), l'ordre, ou
« ne pas découper ». Ça couvre tous les cas réels, ça ne peut pas être malformé, et
ça reste affichable dans une colonne. Une édition pose `Origine::Manuel`, que le
réapprentissage ne touchera jamais.

`set_data` rend une erreur qui est **déjà une phrase traduite**, jamais une clé
brute — contrat du SDK, éprouvé par les tests du greffon MPD.

### Traductions

Le greffon n'a aujourd'hui **ni** `src/locales/en.toml` **ni**
`deploy/locales/musicbrainz/fr.toml` : les deux sont à créer, avec le test de
parité des clés côté greffon (celui de `plugin-mpd/src/admin.rs` est le modèle).

Un greffon `metadata` **ne reçoit pas** `SetLocale` — cette trame n'existe que
pour `SourcePlugin`. Le catalogue est donc figé à la langue passée au lancement, et
un changement de langue ne se voit sur cette page qu'après redémarrage du greffon.
C'est la même limite que la page du greffon MPD ; à écrire, pas à corriger ici.

## Étrangler le débit

MusicBrainz demande environ une requête par seconde et par client. Le greffon pose
bien un `User-Agent` mais **n'a aucun étranglement**. Le sondage de quatre
candidats partirait donc en rafale.

Un intervalle minimal partagé entre requêtes, dans `musicbrainz.rs`, qui couvre
tous les chemins du greffon (disque, release, recording) — pas seulement le neuf.
Un sondage complet prend alors quatre secondes, une fois par station : sans
conséquence, puisque rien n'attend ce résultat pour jouer.

## L'encodage, qu'on ne répare pas mais qu'on nomme

Des stations émettent du latin-1 là où le client suppose de l'UTF-8, ou l'inverse.
Un titre en mojibake ne validera **jamais** contre MusicBrainz, et ressemblera à
un mauvais découpage alors que le découpage était bon.

Quand la chaîne brute contient un caractère de remplacement `U+FFFD` ou une
séquence caractéristique de latin-1 relu en UTF-8, le journal le dit
**distinctement** de l'échec de validation ordinaire, et le morceau ne compte pas
comme un échec de motif — il ne doit pas déclencher un resondage qui échouera
pareil. Sans cette distinction, on cherche le défaut du mauvais côté.

## Ce que les tests doivent mordre

Les propriétés dont la fausseté ne se verrait pas autrement :

1. **Le champ brut survit à l'écrasement.** Après que le greffon a émis un
   artiste, une nouvelle chaîne ICY sur la **même** identité doit encore lui
   parvenir dans `stream_title`. C'est la propriété qui rend la fonctionnalité
   possible ; sans test, sa perte se manifesterait par « ça marche une fois ».
2. **Le nettoyage précède le découpage.** Une chaîne avec réclame accolée doit
   être découpée correctement, pas classée `NePasDecouper`.
3. **La garde de validation refuse un faux positif.** Un couple inversé dont
   MusicBrainz rend un résultat de score moyen doit être écarté par l'égalité de
   titre normalisée, pas accepté sur son score.
4. **Le meilleur gagne, pas le premier.** Deux candidats acceptés, celui de
   meilleur score retenu — et le test doit poser le meilleur en **second** dans
   l'ordre d'essai, sinon il passe aussi avec « le premier accepté ».
5. **`Manuel` n'est jamais écrasé** par un réapprentissage qui aboutit.
6. **Un échec isolé ne resonde pas.** Un morceau inconnu du catalogue, sur une
   station au motif juste, ne doit déclencher aucune requête de sondage — et le
   contrôle qui va avec : trois échecs d'affilée en déclenchent un. Sans les
   deux moitiés, le test passerait aussi bien avec « resonder toujours » qu'avec
   « ne resonder jamais ».
7. **Un succès remet le compteur à zéro** : deux échecs, un succès, deux échecs
   ne doivent pas resonder. C'est la seule assertion qui distingue un compteur
   consécutif d'un compteur cumulatif, et le cumulatif est le défaut naturel.
8. **Une entrée existe ⟺ station sondée**, conformes incluses : le filtre de la
   page masque, il ne détermine pas ce qui est stocké.
9. **`NePasDecouper` ne déclenche aucune requête.**
10. **Parité des clés en/fr** du greffon.

Les fixtures doivent être des réponses MusicBrainz **réalistes**, avec leur champ
`score` : une preuve bâtie sur une réponse que le service ne peut pas émettre ne
prouve rien.

## Ce qui reste ouvert

- **Le seuil de 90** est un choix raisonné, pas mesuré. Il se validera à l'usage,
  sur des stations réelles, et mérite peut-être d'être réglable si le parc
  s'avère hétérogène.
- **La normalisation des titres** est un jugement : trop stricte, elle refuse
  `Pt. 4` contre `Part 4` ; trop souple, elle accepte deux morceaux différents.
  Commencer strict, et desserrer sur des cas constatés plutôt que par anticipation.
- **Rien n'a jamais tourné sur l'appareil**, ce chantier comme les précédents.
  Le premier essai utile : une station de chaque forme (standard, inversée,
  parlée), et vérifier que la troisième cesse d'être sondée.
