# Les pochettes d'album, et le protocole `metadata` en étages

**Date :** 2026-08-24
**État :** validé par le propriétaire, prêt pour le plan d'implémentation
**Base :** `main` à `530312f`. Le chantier « rendez-vous des greffons » y est
fusionné, mais celui-ci ne dépend plus de son multi-genre : les décisions 6 et
7 s'en passent.

Ce chantier fait deux choses, et la seconde était la condition de la première :
il affiche les pochettes d'album, et pour cela il **refond le protocole
`metadata`** en un modèle où l'information de la piste courante se complète par
étages, chaque contributeur déclarant s'il écrase ou s'il complète.

## Le problème

### Aucune image, alors que l'information est là

L'appareil affiche l'artiste, le titre et l'album de ce qui joue. Il n'affiche
aucune image, alors que l'information est déjà là et jetée à trois endroits.

L'état des lieux, mesuré dans le code plutôt que supposé :

- **Les deux greffons de métas radio reçoivent déjà un identifiant de
  pochette et le jettent.** La trame de Radio France porte un champ `cover`
  (un UUID) que `live.rs` n'extrait pas ; la trame SSE d'OUI FM porte un
  `coverId` (un chemin de CDN) que `flux.rs` ignore. Les deux figurent dans
  les fixtures de test du dépôt, sous les yeux, depuis le chantier
  métadonnées.
- **`ritornello-plugin-musicbrainz` jette le `release.id`.** `parse_lookup`
  extrait l'artiste, l'album et les titres du media retenu, et laisse tomber
  l'identifiant de la release — qui est précisément la clé du Cover Art
  Archive.
- **`lofty` est déjà une dépendance**, dans `ritornello-plugin-files`, où il
  sert à lire les durées par en-tête (0,33 ms par fichier, mesuré au chantier
  liste de lecture). Il lit aussi les images embarquées ; personne ne le lui
  demande.
- **Le protocole n'a aucun champ d'image**, ni dans `Enrichment`, ni dans
  `Morceau`, ni dans `PlayerState`.
- **Le cœur ne sert aucun octet binaire.** Ses routes rendent du JSON, du
  HTML et les assets embarqués de la SPA (`rust-embed`). Il n'a pas de client
  HTTP du tout : `reqwest` est absent de son `Cargo.toml`, seuls les greffons
  en ont un.

### Et un protocole qui ne laisse pas compléter

Le protocole `metadata` actuel est un **aller simple sur identité** : le cœur
annonce ce qui joue sous la forme d'une identité opaque, chaque greffon dit
tout ce qu'il sait de cette identité ou se tait, et le premier déclaré qui
répond gagne tout.

Ce modèle a une conséquence qui bloque les pochettes : **un greffon ne sait
jamais ce que les autres ont déjà trouvé.** Il en découle deux impasses.

- Un greffon capable de résoudre une pochette à partir d'un artiste et d'un
  album — ce que MusicBrainz fait très bien — ne peut pas travailler, parce
  que l'identité opaque ne porte ni artiste ni album. Elle porte une URL de
  flux, ou une TOC, ou un chemin.
- Un greffon ne peut pas s'abstenir quand l'information existe déjà, ni
  déclarer qu'il ne vient que compléter. Il gagne tout ou se tait.

D'où la refonte. **La piste courante devient un état partiel qui se complète
par étages**, et **chaque contributeur déclare son intention** : écraser, ou
seulement remplir ce qui manque.

**Pourquoi maintenant :** les trois sources de contenu sont en service, la
carte Player a la place pour une image, le chantier « rendez-vous des
greffons » vient d'arriver, et la documentation du protocole mentionnait déjà
« a late cover » comme un cas anticipé.

## Les motifs d'URL, vérifiés en direct

Mesurés le 2026-08-24 contre les services réels, pas déduits d'une
documentation :

