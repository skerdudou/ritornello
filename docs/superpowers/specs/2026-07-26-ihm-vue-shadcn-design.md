# ritornello — IHM Vue 3 / shadcn-vue, thèmes tweakcn

Remplacer les trois surfaces HTML actuelles par une **SPA unique** Vue 3 +
shadcn-vue, servie par le cœur en Rust (aucun serveur dédié), avec bascule
clair/sombre et sélecteur des **42 thèmes tweakcn**. Les IHM d'admin des
plugins deviennent des **modules ESM chargés à la demande**, livrés par les
plugins eux-mêmes : le cœur continue de ne rien connaître d'eux.

Date : 2026-07-26 — Statut : validé

## Contexte

Trois surfaces d'IHM existent aujourd'hui, toutes en HTML/JS écrit à la main,
sans aucun CSS partagé :

| Surface | Source | Contenu |
|---|---|---|
| `/status` | `ritornello-core/src/status.rs`, HTML généré par `format!` | table des plugins, sortie audio, langue, télécommande (21 boutons), 50 dernières lignes de log |
| `/plugins/radio/` | `ritornello-plugin-radio/src/index.html` (187 l.) | table des stations (numérotation automatique, 9 max), recherche dans l'annuaire Radio Browser, ajout d'un résultat |
| `/plugins/generic-input/` | `ritornello-plugin-generic-input/src/index.html` (295 l.) | sélection du périphérique, 21 actions, apprentissage de touche, presets livrés, import/export TOML |

Deux mécanismes structurent l'existant et déterminent ce qui suit :

- **Les pages de plugin ne sont pas des fichiers servis** : le plugin renvoie
  au cœur le HTML complet par IPC (`AdminReq::GetPage` →
  `include_str!("index.html")`, voir `2026-07-23-serveur-web-unique-design.md`).
  Le cœur relaie sans rien interpréter. Une SPA compilée ne peut pas voyager
  ainsi : c'est le point que cette spec doit résoudre sans casser le
  découplage.
- **L'i18n est une substitution de jetons côté Rust** : chaque page contient
  des `{{clé}}` remplacés par le catalogue du composant avant l'envoi
  (`2026-07-23-i18n-design.md`). Un bundle JS compilé et hashé ne se prête pas
  à cette substitution.

