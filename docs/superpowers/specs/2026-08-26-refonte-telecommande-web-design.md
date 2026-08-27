# Refonte visuelle de la télécommande web

**Date :** 2026-08-26
**État :** validé en conversation par le propriétaire, en attente de sa relecture
écrite avant le plan d'implémentation
**Base :** `main` à `f869df8`.
**Maquettes :** canevas « Télécommande Ritornello »
(https://claude.ai/code/artifact/ba6fbc81-c72e-4130-9111-3ae262610a0d) —
planches *A — Écoute* (téléphone) et *C — Tableau de bord* (PC) retenues,
*B — Poste* écartée.

Ce chantier ne change **aucun protocole ni aucune charge utile existante**, et
n'ajoute qu'une route de lecture (`GET /api/presets`, décision 6) : tout ce
qu'il affiche est déjà dans la trame SSE de `/api/player` (`title`, `artist`,
`album`, `cover_href`, `position_s`, `duration_s`, `seekable`, `playback`,
`preset`, `preset_name`, `preset_count`) ou déjà tenu par le cœur. Il touche
`web/app/src`, `web/kit` pour un composant partagé, et `status.rs` pour la
route.

## Le problème

La page d'accueil est un formulaire, pas une télécommande. Mesuré sur l'écran
actuel (capture du 2026-08-26, cœur e2e) :

- l'état est du texte brut « clé : valeur » (« Volume : 60 % ») ;
- les commandes sont quatre rangées de boutons `outline` à libellés
  (« Lecture/Pause », « Volume − »…), sans hiérarchie : la lecture pèse
  autant qu'Éjecter ;
- la pochette et le morceau sont relégués **sous** trois lignes de texte,
  alors que c'est la seule chose qu'on regarde depuis le canapé ;
- les présélections sont des numéros anonymes alors que la source déclare
  leur nom (`preset_name` pour celle qui joue ; la liste complète est servie
  aux pages d'admin) ;
- la navigation du haut a 2 entrées fixes + 1 par greffon à page d'admin :
  sur 390 px, « Generic-input » passe déjà sur deux lignes, et un greffon de
  plus déborde ;
- rien n'est pensé pour le doigt : la barre de progression fait 6 px de haut
  et ne connaît que `@click`.

## Ce qui ne change pas

- **Le thème.** L'utilisateur choisit parmi 42 palettes tweakcn, clair ou
  sombre. La refonte n'emploie que les jetons (`primary`, `card`, `muted`,
  `border`, `muted-foreground`…). Aucune couleur en dur, aucune police, aucun
  rayon hors `--radius`. Le seul jeton dont la distinction est garantie dans
  les 42 palettes reste `destructive` (cf. `SystemView.vue`).
- **Le kit.** Card, Button, Badge… de `@ritornello/ui` restent la base ; les
  pages d'admin des greffons ne sont pas touchées (elles ont leur propre
  chantier, cf. la dette notée dans `docs/development.md`).
- **La sémantique des commandes** (`remoteCommands.ts`) : mêmes `cmd`, mêmes
  règle de grisage en veille, même bascule Muet, même fenêtre de
  présélections. Ce qui sort de la page (±10 s, Volume −/+, décisions 3 et 5)
  sort avec sa mécanique, sans laisser d'entrée morte dans `REMOTE_ROWS`.
- **Les marqueurs de test** (`data-player`, `data-volume`, `data-pochette`,
  `data-now-playing`, `data-progression`…) : les e2e existants doivent
  continuer de passer, ou changer pour une raison dite.
- **L'ordre des greffons** : celui de `/api/status`, donc de `plugins.toml`.
  Pas de nouvelle notion de priorité.

## Décisions

### 1. Une seule page, deux mises en page selon la largeur

Une seule `HomeView`, un seul flux SSE, un seul jeu de composants ; la
différence téléphone/PC se fait par les points de rupture Tailwind, pas par
deux vues.

- **Sous `md` (téléphone) — planche A.** Une colonne : pochette centrée
  (232 px), bloc morceau centré (surligne `P1 · FIP` en `primary`, titre
  `text-xl font-semibold`, artiste, album en `muted-foreground`), barre de
  progression, transport, volume, présélections en tuiles, barre d'onglets
  basse.
- **À partir de `md` (PC) — planche C.** Deux cartes côte à côte dans
  `max-w-5xl` : à gauche la carte Lecteur (pochette 176 px à gauche du bloc
  morceau, progression, transport, volume), à droite la carte Présélections.
  La nav reste en haut, telle qu'aujourd'hui.