| Provenance | Motif | Résultat mesuré |
|---|---|---|
| Radio France | `https://api.radiofrance.fr/v1/services/embed/image/{cover}?preset=400x400` | 301 vers le CDN `www.radiofrance.fr/s3/cruiser-production/…`, puis JPEG de 31 887 octets |
| OUI FM | `https://www.lesindesradios.fr/servicesimb/images?version=6&iid={coverId}&width=400` | JPEG de 35 613 octets |
| Cover Art Archive | `https://coverartarchive.org/release/{mbid}/front-500` | JPEG de 75 249 octets |
| Recherche release | `https://musicbrainz.org/ws/2/release/?query=artist:"…" AND release:"…"&fmt=json&limit=1` | premier résultat au score 100 |

Quatre mesures qui ont pesé sur la conception :

- **`front` nu rend un PNG de 2 670 705 octets** là où `front-500` en rend
  75 249. C'est ce qui justifie de demander une taille bornée plutôt que
  l'original, et de plafonner ce que le cœur accepte.
- **Le motif d'OUI FM vient de son propre lecteur**, trouvé dans le bundle
  `_app` de `ouifm.fr/player`, dans le code qui lit exactement le même flux
  SSE que notre greffon : `t.coverUrl || (t.coverId && "…/servicesimb/images?version=6&iid=" + t.coverId + "&width=200")`.
  La trame peut donc porter une URL toute faite, et à défaut un identifiant à
  composer — les deux cas sont réels et le greffon doit traiter les deux. Les
  largeurs 200, 400 et 600 répondent toutes ; 400 est retenu.
- **Radio France sert une pochette générique pour « Le direct ».** Dans la
  trame, les entrées qui ne sont pas un morceau portent `songUuid: null` à
  côté d'un `cover` bien rempli — l'image de la station. C'est mesuré : la
  réponse de FIP portait un `songUuid` réel dans `now` et des `null` dans
  `prev` et `next`, chacun avec sa pochette. Ce champ est ce qui permet au
  greffon de ne pas annoncer un placeholder (décision 12).
- **La limite de débit de MusicBrainz ne mord pas ici.** Elle existe (une
  requête par seconde et par adresse sur `ws/2`, user-agent obligatoire), mais
  l'appareil ne joue qu'un morceau à la fois et n'interroge que ce qui joue :
  une requête toutes les trois ou quatre minutes. Elle n'est pas un argument
  de conception, et le greffon `musicbrainz` actuel ne throttle rien.

## Les étages, et l'intention déclarée

Trois étages, du moins informé au plus informé :

1. **La Source** — ce qu'elle déclare de ce qu'elle joue, sur son propre
   canal.
2. **Le cœur** — ce que mpv lui apprend (ICY, tags du fichier) et ce qu'il
   lit lui-même sur le disque (la pochette embarquée).
3. **Les greffons `metadata`.**

**Ce qui circule vers les greffons est l'état partiel, pas seulement
l'identité.** Un greffon reçoit ce qui est déjà connu et voit donc ce qui
manque.

**Chaque contributeur déclare s'il écrase ou s'il complète**, et c'est le cœur
de la refonte. Le cœur ne peut pas deviner la différence entre un greffon
spécialisé qui sait mieux et un greffon générique qui devine : lui seul le
sait, donc c'est lui qui le dit. Le déduire d'un ordre dans un fichier que
rien ne protège serait fragile — et c'est exactement le principe que ce projet
applique déjà, où la Source déclare elle-même son `preset_name`, son
`can_eject`, son `finite`, parce qu'elle seule sait.

En pratique cette intention est **constante par contributeur**, ce qui est un
bon signe : elle exprime un métier, pas une décision au cas par cas.

| Contributeur | Intention | Pourquoi |
|---|---|---|
| `radiofrance-metas`, `ouifm-metas` | écrase | il lit le flux officiel de la station : il sait mieux que l'ICY, par construction |
| `musicbrainz` | complète | il ne sait pas ce qui joue ; il résout depuis ce qu'on lui donne, donc il peut se tromper d'édition |
| le cœur, pour la pochette embarquée | complète | pour que le fichier posé dans le répertoire, déclaré par la Source, garde la préséance |