Ce second mécanisme a déjà coûté un défaut **Critical** (addendum du
`revue-consolidee-2026-07-23.md`, corrigé en `dbfa771`) : une apostrophe droite
dans une valeur française était injectée telle quelle dans un littéral JS
`'{{clé}}'`, cassant la syntaxe et faisant sauter **tout** le script de la page,
en français seulement. Le correctif est un garde-fou qui **interdit** désormais
`'`, `"`, `\` et le retour à la ligne dans toute valeur de traduction des deux
plugins. Le follow-up ouvert — « échappement structurel des jetons injectés en
JS » — est **refermé par cette spec** : il n'y a plus aucune substitution dans
du source, donc plus de classe d'erreur à échapper (voir
« Internationalisation »).

Contraintes de plate-forme inchangées : cible de référence Raspberry Pi 2
(ARMv7, 1 Go), cross-compilation par `cross` dans une image Docker **sans
Node**, un seul port et une seule origine (`:8080`), aucune authentification
(réseau de confiance).

Le plan `2026-07-23-annuaire-radio.md` est **terminé** (commit `69e9090`) :
aucune contrainte de séquencement ne pèse plus sur ce chantier, et la vue
radio à produire est celle décrite ci-dessus, recherche annuaire incluse.

## Décisions de cadrage

| Sujet | Décision |
|---|---|
| Périmètre fonctionnel | **Iso-fonctionnel.** Aucune fonction métier ajoutée ni retirée. Seule réorganisation : `/` (aujourd'hui 404) devient l'accueil. |
| IHM des plugins | **Modules ESM chargés à la demande**, livrés par le plugin, transportés par le protocole admin. Le cœur et la SPA ne contiennent **aucun nom de plugin**. |
| Partage du socle | Le cœur expose `vue` **et** un kit UI complet (`@ritornello/ui`) via une **import map**. Une seule instance de Vue, un seul jeu de composants shadcn. |
| Chaîne de build | Séquence unique : `npm` → `cargo build` x86 → `cross build` ARM. Le npm ne tourne **qu'une fois** ; les deux cargo consomment le même livrable. |
| Livrables JS | **Gitignorés**, régénérés par la chaîne, embarqués dans les binaires. Un `build.rs` dépose un bouchon si absents, pour que `cargo test` reste vert sans Node. |
| Thème | Réglage **de l'appareil** : persisté dans `state.json` comme la locale. Défaut : preset `northern-lights`, mode **clair**. Pas de mode `system`. |
| Polices des thèmes | Chargées depuis un **CDN**, injectées à l'application du thème, repli `system-ui`. Seule ressource externe de l'IHM. |
| i18n | La substitution `{{clé}}` disparaît. Les catalogues sont **exposés en JSON** ; les packs TOML externes restent l'unique source de vérité. |
| Authentification | Inchangée : aucune. |

## Arborescence

Un espace de travail npm à la racine, deux paquets de socle, et un paquet
d'IHM par plugin qui en a une :

```
package.json                 espace de travail npm (workspaces)
web/kit/                     @ritornello/ui  — composants, composables, contrat
web/app/                     le shell — index.html, routage, accueil, /status
crates/ritornello-plugin-radio/ui/            module IHM du plugin radio
crates/ritornello-plugin-generic-input/ui/    module IHM du plugin generic-input
```

`npm ci && npm run build --workspaces` construit les quatre paquets dans le bon
ordre (le kit avant ses consommateurs).

**Pile technique** : Vue 3 (Composition API, `<script setup>`), TypeScript,
Vite, Tailwind CSS v4 (configuration CSS-first), shadcn-vue (sur `reka-ui`),
`vue-router`. Pas de Pinia : deux composables suffisent à l'état partagé
(thème, catalogue i18n). Pas de `vue-i18n` : le catalogue est plat.

## Chaîne de build

`deploy/build.sh` (nouveau) enchaîne toujours les trois étapes, dans cet ordre :

1. `npm ci && npm run build --workspaces` — produit les bundles et les dépose
   aux emplacements embarqués ci-dessous.
2. `cargo build --workspace` — cible native x86_64, utilisée pour les tests.
3. `cross build --release --workspace --target armv7-unknown-linux-gnueabihf`.

Le npm **ne tourne pas** à l'étape 3 : le livrable produit à l'étape 1 est déjà
sur le disque, `include_str!` le lit à la compilation, quelle que soit la cible.
C'est ce qui permet à `cross` de fonctionner avec une image Docker sans Node.

| Sortie du build npm | Embarquée dans |
|---|---|
| `web/app/dist/index.html` | `ritornello-core` |
| `web/app/dist/assets/app-<hash>.js`, `app-<hash>.css` | `ritornello-core` |
| `web/app/dist/assets/vue.js`, `assets/ui-kit.js` (noms **stables**) | `ritornello-core` |
| `crates/<plugin>/ui/dist/ui.js`, `ui.css` | le plugin concerné |

Il n'y a **pas** de `ui-kit.css` séparé : les classes des composants du kit sont
produites par la passe Tailwind du shell (voir « CSS » plus bas) et vivent donc
dans `app-<hash>.css`.

Les `dist/` sont ajoutés au `.gitignore` : la chaîne les régénère toujours, et
rien ne peut donc être commité de périmé.

**Mécanisme d'embarquement.** Le cœur embarque un **répertoire** dont les noms
de fichiers sont hashés, donc inconnus à l'écriture du code : il utilise
`rust-embed` (ou `include_dir`), qui expose l'arborescence et permet de servir
`/assets/*` par recherche de chemin. Chaque plugin, lui, n'embarque que **deux
fichiers à noms fixes** (`ui.js`, `ui.css`) : un `include_str!` suffit.

**Bouchon de compilation.** `include_str!` sur un fichier absent est une erreur
de compilation : un clone frais, ou un `cargo test` lancé sans avoir construit
l'IHM, casserait. Chaque crate qui embarque un livrable reçoit un `build.rs`
qui, si le `dist/` attendu manque, y écrit un bouchon minimal — un
`index.html` (ou `ui.js`) qui affiche « interface non construite : exécuter
`npm run build --workspaces` ». Conséquence : `cargo build` et `cargo test`
réussissent partout, sans Node, et le message est explicite au lieu d'être une
erreur de macro. Le `build.rs` déclare `cargo::rerun-if-changed` sur le
répertoire `dist/`.

## Partage de Vue et du kit

Le shell déclare une import map dans `index.html` :

```html
<script type="importmap">
{ "imports": { "vue": "/assets/vue.js", "@ritornello/ui": "/assets/ui-kit.js" } }
</script>
```

Les **quatre** bundles (`app`, `ui-kit`, et le `ui.js` de chaque plugin)
marquent `vue` et `@ritornello/ui` comme **externes** dans leur configuration
Vite. Le navigateur résout ces spécificateurs par l'import map : tout le monde
reçoit la même copie. Sans cette externalisation côté `app`, le shell
embarquerait son propre Vue et deux instances coexisteraient — réactivité et
`provide`/`inject` cloisonnés.

Ces deux modules gardent un **nom stable, sans hash** : ils constituent le
contrat contre lequel les plugins sont compilés, et une URL hashée changerait à
chaque build du cœur. Ils sont servis avec un `ETag` (revalidation), alors que
les chunks de l'app, hashés, sont servis en `immutable`.

**Version de contrat.** `@ritornello/ui` exporte `UI_CONTRACT: number`. Chaque
module de plugin exporte `contract: number` à côté de son composant. La SPA
compare avant de monter :

```ts
// web/kit/contract.ts
export const UI_CONTRACT = 1;

// crates/<plugin>/ui/src/index.ts
export const contract = 1;
export default defineComponent({ /* … */ });
```

En cas d'écart, la vue affiche un message traduit indiquant que le plugin doit
être reconstruit, au lieu d'un écran blanc ou d'une exception non rattrapée.
Toute modification incompatible du kit incrémente `UI_CONTRACT`.

**CSS.** Tailwind ne génère que les classes qu'il rencontre à la compilation.
Le CSS du shell est produit par une passe qui balaie `web/app` **et**
`web/kit` : il contient donc les classes des composants du kit, y compris quand
ce sont des vues de plugin qui les utilisent. Chaque plugin fait en plus **sa
propre passe Tailwind** sur ses sources et livre son `ui.css`, pour les classes
qu'il est seul à employer. Les deux CSS consomment les mêmes variables de
thème : rien à faire côté plugin pour être thémé.

**Contenu du kit** (`@ritornello/ui`) : `Button`, `Input`, `Label`, `Select`,
`Table` (et ses sous-composants), `Card`, `Dialog`, `Switch`, `Badge`,
`ScrollArea`, `Sonner` (notifications, qui remplacent les `<span id="msg">`
actuels) ; le helper `cn()` ; les composables `useTheme()`, `useCatalog()` /
`t()`, et `api` (client `fetch` typé : `get`, `put`, `post`, avec extraction du
message d'erreur des réponses 422).

**Un plugin n'a pas besoin de Node.** L'ESM natif ne demande aucune
compilation : un plugin simple peut livrer un `ui.js` **écrit à la main**
(`import { ref } from 'vue'; export const contract = 1; export default { setup() {…}, template: '…' }`)
et ne dépendre que des classes du kit. Les deux plugins livrés utilisent un
build Vite pour bénéficier des `.vue` et de TypeScript ; c'est un choix de
confort, pas une obligation d'architecture.

## Protocole admin

`ritornello-proto/src/admin.rs` : `GetPage` disparaît au profit du transport
d'actifs opaques, et une requête de catalogue est ajoutée.

```rust
#[serde(tag = "req", content = "arg")]
pub enum AdminReq {
    GetAsset(String),   // "ui.js" | "ui.css" — chemin opaque pour le cœur
    GetCatalog,         // catalogue i18n du plugin, langue courante
    GetData,            // inchangé
    SetData(serde_json::Value),  // inchangé
}