### 2. La pochette et le morceau deviennent le sujet

- La carte Lecteur ne montre plus « Source active : … », « Présélection : … »,
  « Volume : … » en lignes de texte. La source devient une **pastille** dans
  l'en-tête (icône + nom, `bg-muted`) ; la présélection devient la surligne
  `P{n} · {preset_name}` au-dessus du titre ; le volume est **le curseur**.
- Quand rien n'est connu du morceau (`riendAfficher(etat)`), la pochette de
  repli (carré `bg-muted`, glyphe note en SVG) reste en place et le bloc
  morceau montre le `status` de la source s'il existe (« PAS DE DISQUE »),
  sinon le nom de la présélection seul. Le carré ne disparaît jamais : c'est
  lui qui tient la mise en page.
- Le badge « VEILLE » reste ; en veille la pochette passe en `opacity-50` et
  le transport est grisé comme aujourd'hui.
- Les badges d'origine des métadonnées et de la pochette (chantier pochettes)
  sont conservés, en `text-xs` sous l'album.

### 3. Le transport : trois touches et un bouton de lecture dominant

Ordre : **|◀ · ▶/❚❚ · ▶|**, plus **■** et **⏏** en retrait (à droite sur PC,
en fin de rangée sur téléphone).

- Lecture/Pause est le seul bouton plein `bg-primary`, rond, 72 px sur
  téléphone, 56 × 40 px sur PC. Son icône suit `playback` (▶ à l'arrêt/pause,
  ❚❚ en lecture) — le champ existe et n'était pas lu.
- **Les touches Reculer/Avancer (±`seek_step_s`) disparaissent de la page.**
  Décidé par le propriétaire au vu de VLC, Deezer et WMP : aucun n'a ces
  touches, c'est la barre d'avancement qui fait le travail. La commande
  `SeekBackward`/`SeekForward` reste dans le protocole et sur la télécommande
  physique ; sur le web elle ne survit qu'au clavier (flèches sur la barre,
  décision 4). `REMOTE_ROWS` perd ces deux entrées et le test qui vérifiait
  leur grisage est remplacé par un test qui vérifie leur absence.
- Éjecter est **masqué** si `!can_eject`. Vérifié dans le cœur : `can_eject`
  est une capacité que le greffon source déclare pour lui-même (le cd la
  déclare `true` disque ou pas, la radio ne la déclare pas), remise à `false`
  en veille — la masquer ne cache donc jamais un lecteur qui existe. Sur la
  radio, la rangée compte quatre touches ; sur le cd, cinq. Stop reste
  toujours visible.

### 4. La barre de progression devient un vrai curseur tactile

Condition posée par le propriétaire à la suppression des touches ±10 s.

- **Zone de contact ≥ 44 px** de haut autour d'une piste visuelle de 6 px
  (le padding porte la zone, pas la piste).
- **Glisser au pointeur** (`pointerdown` → `setPointerCapture` →
  `pointermove` → `pointerup`), `touch-none` pendant le glisser pour que la
  page ne défile pas. Pendant le glisser, le remplissage et le temps écoulé
  suivent le doigt **localement** ; **un seul `SeekTo`** part au relâchement.
  Le clic simple reste un `SeekTo` immédiat.
- Après le relâchement, la barre garde la valeur visée jusqu'à la trame SSE
  suivante dont la position la rejoint (à un pas près), pour éviter le
  retour en arrière d'un instant qu'on voit sur les lecteurs naïfs.
- Le clavier ne change pas (flèches = `seek_step_s`, Home/End) ; `role="slider"`,
  `aria-valuenow/min/max` et `aria-valuetext` (« 2 min 7 s ») restent.
- Non `seekable` : pas de poignée, pas de curseur, pas de `role`, `cursor`
  par défaut — la barre reste informative.

### 5. Le volume : un curseur, le Muet à son bout

- Un curseur horizontal 0–100 (icône haut-parleur à gauche, valeur `60 %` à
  droite). Même mécanique de glisser que la barre de progression (composant
  commun, décision 8), un seul `SetVolume` au relâchement ; l'appui maintenu
  sur les touches physiques n'est pas concerné.