**La boucle est bornée, et par un mécanisme déjà en place.** Le cœur ne
rediffuse un état que s'il a **changé** — c'est la déduplication par égalité
que `publie_etat` pratique déjà, et que `set_tags` et `set_icy` pratiquent
champ par champ. Chaque champ ne passe de vide à rempli qu'une fois par piste,
et un contributeur qui écrase ne peut le faire qu'une fois : le nombre de
tours est borné par le nombre de champs multiplié par le nombre de
contributeurs, pas par le temps.

## Décisions

Quinze choix tranchés en conception, à ne pas re-débattre en implémentation.

1. **`NowPlaying` porte l'état partiel**, en plus de l'identité opaque : ce
   qui est connu du morceau, et si une pochette est déjà tenue. C'est la
   refonte, et tout le reste en découle.
2. **Un type dédié pour ce qui est connu**, pas la réutilisation de `Morceau`.
   `Morceau` porte `cover_href` et `cover_origin`, qui sont des URL locales de
   l'appareil : elles n'ont aucun sens pour un greffon et l'inviteraient à
   croire qu'il peut les lire.
3. **La pochette est annoncée comme tenue ou non, jamais transmise.** Un
   greffon n'a pas besoin de l'image pour décider s'il doit en chercher une ;
   lui envoyer des octets, ou même une URL locale, n'ajouterait rien et
   alourdirait chaque trame.
4. **L'intention est déclarée par le contributeur, pas déduite d'un ordre.**
   Un champ `fill_only` sur l'enrichissement, dont le **défaut est
   « écrase »** — ce qui est la règle actuelle du projet (« a plugin takes
   precedence over ICY and over file tags under all circumstances »). Avec ce
   défaut, les trois greffons livrés n'ont rien à changer, et seul
   `musicbrainz` déclare son intention.
5. **L'ordre de `plugins.toml` ne départage plus que des pairs.** Deux
   greffons qui écrasent tous les deux le même champ — deux greffons
   spécialisés pour une même station, cas pathologique. Le critère reste la
   prévisibilité pour qui débogue : « premier arrivé » dépendrait de la
   latence réseau.
6. **Aucun genre de plugin n'est ajouté.** Le rôle « je ne fais que
   compléter » est une propriété de l'enrichissement, pas du greffon : un
   même binaire peut écraser dans un cas et compléter dans un autre — ce que
   `musicbrainz` fera précisément, écrasant sur un disque dont il tient la
   TOC et complétant partout ailleurs.
7. **Une Source déclare ses métadonnées sur son propre canal**, sans devenir
   un greffon `metadata`. Le canal existe déjà : `SourceMessage` accepte
   `id: None` comme notification spontanée, et le trait `SourcePlugin` a déjà
   `poll_notification`, qui remonte identité, statut et présélection. `files`
   reste donc **mono-genre**, et répond vite à son `Play` avant d'annoncer la
   pochette quand son `readdir` a abouti — ce qui compte sur un partage SMB.
8. **C'est l'appareil qui va chercher l'image, jamais le navigateur.** Le
   cœur télécharge, retient et sert ; la page ne charge que depuis l'appareil.
   Cela tient le principe déjà posé pour les pages d'admin (« the page loads
   no external resource »), rend l'image disponible à un futur afficheur
   graphique, et couvre le cas que le navigateur ne pourrait pas traiter :
   une pochette embarquée dans un fichier.
9. **Deux champs pour deux rôles.** `cover` est ce qu'un contributeur a
   trouvé, sous une forme que le cœur doit encore aller chercher ;
   `Morceau.cover_href` est ce que l'IHM met dans un `src`, et c'est toujours
   une URL locale. Un seul champ laisserait passer l'URL externe jusqu'au
   navigateur, ce que la décision 8 refuse.
10. **`cover` accepte une URL `https` ou un chemin local**, sous deux formes
    explicitement distinctes — pas une chaîne polymorphe que le cœur
    devinerait. Le chemin local sert au `folder.jpg`, **qui existe déjà sur le
    disque** : rien à extraire, aucun fichier temporaire, aucun répertoire
    partagé à provisionner.