#[serde(tag = "kind", content = "data")]
pub enum AdminResult {
    Asset { mime: String, body: Option<String> },  // None → 404
    Catalog(serde_json::Value),
    Data(serde_json::Value),
    Set { ok: bool, error: Option<String> },
}
```

Le cœur reste aveugle : il relaie une chaîne dont il ignore le sens, comme il
le fait déjà pour `GetData`. Le `mime` est fourni par le plugin (le cœur ne
déduit rien d'une extension). `body: None` est la réponse normale à un chemin
inconnu, traduite en `404` par le cœur.

**SDK** (`ritornello-plugin-sdk`) :

```rust
#[async_trait]
pub trait AdminPlugin: Send + 'static {
    fn asset(&self, path: &str) -> Option<(String, String)>;   // (mime, corps)
    fn catalog(&self) -> serde_json::Value;                     // carte plate clé → texte
    async fn get_data(&self) -> serde_json::Value;              // inchangé
    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String>;  // inchangé
}
```

`AdminClient` gagne `get_asset(path)` et `get_catalog()`, sur le modèle exact
de `get_data`. La forme de `run_admin_plugin` (lier, accepter, boucler) ne
change pas.

**Mise en cache côté cœur.** Un `ui.js`/`ui.css` est immuable pour la durée de
vie du processus du plugin. Le cœur le récupère au **premier** accès, le garde
en mémoire (quelques dizaines de Ko par plugin) et le sert ensuite avec un
`ETag` dérivé de son empreinte — pas d'aller-retour IPC à chaque rechargement
de page, et un `304` sur revalidation.

## Routes du cœur

Nouvelles :

| Route | Rôle |
|---|---|
| `GET /` | le shell de la SPA |
| repli SPA | `GET` d'un chemin non résolu sert le shell (`/status`, `/plugins/<nom>/`…) — les URL existantes continuent de répondre |
| `GET /assets/*` | actifs embarqués du shell (`immutable` si hashé, `ETag` sinon) |
| `GET /plugins/{nom}/ui.js`, `/ui.css` | `GetAsset`, mis en cache + `ETag` |
| `GET /plugins/{nom}/api/i18n` | `GetCatalog` → JSON |
| `GET /api/i18n` | catalogue du cœur → JSON |
| `GET /api/logs` | les 50 dernières lignes WARN/ERROR, les plus récentes en premier |
| `GET`/`PUT /api/theme` | `{ theme, mode }` |

`/api/logs` est la seule route de données réellement **nouvelle** : les
dernières lignes de log n'existaient que dans le HTML rendu par le cœur, donc
sans page rendue il n'y avait plus aucun moyen de les lire. Le tampon
circulaire (`LogBuffer`, 50 lignes) et l'ordre d'affichage sont ceux
d'aujourd'hui.

Inchangées : `/api/status`, `/api/audio-output`, `/api/locale`, `/api/command`,
`GET`/`PUT /plugins/{nom}/api/data`. Un `{nom}` inconnu ou sans capacité admin
répond `404`, comme aujourd'hui.

**Portée exacte du repli.** Il ne doit jamais avaler une route de données : une
faute de frappe sur `/api/statuss` répondant le shell en `200` serait un piège à
débogage. Le repli est donc restreint aux `GET` dont le chemin ne commence pas
par `/api/`, `/assets/`, et ne correspond ni à `/plugins/*/api/*` ni à
`/plugins/*/ui.*` — tout le reste de ces espaces répond `404`. Les méthodes
autres que `GET` ne sont jamais servies par le repli.

**Découverte des plugins.** La SPA construit sa navigation à partir de
`/api/status`, qui liste déjà chaque plugin avec son drapeau `admin`. Pour
chaque plugin `admin = true`, la route `/plugins/<nom>` charge
`import('/plugins/<nom>/ui.js')` et injecte `/plugins/<nom>/ui.css`. Aucun nom
de plugin n'apparaît donc ni dans le cœur, ni dans la SPA : un plugin tiers qui
déclare `admin = true` et livre un `ui.js` voit son IHM apparaître sans qu'une
ligne du cœur change. Un plugin déclaré `admin` dont le `ui.js` est absent ou
illisible affiche un message d'indisponibilité, cohérent avec la tolérance
existante à la mort d'un plugin.

`status.rs` perd `status_page`, `escape_html` et la génération des boutons de
télécommande (~130 lignes de `format!`) ; il ne garde que l'état, les routes
JSON et le `LogBuffer`.

## Moteur de thèmes

**Source des presets.** Les 42 presets de [tweakcn](https://tweakcn.com)
(dépôt `jnsahaj/tweakcn`, **Apache-2.0**) sont convertis en
`web/kit/themes/presets.json` par `web/kit/scripts/fetch-presets.mjs`, lancé à
la main lors d'une mise à jour. Ce JSON est **commité** (c'est une source, pas
un livrable de build) : ~110 Ko brut, ~12 Ko gzip, importé statiquement et
appliqué avant le montage. Le fichier porte l'attribution et la mention de
licence.

Forme d'un preset, telle qu'elle vient de l'amont :

```json
{ "northern-lights": {
    "label": "Northern Lights",
    "styles": {
      "light": { "background": "#f9f9fa", "primary": "#34a85a", "radius": "0.5rem",
                 "font-sans": "Plus Jakarta Sans, sans-serif", "…": "…" },
      "dark":  { "background": "#1a1d23", "primary": "#34a85a", "…": "…" } } } }