- L'icône haut-parleur **est** le bouton Muet (bascule, `aria-pressed`) ; muet
  actif = icône barrée + piste en `opacity-60`, la valeur reste lisible
  (c'est celle qui revient au rétablissement, comme aujourd'hui).
- Les touches Volume −/+ **disparaissent sur les deux largeurs** : elles
  restent le geste de la télécommande physique (`VolumeUp`/`VolumeDown` avec
  appui maintenu). Côté accessibilité elles n'apportent rien : le curseur est
  un `role="slider"` piloté au clavier (flèches = 1 %, Page ↑/↓ = 10 %,
  Début/Fin) et annoncé avec sa valeur par les lecteurs d'écran, ce qui est
  la forme attendue d'un réglage continu. `REMOTE_ROWS` perd donc aussi ces
  deux entrées et la mécanique d'appui maintenu (`pointerdown` cadencé par
  `/api/settings`) quitte la page web ; le test qui la couvrait disparaît
  avec elle.

  Le protocole a déjà `SetVolume(u8)` (`ritornello-proto/src/command.rs`,
  sérialisé `{"cmd":"SetVolume","arg":40}`) ; la page n'envoyait jusqu'ici
  que `VolumeUp`/`VolumeDown`. Aucun changement du cœur.

### 6. Les présélections : des tuiles nommées

- Une tuile = numéro (gras, `muted-foreground`) + nom, hauteur 56 px sur
  téléphone / 48 px sur PC, `bg-card border`, celle qui joue en `bg-primary
  text-primary-foreground` avec un point à droite.
- **Le nom vient du cœur, qui l'a déjà.** Depuis le chantier serveur MPD, le
  cœur tient `presets_par_source` (les `Preset` nommés que chaque source
  déclare dans sa trame `presets`) et le diffuse aux afficheurs sous forme de
  `Catalogue { sources: [{ name, presets }] }` (`core.rs`, `catalogue()`),
  dans l'ordre de `SourceCycle`. Aucune route HTTP ne le sert. Le chantier
  ajoute **`GET /api/presets`** qui renvoie ce `Catalogue` tel quel — une
  route de lecture, aucun changement de protocole ni de charge utile
  existante. La page le charge au montage et le recharge quand la source
  active change (la trame SSE le dit) ; une source dont la liste est vide
  (cd, entrée aux) montre des tuiles à numéro seul, comme aujourd'hui.
  Écartée : lire les API d'admin de chaque greffon (formats divergents, et
  le cœur a déjà la réponse).
- Grille : 2 colonnes sur téléphone, 2 sur PC dans la demi-largeur, fenêtre
  de 6 (téléphone) ou 10 (PC) avec la pagination `‹ 1–6 ›` existante
  (`preset_count`, bornes sans rebouclage). L'en-tête dit « Présélections ·
  12 stations » (ou « 12 pistes » — le mot vient du catalogue, pas de la
  page, cf. i18n).
- `preset_count === null` → les 9 touches nues historiques, inchangé.
  `preset_count === 0` → la carte dit le `status` (« PAS DE DISQUE ») et
  aucune tuile.

### 7. La navigation : fixe en bas sur téléphone, inchangée sur PC

- **Téléphone :** barre d'onglets basse **à 4 entrées fixes**, quel que soit
  le nombre de greffons : **Écoute · Greffons · Système · Réglages**, icônes
  SVG au trait + libellé `text-[11px]`, onglet actif en `primary`, hauteur
  56 px + `safe-area-inset-bottom`. La nav du haut disparaît sous `md` ; la
  marque « Ritornello » + la bascule de thème restent en en-tête.
- **« Greffons »** regroupe la partie variable : les pages d'admin des greffons
  (radio, files, generic-input, mpd, métadonnées demain…). Le mot est celui
  de la carte « Greffons » de Configuration ; « Sources » était faux puisque
  la télécommande IR ou un greffon de métadonnées y figurent aussi. L'onglet
  ouvre une **liste** (une route `/plugins/`, pas un menu volant : lisible,
  testable, adressable) des greffons à page d'admin, dans l'ordre de
  `/api/status`, chacun avec son nom `first-letter:uppercase` et son état
  (connecté / inactif, mêmes badges que Configuration). Un seul greffon →
  l'onglet mène directement à sa page, sans liste intermédiaire.
- **PC :** liens en haut, inchangés (Configuration, Système, un lien par
  greffon) ; la route `/plugins/` existe aussi mais rien n'y mène depuis la nav.