11. **`files` lit le répertoire, le cœur extrait l'embarquée.** Chacun là où
    c'est le plus simple : `files` annonce le chemin d'un fichier qui est là ;
    le cœur, qui tient déjà le chemin du fichier audio par mpv, en extrait
    l'image avec `lofty`. Aucun fichier temporaire nulle part. Le cœur
    **complète** pour cette pochette (voir le tableau des intentions), ce qui
    donne au `folder.jpg` la préséance voulue sans qu'aucune convention n'ait
    à être inversée.
12. **Un greffon n'annonce pas un placeholder.** `radiofrance-metas` ne
    remonte la pochette que quand `songUuid` est non nul : la station sert une
    image générique pour « Le direct », et l'annoncer ferait taire le relai
    générique — un champ rempli est un champ rempli, aucun étage supérieur ne
    peut savoir qu'il l'est mal. C'est une vérification locale dans le greffon
    qui connaît le format, pas une règle dans le cœur, qui ne peut pas juger
    qu'une image est un placeholder.
13. **Le relai générique exige un artiste et un album séparés.** Il ne se
    déclenche **jamais** sur un titre ICY. L'ICY est un texte brut, non
    découpé exprès dans ce projet, et OUI FM émet `Titre - ARTISTE` dans
    l'ordre inverse de l'usage : le donner à MusicBrainz rendrait n'importe
    quoi avec assurance.
14. **Le cache est en mémoire, borné, et ne survit pas au redémarrage.** Un
    cache disque ne gagnerait presque rien : une radio change de morceau
    toutes les trois minutes, le CD est déjà mémorisé par disque dans son
    greffon, et une pochette locale se relit en une fraction de milliseconde.
    Il coûterait un répertoire à provisionner, une propriété, une politique de
    purge et un état de plus à comprendre quand quelque chose cloche.
15. **Le cœur ne publie une pochette qu'une fois les octets en main.** L'IHM
    ne reçoit donc jamais l'URL d'une image cassée. C'est ce qui rend
    inoffensifs les 404 du Cover Art Archive, fréquents : beaucoup de
    releases n'ont pas d'image, et l'échec devient un silence au lieu d'un
    cadre vide.

## Ce qui voyage

### `NowPlaying` — l'état partiel

```rust
pub struct NowPlaying {
    pub source: String,
    #[serde(default)]
    pub identity: Option<serde_json::Value>,
    /// Ce qui est **déjà connu** du morceau, tous étages confondus.
    /// Un champ à `None` est un champ que personne n'a encore rempli.
    #[serde(default)]
    pub known: Known,
}

/// L'état partiel tel qu'un greffon a besoin de le voir.
#[derive(Default)]
pub struct Known {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub duration_s: Option<u32>,
    /// Une pochette est **déjà tenue**. Un booléen, jamais l'image :
    /// un greffon n'a pas besoin de la voir pour décider de chercher.
    pub cover: bool,
}
```

`Known` et non `Morceau` (décision 2). `#[serde(default)]` sur `known` : une
trame écrite par un binaire antérieur se relit, et un greffon qui ignore le
champ continue de fonctionner exactement comme avant — c'est ce qui rend la
refonte déployable greffon par greffon.

### `Enrichment` — deux champs de plus

```rust
/// Ce qu'un contributeur a trouvé comme pochette, à charge pour le cœur
/// d'aller la chercher. Jamais des octets : le canal reste textuel.
#[serde(default)]
pub cover: Option<CoverRef>,

/// Ce contributeur ne fait que **compléter** : il ne remplace aucun champ
/// déjà renseigné. Défaut `false` = il écrase, ce qui est la règle actuelle
/// et évite de toucher aux greffons existants.
#[serde(default, skip_serializing_if = "std::ops::Not::not")]
pub fill_only: bool,

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoverRef {
    /// URL externe, à télécharger. `https` uniquement.
    Url { url: String },
    /// Chemin absolu d'un fichier image déjà présent sur le disque.
    Path { path: String },
}
```

Deux formes explicites pour `CoverRef`, pas une chaîne que le cœur devinerait
(décision 10).

**Validation, dans `cleaned()`.** Les deux formes sont des entrées venues d'un
autre processus, et le cœur va agir dessus — il faut les traiter comme telles.