```

**Application.** `applyTheme(preset, mode)` calcule
`{ ...styles.light, ...styles[mode] }` — le bloc clair sert de base, le bloc du
mode surcharge, car les blocs `dark` de l'amont omettent souvent les clés non
chromatiques (polices, rayon) — puis écrit chaque entrée en variable CSS sur
`document.documentElement` (`--background`, `--primary`, `--radius`,
`--font-sans`…).

L'itération est **générique** : aucune liste de clés en dur. Les presets n'ont
pas exactement le même jeu de clés (ombres, polices, variables de barre
latérale), et un preset ajouté en amont doit fonctionner sans toucher au code.

**Polices.** Les familles citées par les presets (une trentaine sur les 42, du
type « Plus Jakarta Sans », « JetBrains Mono ») sont chargées par un `<link>`
CDN injecté/remplacé à chaque application de thème. Toutes les déclarations
`font-*` reçoivent `system-ui`/`monospace` en fin de pile : hors ligne, l'IHM
reste parfaitement lisible dans la police système. C'est la seule ressource
externe de l'IHM.

**Persistance.** `PersistedState` gagne deux champs, en `#[serde(default)]`
comme `locale` :

```rust
pub struct PersistedState {
    // … active_source, volume, audio_device, locale
    #[serde(default)] pub theme: Option<String>,  // défaut : "northern-lights"
    #[serde(default)] pub mode: Option<String>,   // "light" | "dark", défaut : "light"
}
```