- « Réglages » = `/config` : on ne renomme pas la route, seulement le libellé
  de l'onglet, qui a 8 caractères de large.

### 8. Un composant de curseur partagé, dans le kit

La barre de progression et le volume ont la même mécanique (glisser au
pointeur, zone 44 px, valeur locale pendant le glisser, une commande au
relâchement, clavier, `role="slider"`). Elle vit dans **un** composant
`Slider` du kit (`web/kit/src/components/ui/slider`), sur la primitive
`SliderRoot` de reka-ui 2.10 (déjà une dépendance ; vérifié : elle gère le
pointeur, le clavier, `role="slider"` et l'ARIA, et émet `valueCommit` au
relâchement — c'est là que part la commande, `update:modelValue` ne servant
qu'au rendu local). La zone de 44 px est le padding du `SliderRoot`, la
piste visuelle reste à 6 px. `BarreProgression`
et le volume l'habillent. `UI_CONTRACT` n'a pas à bouger : un composant
ajouté ne casse aucune page de greffon.

### 9. Les icônes

`@radix-icons/vue` est déjà une dépendance (bascule de thème). On l'emploie
pour tout le transport, le volume, la nav et la source ; les glyphes Unicode
(`♫ ‹ › ▲ ▼ ✕`) de la page d'accueil disparaissent. Les pages d'admin gardent
les leurs (hors périmètre). Si une icône manque à Radix (haut-parleur barré,
éjecter), SVG au trait dessiné dans le même style, 15 px de grille.

## i18n

Nouvelles clés (en + fr), dans le catalogue du shell : `nav_listen`,
`nav_plugins`, `nav_settings` (le libellé court), `presets_count_stations`,
`presets_count_tracks` (le mot pluriel derrière le nombre, choisi par le
`kind` que la source déclare — sinon `presets_count_generic`),
`plugins_list_title`, `volume_slider_label`, `progress_slider_label`.
Clés retirées : `remote_seek_back`, `remote_seek_forward`. Le test de parité
en/fr existant fait foi ; le test « aucune clé n'atteint l'écran » aussi.

## Tests

- **Unitaires (vitest)** : `Slider` (glisser émet une seule valeur au
  relâchement ; clavier ; non déplaçable = pas de rôle) ; `HomeView` (deux
  mises en page rendent les mêmes marqueurs ; ±10 s absents ; icône de
  lecture suit `playback` ; tuiles nommées quand la liste est là, numéros
  seuls sinon ; nav basse à 4 entrées avec 0, 1 et 3 greffons ; 1 greffon →
  lien direct).
- **e2e (Playwright)** : le parcours existant est rejoué ; on y ajoute une
  vérification au **viewport téléphone** (`devices['Pixel 7']` ou équivalent)
  : barre basse visible, nav du haut absente, glisser sur la barre de
  progression envoie un `SeekTo` (observable par la position rendue ensuite),
  la tuile de la présélection en cours porte `aria-current`.
- **Visuel** : les captures `docs/captures/*.png` sont obsolètes depuis
  plusieurs chantiers ; elles sont **regénérées** par un script Playwright
  (`web/app/scripts/captures.mjs`, clair + sombre, PC + téléphone) plutôt
  qu'à la main, et `docs/interface.md` est mis à jour pour décrire la nouvelle
  page.

## Hors périmètre

Les pages d'admin des greffons (tables HTML, glyphes Unicode) ; la page
Système ; la page Configuration (hors libellé d'onglet) ; le thème et ses
palettes ; tout changement de protocole. Côté cœur, **une seule** addition,
la route de lecture `GET /api/presets` (décision 6), dans un commit séparé
avec son test axum.

## Risques

- **Fraîcheur de `/api/presets`** : la liste est rechargée au changement de
  source, pas à chaque modification de `stations.toml` par la page d'admin.
  Acceptable au premier jet (on revient rarement de l'admin sans changer de
  page) ; si ça gêne, le cœur pourra pousser un événement `presets` sur le
  SSE existant — hors périmètre ici.
- **Les e2e sur téléphone** doublent la durée du parcours ; le harnais e2e
  démarre un cœur dans WSL (60 s) et n'a qu'un worker — accepter, ou
  restreindre la partie téléphone à un second `project` Playwright.
- **`safe-area-inset-bottom`** ne se voit que sur un vrai téléphone en mode
  plein écran ; à vérifier sur l'appareil, pas dans Playwright.