- `Url` : rejet de tout ce qui n'est pas `https://` visant un nom d'hôte ; une
  adresse IP littérale est refusée. Ce n'est pas une méfiance envers les
  greffons, c'est que **leurs données viennent du réseau** : le champ
  `coverUrl` de la trame SSE d'OUI FM est écrit par un tiers. Sans ce filtre,
  une trame hostile fait émettre à l'appareil une requête vers l'adresse de
  son choix sur le réseau local.
- `Path` : chemin **absolu**, extension parmi `jpg`, `jpeg`, `png`, `webp`, et
  — avant de servir un seul octet — vérification des octets d'en-tête du
  fichier. Sans cela, un contributeur mal écrit ferait servir n'importe quel
  fichier du système sur une route HTTP publique.

`is_empty()` **ignore** `cover`, comme il ignore déjà `duration_s` : une
pochette seule ne doit pas gagner l'arbitrage du texte.

### `Notification` de Source — un champ de plus

Le même `cover: Option<CoverRef>`, remonté par `poll_notification` et déjà
recopié dans `SourceMessage` par le SDK avec les autres champs de la
notification. C'est ce qui permet à `files` de rester mono-genre (décision 7).

### `Morceau` — deux champs de plus

```rust
/// URL **locale** de la pochette, à mettre telle quelle dans un `src`.
/// Toujours de la forme `/api/cover/{clé}`. `None` = aucune pochette.
pub cover_href: Option<String>,
/// Qui a fourni cette pochette : le nom de la Source, `"tags"`, ou le nom
/// du greffon.
pub cover_origin: Option<String>,
```

Une seconde origine, et non la réutilisation de l'`origin` du texte : le texte
et l'image peuvent venir de deux contributeurs différents, et une seule
origine ne saurait plus répondre pour les deux. Le champ existe pour la même
raison que son aîné — « qui a dit ça ? » est la première question devant une
pochette manifestement fausse.

Les deux champs sont `skip_serializing_if = "Option::is_none"` : une trame
sans pochette reste identique à l'octet près à ce qu'elle est aujourd'hui.

### `PlayerState` — rien à faire

`Morceau` y est déjà aplati par `serde(flatten)` : les deux champs arrivent
dans la charge utile de la SPA et des afficheurs sans une ligne de plus.

## D'où vient la pochette

Cinq contributeurs. L'ordre effectif découle des étages et des intentions, il
n'est écrit nulle part comme une liste de priorités.

### 1. `files` — le fichier posé à côté (origine `files`)

Sur une identité `{kind: "file", path: …}`, le greffon cherche :

1. **dans le répertoire du fichier joué**, par ordre de préférence de nom :
   `cover`, `folder`, `front`, `albumart`, `album` — extensions `jpg`,
   `jpeg`, `png`, `webp`, comparaison insensible à la casse ;
2. **dans un sous-répertoire d'artwork** — `artwork`, `scans`, `covers`,
   `art` — même ordre de préférence, et **un seul niveau de profondeur** :
   au-delà, on parcourt un NAS pour trouver une image ;
3. à défaut, s'il n'y a **qu'une seule** image dans le répertoire, celle-là.

Il annonce un `CoverRef::Path` par notification sur son canal `source`
(décision 7), sans devenir un greffon `metadata`. C'est lui qui fait ce travail
et non le cœur parce que c'est lui qui a monté ce partage et qui connaît la
racine de la source déclarée, et parce qu'un `folder.jpg` n'a rien à
extraire : le chemin suffit.

**La liste d'exclusion** — `back`, `verso`, `inlay`, `cd`, `disc`, `disque`,
`booklet`, `matrix` — ne s'applique **qu'à la règle 3**, la seule qui devine.
Les règles 1 et 2 n'en ont pas besoin : elles ne retiennent qu'un nom de leur
propre liste de préférence, donc un répertoire contenant `front.jpg` et
`back.jpg` est réglé par la préférence.

Le cas que l'exclusion traite est celui du répertoire qui ne contient **qu'une**
image, nommée comme un dos ou un livret : `back.jpg` seul, `Scan_verso.png`
seul. Sans elle, la règle 3 y verrait une pochette et afficherait le dos du
boîtier. Avec elle, `files` se tait — et le relai générique peut prendre la
main.