`GET`/`PUT /api/theme` lit et écrit ces champs, sur le modèle exact de
`/api/locale` (mise à jour de l'état partagé puis persistance). Un preset ou un
mode inconnu en `PUT` répond `422` : le cœur valide la **forme** (`mode` parmi
deux valeurs, `theme` non vide) sans connaître la liste des presets, qui vit
côté SPA.

**Pas de clignotement.** Le cœur injecte le choix courant dans le shell servi :

```html
<script>window.__RITORNELLO_THEME__ = {"theme":"northern-lights","mode":"light"};</script>
```

La SPA applique donc le bon thème dès le premier rendu, sans attendre
`GET /api/theme`. Le cœur ne transporte que deux chaînes : il ne connaît
toujours aucune couleur.

**IHM.** Dans l'en-tête du shell : un **toggle** clair/sombre, et un bouton qui
ouvre la **popin** (`Dialog`) de sélection. La popin présente les 42 thèmes en
grille, chaque carte montrant le libellé et quatre pastilles
(`background`, `primary`, `secondary`, `accent`) rendues dans le mode courant,
avec un champ de filtre par nom et l'indication du thème actif. Le choix
s'applique immédiatement et part en `PUT /api/theme`.

## Internationalisation

La substitution `{{clé}}` côté Rust est supprimée. Disparaissent avec elle,
dans les deux plugins :

- les constantes `PAGE_KEYS` et les tests qui vérifiaient qu'aucun jeton
  `{{…}}` ne survivait au rendu — il n'y a plus de rendu ;
- le garde-fou `aucune_valeur_ne_contient_un_caractere_dangereux_pour_la_substitution`,
  qui interdit aujourd'hui `'`, `"`, `\` et le retour à la ligne dans les
  valeurs de traduction. Un catalogue transporté en JSON est échappé par
  `serde_json` à la sérialisation et consommé comme **donnée**, jamais comme
  source : la contrainte n'a plus de raison d'être, et les traducteurs
  retrouvent la ponctuation française normale (apostrophes droites, guillemets)
  sans reformuler.

Le test de **parité des clés** entre l'anglais embarqué et le pack français est
en revanche **conservé** dans les deux plugins : il ne dépend pas du mécanisme
de rendu et garde toute sa valeur.

`ritornello_i18n::Catalog` gagne une méthode d'export de la carte fusionnée :

```rust
/// Carte plate de toutes les clés connues, `own` surchargeant `common`.
pub fn entries(&self) -> std::collections::HashMap<&str, &str>
```

Elle alimente `GET /api/i18n` (catalogue du cœur) et `GetCatalog` (catalogue de
chaque plugin). La résolution par couches et le repli par clé restent
strictement ceux d'aujourd'hui — seul le point de sortie change. Les packs TOML
externes (`/etc/ritornello/locales/<composant>/<lang>.toml`) demeurent l'unique
source de vérité, et le sélecteur de langue continue de recharger les
catalogues à chaud.

Côté SPA, le kit fournit `t(clé, params?)` : résolution dans le catalogue
chargé, interpolation des jetons `{detail}` / `{n}` comme le fait le Rust, et
repli sur la clé elle-même si absente. Le shell charge `/api/i18n` ; chaque vue
de plugin reçoit son propre catalogue depuis `/plugins/<nom>/api/i18n`, injecté
par la route qui la monte — un plugin ne voit jamais que ses clés et celles du
vocabulaire commun.

Le changement de langue provoque le rechargement des catalogues côté SPA (pas
un `location.reload()` comme aujourd'hui).

## Les trois vues

Reprise à l'identique du comportement actuel, avec les composants du kit et les
notifications à la place des `<span id="msg">`.

**Accueil (`/`)** — la télécommande : 9 boutons de présélection et les 12
commandes simples (présélection et piste suivante/précédente, volume, muet,
lecture/pause, stop, éjecter, changement de source, veille), chacune postée à
`/api/command`. Affiche la source active. C'est le seul écran gagné : `/`
répondait 404.

**Statut (`/status`)** — table des plugins (nom, genre, état connecté/
indisponible, lien d'admin quand `admin = true`), sélecteur de sortie audio,
sélecteur de langue, et les 50 dernières lignes de log en ordre inverse.

**Radio (`/plugins/radio`)** — table des stations : numéro **non éditable**
(position, recalculé à chaque ajout/suppression), nom et URL éditables,
suppression par ligne ; boutons « Ajouter » et « Enregistrer » ; limite de **9**
présélections refusée côté IHM avec message. Bloc de recherche annuaire :
requête, filtre pays (FR / US / tous), bouton de recherche et validation à
`Entrée` ; requête vide → message dédié (`empty_query`) ; résultats affichés
`nom — codec bitrate kbps (pays)` avec un bouton d'ajout par résultat. Le plugin
interroge l'annuaire (`op: "search"`), la vue relit les résultats par
`GetData` ; **rien n'est persisté avant « Enregistrer »**. La saisie manuelle
d'une URL reste le repli quand l'annuaire est en panne.

**Recherche en vol unique** — comportement à reproduire fidèlement, il corrige
un défaut réel : le SDK sert les requêtes d'admin **strictement en série**, donc
un second déclenchement pendant qu'une recherche court se met en file derrière
la première ; avec l'annuaire en panne (budget de 4 s côté plugin), la seconde ne
démarrerait qu'après et dépasserait le plafond de 5 s du cœur, qui afficherait
« plugin injoignable ». La vue garde donc un état « recherche en cours » qui
désactive le déclenchement, rétabli dans un `finally` pour se remettre aussi bien
après une erreur qu'après un succès, et partagé par le bouton **et** la touche
`Entrée`.

**Entrées (`/plugins/generic-input`)** — sélecteur de périphérique et bouton de
rafraîchissement (`op: "rescan"`) ; table des 21 actions avec, par ligne, le ou
les codes (champ éditable, valeurs séparées par des virgules), un bouton
« Apprendre » et un bouton d'effacement ; apprentissage par sondage de
`GetData` (300 ms, délai de 10 s, bouton « Annuler ») ; chargement d'un preset
livré, import d'un `.toml` local (`op: "import_preset"`), export des bindings
du périphérique courant en TOML généré côté navigateur ; « Enregistrer »
(`op: "save"`) qui réécrit le périphérique courant en préservant les autres.

Le format TOML produit à l'export doit rester le miroir de
`presets::parse_preset` — l'avertissement présent dans le code actuel est
reporté dans la vue Vue.

## Tests

**Rust** — `cargo test --workspace` :

- `proto/admin.rs` : roundtrip JSON de chaque variante de `AdminReq` /
  `AdminResult`, dont `GetAsset`, `Asset { body: None }` et `Catalog`.
- SDK : `run_admin_plugin` + `AdminClient` bout à bout sur socket réel en
  tempdir — `GetAsset` d'un chemin connu, d'un chemin inconnu (`None`),
  `GetCatalog`, et les cas `GetData`/`SetData` existants conservés.
- Cœur, routes (`oneshot` axum) : `/` sert le shell ; repli SPA sur `/status`
  et `/plugins/radio/` ; `/assets/*` avec en-têtes de cache attendus ;
  `/plugins/{nom}/ui.js` servi puis **re-servi depuis le cache** sans second
  aller-retour IPC ; `304` sur `If-None-Match` ; `404` sur plugin inconnu et
  sur actif inconnu ; `/api/i18n` et `/plugins/{nom}/api/i18n` renvoient une
  carte non vide ; `GET /api/logs` renvoie les lignes les plus récentes en
  premier ; `GET`/`PUT /api/theme` (204, persistance dans `state.json`,
  `422` sur `mode` invalide) ; le shell servi contient bien
  `window.__RITORNELLO_THEME__` ; `/plugins/<nom>/` (segment final vide) tombe
  bien sur le repli et n'est pas capté par la route d'actifs.
- `state.rs` : roundtrip avec `theme`/`mode`, et défauts quand les champs sont
  absents d'un `state.json` existant (compatibilité ascendante).
- `ritornello-i18n` : `entries()` fusionne `own` par-dessus `common` et
  contient les clés des deux couches.
- Bouchon : le contenu embarqué est non vide et constitue un document HTML
  servable — que ce soit la vraie SPA ou le bouchon (l'un des deux est
  nécessairement présent, le `build.rs` le garantit). La fabrication du bouchon
  elle-même est testée en tant que **fonction pure** du `build.rs`, sans
  dépendre de l'état du disque au moment du test.

**Vitest** (`web/`) — `npm test --workspaces` :

- Moteur de thèmes : `applyTheme` écrit bien chaque clé en variable CSS ;
  superposition `light` → `mode` (une clé absente du bloc `dark` retombe sur le
  bloc `light`) ; itération générique (un preset avec une clé inconnue est
  appliqué sans erreur) ; injection et remplacement du `<link>` de polices ;
  repli `system-ui` présent dans chaque pile de polices.
- `t()` : résolution, interpolation `{detail}`, repli sur la clé absente.
- Client `api` : extraction du message d'erreur d'un `422`, `204` traité comme
  succès.
- Popin : filtre par nom, indication du thème actif, sélection émettant le bon
  preset.
- Chargeur de plugin : contrat compatible → montage ; contrat incompatible →
  message d'erreur ; `ui.js` en échec → message d'indisponibilité.
- Quelques composants de vue (table des stations : renumérotation après
  suppression, refus au-delà de 9).
- Recherche en vol unique : un second déclenchement pendant une recherche en
  cours n'émet **aucune** seconde requête ; l'état est rétabli après un succès
  **et** après une erreur ; le bouton et la touche `Entrée` partagent la garde ;
  une requête vide n'émet rien et affiche le message dédié.

**Playwright** (chromium) — parcours de bout en bout sur un cœur réellement
lancé (configuration de développement du README, `RITORNELLO_HTTP` sur un port
libre, `state.json` en répertoire temporaire) :

1. navigation accueil → statut → page de chaque plugin ;
2. bascule en sombre, puis vérification sur une **variable CSS calculée** que
   le mode a changé, et persistance après rechargement ;
3. ouverture de la popin, choix d'un thème, vérification de `--primary`
   calculé et persistance après rechargement ;
4. ajout d'une station et enregistrement, puis relecture depuis
   `/plugins/radio/api/data` ;
5. apprentissage de touche : le sondage et son annulation, sur un plugin
   `generic-input` sans périphérique réel (le cas « aucun périphérique » est un
   parcours légitime et déterministe).

## Documentation et déploiement

`deploy/deploy.sh` n'est **pas modifié** : les livrables JS étant embarqués dans
les binaires, il continue de ne copier que des exécutables et des fichiers de
configuration. Le `README.md` gagne en revanche :

- **Node comme prérequis de développement** (là où `cargo` suffisait), et la
  chaîne `deploy/build.sh` en trois étapes comme procédure de référence — la
  section « Compiler » explique que `npm run build --workspaces` doit précéder
  les deux `cargo`/`cross`.
- La mention du **bouchon** : un `cargo build` seul produit une IHM qui invite à
  lancer le build npm ; ce n'est pas une panne.
- Une section **Thème** : bascule clair/sombre, sélecteur des 42 presets
  tweakcn, persistance dans `state.json`, et le fait que les polices viennent
  d'un CDN avec repli sur la police système hors ligne.
- Dans la section « Plugins », **comment un plugin tiers livre une IHM** :
  `admin = true`, répondre à `GetAsset("ui.js")`/`("ui.css")` et `GetCatalog`,
  exporter `contract` et un composant par défaut, importer `vue` et
  `@ritornello/ui` sans les embarquer — avec la précision qu'un `ui.js` écrit à
  la main, sans build Node, est parfaitement valable.

## Hors périmètre

- Authentification et contrôle d'accès (inchangé : aucun, réseau de confiance).
- Toute nouvelle fonction métier, tout nouveau réglage.
- Éditeur de thème maison : on consomme les presets tweakcn, on n'en crée pas.
- Embarquement des polices dans les binaires (le CDN avec repli système suffit).
- PWA, fonctionnement hors ligne, service worker.
- Le plugin `console` (affichage TTY) : il n'a pas d'IHM web et n'est pas
  touché.
- Rendu côté serveur, hydratation : la SPA est servie statiquement.
- Réordonnancement des stations par glisser-déposer (déjà hors périmètre de la
  spec annuaire).
