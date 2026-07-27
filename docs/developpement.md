# Développement

## Instance locale sans matériel

Sur n'importe quelle machine Linux (ou WSL sous Windows, l'environnement
utilisé pour développer ce projet — WSL n'est qu'un détail d'environnement,
pas une exigence : un Linux natif fonctionne à l'identique). Après
`cargo build --workspace` (voir [installation.md](installation.md)), lancer
une instance locale sans matériel Pi :

    mkdir -p /tmp/rp
    cat > /tmp/rp/plugins.toml <<'PLUGINS'
    [[plugin]]
    name = "radio"
    kind = "source"
    exec = "target/debug/ritornello-plugin-radio"
    admin = true

    [[plugin]]
    name = "console"
    kind = "display"
    exec = "target/debug/ritornello-plugin-console"
    PLUGINS
    cat > /tmp/rp/stations.toml <<'STATIONS'
    [[stations]]
    name = "FIP"
    url = "http://icecast.radiofrance.fr/fip-midfi.mp3"
    preset = 1
    STATIONS
    RITORNELLO_PLUGINS=/tmp/rp/plugins.toml RITORNELLO_STATE=/tmp/rp/state.json \
    RITORNELLO_MPV_SOCKET=/tmp/rp/mpv.sock RITORNELLO_RUNTIME_DIR=/tmp/rp \
    RITORNELLO_HTTP=127.0.0.1:8080 \
    RITORNELLO_CONSOLE_TTY=/dev/stdout \
    RITORNELLO_RADIO_STATIONS=/tmp/rp/stations.toml RITORNELLO_RADIO_STATE=/tmp/rp/plugin-radio.json \
    cargo run -p ritornello-core

Le plugin `generic-input` peut être ajouté au `plugins.toml` de `/tmp/rp` :

    [[plugin]]
    name = "generic-input"
    kind = "input"
    exec = "target/debug/ritornello-plugin-generic-input"
    admin = true

et les variables suivantes ajoutées à la ligne d'environnement :

    RITORNELLO_INPUT_BINDINGS=/tmp/rp/input-bindings.toml RITORNELLO_INPUT_PRESETS=deploy/input-presets

Les plugins `metadata` s'ajoutent de la même façon (`kind = "metadata"`,
exécutables `ritornello-plugin-musicbrainz` et
`ritornello-plugin-ouifm-metas`).

## Tests

    cargo test --workspace                              # suites Rust
    cargo clippy --workspace --all-targets -- -D warnings
    npm test --workspaces                               # vitest (SPA, kit, IHM de plugins)
    npm run typecheck                                   # vue-tsc
    npm run e2e -w app                                  # parcours Playwright

Le style de test du projet : fonctions pures testées contre des **captures
réelles** (trames mpv, réponses radio-browser, flux OUI FM), et des tests
**discriminants** — plusieurs encodent une régression vécue et disent
laquelle en commentaire.

## Parcours e2e (Playwright)

`npm run e2e -w app` a besoin d'un cœur compilé (`cargo build --workspace`)
et de `mpv` sur la machine qui exécute les parcours (lecture réelle par le
plugin radio). Sous Windows — l'environnement où npm/node/Playwright tournent
dans ce projet —, le binaire du cœur est un ELF Linux compilé sous WSL : le
harnais (`web/app/e2e/serve.mjs`) le lance donc via `wsl.exe`, pas
directement, et l'arrêt (`web/app/e2e/teardown.mjs`) doit cibler
explicitement le processus côté WSL, un `taskkill` Windows ne tuant que
l'arbre de processus Windows. Sous Linux natif, le même harnais lance le
binaire directement. Les particularités (répertoires de configuration vs
d'exécution, sockets Unix impossibles sur le montage DrvFs) sont documentées
en tête de `serve.mjs`.

## Données embarquées à régénérer

- **Presets de thème** (42 thèmes tweakcn) :
  `cd web/kit && node scripts/fetch-presets.mjs`.
- **Table des webradios OUI FM** :
  `node crates/ritornello-plugin-ouifm-metas/scripts/fetch-webradios.mjs`
  (relit la variable `apidata` du site ; `--verifier` signale une dérive sans
  rien écrire).

## Garde-fous de build

`web/app/scripts/verifier-dist.mjs` vérifie après chaque build npm que
l'import map est correcte et que le runtime Vue est unique ; l'équivalent
pour les bundles de plugin est `verifier-dist-plugin.mjs`. Le build npm doit
**toujours** précéder les builds cargo : la SPA et les `ui.js` des plugins
sont embarqués à la compilation (`rust-embed`, `include_str!`). C'est
l'ordre qu'applique `deploy/build.sh`.

## Processus

Le projet est développé par spécifications, plans d'implémentation et revues
systématiques ; ces documents sont archivés dans
[docs/superpowers/](superpowers/). La revue complète du 2026-07-27 (quatre
relecteurs par zone : protocole/SDK, cœur, plugins, web/déploiement) a
produit la série de correctifs `fix(core)`/`fix(sdk,i18n)`/`fix(plugins)`/
`fix(web)`/`fix(deploy)` visible dans l'historique. Dette identifiée et
**assumée** à ce stade, par ordre d'intérêt :

- pas de version de protocole entre cœur et plugins : une requête inconnue
  d'un vieux binaire est ignorée côté plugin et coûte un timeout de 5 s côté
  cœur — acceptable tant que cœur et plugins sont déployés ensemble ;
- le bootstrap « deux moitiés » (source/input + admin) et le couple
  `build.rs`/placeholder sont dupliqués entre radio et generic-input, comme
  les helpers `env_or`/`log_half` — à faire remonter au SDK au troisième
  plugin à IHM ;
- `Enrichment` dérive `Default`, ce qui permet d'oublier l'écho d'identité
  (l'enrichissement est alors simplement écarté) ;
- les trois copies du test `i18nKeysUsed` et les tables HTML des admins
  mériteraient un helper/composant partagé dans le kit.