### 2. Le cœur — la pochette embarquée (origine `tags`)

Lue par `lofty`, qui devient dépendance du cœur, où il rejoint la couche « ce
que le fichier porte » qui existe déjà, alimentée par la propriété `metadata`
de mpv. Le cœur **complète** : il ne tente l'extraction que si la case est
encore vide, ce qui laisse la préséance au fichier posé dans le répertoire et
évite au passage une lecture inutile.

**Comment le cœur sait quel fichier est joué.** Il ne le lit pas dans
l'identité : il a fait un principe de ne jamais l'interpréter. Il le lit chez
mpv, dont la propriété `path` s'ajoute aux cinq déjà observées (`OBSERVEES`
dans `player/mpv.rs`). Le principe est tenu — c'est mpv qui dit ce qu'il joue,
pas la Source. L'extraction n'est tentée que sur un chemin **sans schéma**,
donc jamais sur un flux.

### 3. `radiofrance-metas` — écrase

`now.cover` de la trame qu'il analyse déjà → `CoverRef::Url` avec le motif
vérifié plus haut, `preset=400x400`. **Seulement si `now.songUuid` est non
nul** (décision 12).

### 4. `ouifm-metas` — écrase

`coverUrl` de la trame **si son hôte est celui qu'il connaît**, sinon composé
depuis `coverId` avec `width=400`.

### 5. `musicbrainz` — le relai générique

Ne lit aucun fichier. Deux chemins, deux intentions :

- **le disque** : il tient la TOC, donc il sait ce qui joue — il **écrase**,
  comme aujourd'hui. `DiscInfo` gagne le `release.id` que `parse_lookup` jette
  aujourd'hui → `front-500`, sans requête supplémentaire ;
- **tout le reste** : il **complète** (`fill_only: true`). Si `known.artist`
  et `known.album` sont tous deux renseignés et que `known.cover` est faux, il
  cherche la release et annonce `front-500`. Ce chemin sert le fichier nu du
  NAS, la radio dont le greffon de métas donne le texte mais pas l'image, et
  tout ce qui viendra ensuite.

C'est le cas qui justifie la décision 6 : un même binaire porte les deux
intentions, donc l'intention ne pouvait pas être une propriété du greffon.

## La route et le cache

```
GET /api/cover/{clé}
```

Rend les octets avec leur `Content-Type` et un `Cache-Control` immuable — la
clé est une empreinte, donc le contenu ne change jamais sous elle. Clé
inconnue : 404.

**La clé** est un hash de l'URL ou du chemin d'origine, calculé avec le
`DefaultHasher` de la bibliothèque standard. Pas de `sha2` : une collision
ferait afficher la mauvaise pochette et rien d'autre, ce qui ne justifie pas
une dépendance cryptographique.

**Le cache** est une petite table bornée à quatre entrées, de deux natures :

```rust
enum Pochette {
    /// Venue du réseau : les octets sont en mémoire.
    Octets(Vec<u8>, &'static str),
    /// Locale : seul le chemin est retenu, la route relit le fichier.
    Fichier(PathBuf),
}
```

Une pochette locale n'entre pas en mémoire. Un `folder.jpg` de trois
mégaoctets est banal sur un NAS, et le charger en RAM sur un Pi pour une image
que le navigateur cachera de son côté serait du gaspillage. La relecture à la
demande est un accès disque local, et l'ETag évite qu'elle se répète.

**Le téléchargement** ajoute `reqwest` au cœur (avec `rustls`, comme les
greffons), timeout de dix secondes, redirections suivies — Radio France en
fait une, cross-host. Trois garde-fous :

- **plafond de 2 Mo appliqué en lisant par morceaux.** Contrôler le
  `Content-Length` annoncé ne protège de rien : il est déclaratif.
- **refus si le `Content-Type` n'est pas `image/*`.**
- **le plafond ne s'applique pas au local.** Il protège d'un tiers sur le
  réseau ; un fichier du NAS de l'utilisateur est de confiance — mais ses
  octets d'en-tête sont vérifiés avant qu'il soit servi.

**Annulation.** Un téléchargement en vol est abandonné si l'identité change,
et son résultat écarté s'il arrive après. C'est le même garde-fou que l'écho
d'identité du texte, pour la même raison : sinon la pochette du morceau
précédent s'installe sur le suivant.

## L'arbitrage

Il n'y a plus de liste de priorités à tenir. Un contributeur qui **écrase**
remplace ce qui est là ; un contributeur qui **complète** ne touche qu'à une
case vide ; entre deux contributeurs qui écrasent la même case, l'ordre de
`plugins.toml` départage.

L'emplacement de la pochette vit dans `metadata.rs` et est vidé au changement
d'identité, comme l'ICY et les enrichissements.

Ce que cela donne dans les faits, pour un fichier du NAS :

| Ce qu'il y a | Ce qui s'affiche | Qui |
|---|---|---|
| un `folder.jpg` | le `folder.jpg` | `files` |
| pas de `folder.jpg`, une pochette embarquée | l'embarquée | le cœur, qui complète |
| ni l'un ni l'autre, des tags exploitables | Cover Art Archive | `musicbrainz`, qui complète |
| ni l'un ni l'autre, pas de tags | rien | — |

Et pour une radio : le greffon de métas écrase l'ICY et fournit l'image de la
station quand c'est un vrai morceau ; s'il n'en a pas, `musicbrainz` complète
depuis l'artiste et l'album qu'il vient de donner.

**Les deux inversions de convention que des versions antérieures de cette
conception traînaient ont disparu.** Le `folder.jpg` prime sur l'embarquée
parce que le cœur complète, pas parce qu'une règle est inversée ; et
l'embarquée prime sur `musicbrainz` parce que `musicbrainz` complète. Il n'y a
plus d'exception à documenter dans `docs/plugins.md` — seulement le modèle et
les deux intentions.

## Rendu dans la carte Player

La pochette prend place à gauche du bloc titre / artiste / album de
`PlayerCard.vue` : carrée, coins arrondis, **place réservée d'avance** pour
qu'elle n'introduise aucun saut de mise en page quand elle arrive — elle
arrive toujours après le texte, parfois plusieurs secondes après. Une icône de
repli occupe le carré quand il n'y a pas d'image.

`cover_origin` s'affiche comme le fait déjà `origin` pour le texte, dans le
même esprit et au même endroit.

Une clé i18n pour le texte alternatif, en anglais et en français : la parité
des catalogues est vérifiée par un test Rust.

## Ce qui ne change pas

- **Aucun genre de plugin ajouté**, et le multi-genre n'est pas utilisé
  (décisions 6 et 7). `files` reste une Source.
- **Le greffon `console` et les afficheurs texte.** Les champs sont
  additifs ; ils les ignorent.
- **Le nombre de greffons et de déclarations.** Aucun nouveau crate, aucune
  entrée de plus dans `plugins.toml` ni dans `deploy.sh`.
- **Le comportement des greffons existants.** Le défaut de `fill_only` est
  « écrase » (décision 4), donc les trois greffons livrés gardent exactement
  la préséance qu'ils ont aujourd'hui, sans qu'une ligne change chez eux pour
  cette raison.
- **La compatibilité des trames.** `known` et `fill_only` sont
  `#[serde(default)]` : un greffon qui les ignore fonctionne comme avant, ce
  qui permet de déployer la refonte greffon par greffon.

## Tests

- **`ritornello-proto`** : aller-retour JSON de `known`, de `CoverRef` sous
  ses deux formes, de `fill_only` et des deux champs de `Morceau` ;
  `cleaned()` rejette une URL non-`https`, une IP littérale, un chemin
  relatif, une extension non reconnue ; `is_empty()` reste vrai avec une
  pochette seule ; une trame sans pochette est identique à l'octet près à
  l'actuelle ; une trame sans `known` ni `fill_only` se relit, et
  `fill_only` y vaut faux.
- **Cœur, les étages** : l'état partiel diffusé contient ce que la Source puis
  mpv ont donné ; il n'est rediffusé que s'il a changé ; un contributeur qui
  ne complète rien ne provoque aucune rediffusion ; la séquence de complétion
  s'arrête d'elle-même.
- **Cœur, les intentions** : un enrichissement par défaut écrase un champ
  rempli ; un `fill_only` ne le touche pas ; un `fill_only` remplit bien une
  case vide ; entre deux qui écrasent, l'ordre de déclaration départage ; le
  cœur ne tente pas l'extraction de l'embarquée quand la case est déjà tenue.
- **Cœur, route et téléchargement** : 404 sur clé inconnue ; refus d'un
  `Content-Type` non-image ; refus d'un `Path` dont les octets d'en-tête ne
  sont pas ceux d'une image ; le plafond coupe un corps trop long **en cours
  de lecture** ; une pochette locale est servie sans passer par la mémoire.
- **`files`** : l'ordre de préférence ; la règle de l'image unique ;
  l'exclusion du `back.jpg` **seul** ; le sous-répertoire d'artwork ; le
  silence devant deux images non reconnaissables ; et la pochette part bien
  par une notification, après la réponse au `Play`.
- **`radiofrance-metas`** : la pochette est annoncée sur un `songUuid`
  renseigné et **se taise** sur un `songUuid` nul — les deux cas sont dans les
  fixtures du dépôt.
- **`ouifm-metas`** : `coverUrl` de la trame préféré, `coverId` composé à
  défaut, hôte inconnu refusé.
- **`musicbrainz`** : `parse_lookup` retient le MBID ; le chemin disque
  écrase ; le chemin générique déclare `fill_only` ; il part d'un artiste
  **et** d'un album, jamais d'un titre ICY seul ; il se tait quand
  `known.cover` est vrai.
- **Web** : la carte affiche la pochette, affiche le repli en son absence, et
  ne se réagence pas quand elle arrive.

## Ce qui reste hors périmètre

- Pas de cache disque (décision 14).
- Pas d'afficheur graphique — le champ est prêt, aucun matériel ne le
  consomme.
- Pas de redimensionnement côté appareil : on demande une taille bornée aux
  fournisseurs et on sert le local tel quel. Décoder et réencoder une image
  demanderait une dépendance lourde pour un Pi.
- Pas d'empreinte acoustique.
- Pas de découpage de l'ICY pour en tirer un artiste et un album (décision
  13). Une station qui n'émet qu'un ICY brut, sans greffon de métas pour le
  structurer, reste donc sans pochette.
- Pas de `folder.jpg` pour une future Source de fichiers autre que `files` :
  chaque Source déclare ses propres métadonnées. La pochette embarquée, lue
  par le cœur, sert n'importe quelle Source jouant un fichier.
- Pas de moyen pour une Source de déclarer que son ICY n'est pas fiable. Ce
  serait utile pendant le délai avant la première réponse d'un greffon de
  métas — le placeholder reste affiché jusque-là — mais c'est un sujet à part,
  qui concerne le texte et non l'image.

## Ce qui reste à mesurer sur le matériel

- **La stabilité des deux motifs d'URL de radio.** Ils ont été mesurés une
  fois, contre les services réels. Celui d'OUI FM vient de son propre lecteur
  et suivra ses évolutions ; celui de Radio France passe par une redirection
  dont la cible peut changer sans préavis. Un motif qui casse rend un silence,
  pas une erreur.
- **La taille réelle des `folder.jpg` du NAS**, qui décidera si servir le
  local sans plafond était le bon choix. La conception le suppose sans risque
  parce que ces octets ne transitent pas par la mémoire du cœur.
- **Le temps du `readdir` sur le partage SMB.** C'est ce qui a fait choisir la
  notification plutôt qu'une réponse jointe au `Play` (décision 7) ; reste à
  voir si le délai se remarque à l'écran.
- **Le nombre de tours de complétion en usage réel.** La borne est
  théorique ; ce qui compte à l'écran est de ne pas voir le texte changer
  trois fois au démarrage d'une piste. Si cela se voit, le remède est de
  retarder la première publication, pas de retoucher les étages.
