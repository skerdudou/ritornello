# CI GitHub Actions — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** que chaque poussée sur GitHub exécute les cinq commandes de `docs/development.md:189-193` (tests Rust, clippy, tests web, typecheck, e2e) et produise sur tag les binaires armv7 que `deploy.sh` attend.

**Architecture:** un seul workflow `ci.yml` en quatre jobs. `web` construit les dist (obligatoires : sans eux `cargo build` embarque un **bouchon** et ne fait qu'avertir) et les publie en artefact ; `rust` et `e2e` les téléchargent puis compilent sur Ubuntu (les tests du SDK exigent des sockets Unix — pas de runner Windows possible) ; `release` ne tourne que sur tag `v*` et pousse les binaires `cross` en artefact. Un script local `scripts/ci-local.sh` reprend les mêmes commandes dans le même ordre, parce que **le dépôt n'a pas encore de remote** : la seule façon d'exercer le YAML est la première publication, il faut donc que les commandes aient déjà tourné ici.

**Tech Stack:** GitHub Actions, `actions/setup-node@v4`, `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`, `actions/upload-artifact@v4` / `download-artifact@v4`, `cross`, Playwright chromium, `actionlint` pour valider la syntaxe sans remote.

**Spec:** conversation du 2026-08-26 ; faits : `package.json` racine (`engines.node >= 20`, `npm ci`, scripts `build`/`test`/`typecheck --workspaces --if-present`), `deploy/build.sh` (npm ci → build → typecheck → `cargo build --workspace` → `cross build --release --workspace --target armv7-unknown-linux-gnueabihf`), `web/app/playwright.config.ts` (chromium seul, `webServer: node e2e/serve.mjs`, `workers: 1`), `docs/development.md:203-205` (e2e = cœur compilé + `mpv`).

## Global Constraints

- Node `>= 20` (prendre 22 LTS) ; `npm ci` strict sur `package-lock.json` (pas de `npm install`).
- Rust : aucun `rust-toolchain.toml` ni `rust-version` dans le dépôt → `stable`. Ne pas en ajouter (hors périmètre).
- Les tests Rust auto-sautent si `ffmpeg` manque (`eprintln!("ffmpeg absent : test saute")`) : l'installer pour que la CI teste vraiment.
- `dist/` est ignoré par git : le job `web` est le seul producteur des dist, les autres les téléchargent.
- Aucun secret nécessaire : pas de déploiement depuis la CI (`deploy.sh` reste manuel, il parle au Pi).
- Flake connu : un test de `core` est flaky sous charge (mémoire `fusion-quatre-chantiers-2026-08-21`) et quatre tests vitest frôlent 5 s. On **ne** masque pas par retry aveugle ; on installe une seule relance ciblée si la première exécution réelle le montre.
- Le badge README et la doc sont dans le périmètre ; la protection de branche et les règles GitHub ne le sont pas (se font dans l'interface, une fois le dépôt publié).

---

### Task 1 : le job `web` — build, typecheck, tests vitest, artefact des dist

**Files:**
- Create: `.github/workflows/ci.yml`
- Create: `scripts/ci-local.sh`

**Interfaces:**
- Produces: artefact `dist-web` contenant `web/app/dist/**` et `crates/*/ui/dist/**` ; c'est ce que `rust` et `e2e` consomment.

- [ ] **Step 1 : le script local, miroir des commandes**

`scripts/ci-local.sh` (exécuté depuis WSL, où vivent `cargo` et `npm` ; voir `docs/development.md`) :

```bash
#!/usr/bin/env bash
# Les mêmes commandes que .github/workflows/ci.yml, dans le même ordre : le
# dépôt n'a pas encore de remote, donc c'est ici que la recette est validée
# avant que le YAML ne tourne pour la première fois. Si l'un des deux change,
# l'autre doit suivre.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== web =="
npm ci
npm run build --workspaces --if-present
npm run typecheck
npm test --workspaces --if-present

echo "== rust =="
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

echo "== e2e =="
(cd web/app && npx playwright install chromium && npm run e2e)
```

- [ ] **Step 2 : le workflow, job `web` seul**

```yaml
name: CI

on:
  push:
    branches: [main]
    tags: ['v*']
  pull_request:

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true

jobs:
  web:
    name: IHM (build, typecheck, vitest)
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: npm
      - run: npm ci
      - run: npm run build --workspaces --if-present
      - run: npm run typecheck
      - run: npm test --workspaces --if-present
      - name: Publier les dist pour les jobs Rust
        uses: actions/upload-artifact@v4
        with:
          name: dist-web
          if-no-files-found: error
          path: |
            web/app/dist
            crates/*/ui/dist
```

- [ ] **Step 3 : valider la syntaxe sans remote**

Run (WSL) : `docker run --rm -v "$PWD:/repo" -w /repo rhysd/actionlint:latest`
Expected: aucune sortie (zéro erreur). Si docker est absent : `curl -sSfL https://raw.githubusercontent.com/rhysd/actionlint/main/scripts/download-actionlint.bash | bash && ./actionlint`.

- [ ] **Step 4 : exécuter la partie web du script local**

Run (WSL) : `bash scripts/ci-local.sh 2>&1 | sed -n '/== web ==/,/== rust ==/p'`
Expected: build des 6 workspaces, typecheck, vitest verts — et `ls web/app/dist/index.html crates/*/ui/dist/ui.js` liste 1 + 4 fichiers (les chemins que l'artefact doit contenir).

- [ ] **Step 5 : commit**

```bash
chmod +x scripts/ci-local.sh
git add .github/workflows/ci.yml scripts/ci-local.sh
git commit -m "ci: le job web, et le script local qui en est le miroir"
```

---

### Task 2 : le job `rust` — build avec les vrais dist, clippy, tests

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: artefact `dist-web` (Task 1).

- [ ] **Step 1 : ajouter le job**

```yaml
  rust:
    name: Rust (build, clippy, tests)
    needs: web
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/download-artifact@v4
        with:
          name: dist-web
      - name: Refuser un bouchon embarqué
        # build.rs remplace un dist absent par un bouchon et se contente d'un
        # warning : ici c'est une erreur, sinon la CI validerait un binaire
        # sans IHM.
        run: test -f web/app/dist/index.html && ls crates/*/ui/dist/ui.js
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy
      - uses: Swatinem/rust-cache@v2
      - name: Outils des tests
        # ffmpeg : sinon quatre tests se sautent eux-mêmes en silence.
        run: sudo apt-get update && sudo apt-get install -y --no-install-recommends ffmpeg
      - run: cargo build --workspace
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace
```

Point de vigilance sur `download-artifact@v4` : sans `path`, il restaure les fichiers **à leur chemin d'origine relatif au dépôt** parce que l'upload a listé plusieurs chemins — c'est le comportement voulu, et la ligne « Refuser un bouchon » le vérifie.

- [ ] **Step 2 : valider**

Run: `actionlint` (comme Task 1, Step 3) puis, en WSL : `bash scripts/ci-local.sh 2>&1 | sed -n '/== rust ==/,/== e2e ==/p'`
Expected: build sans `cargo::warning=IHM … non construite` (les dist sont là), clippy et tests verts. Noter la durée : elle donne l'ordre de grandeur du job.

- [ ] **Step 3 : commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: le job rust, qui refuse un bouchon d'IHM"
```

---

### Task 3 : le job `e2e` — Playwright contre le vrai cœur

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: artefact `dist-web` ; `web/app/e2e/serve.mjs` lance `target/debug/ritornello-core` directement sous Linux.

- [ ] **Step 1 : ajouter le job**

```yaml
  e2e:
    name: Parcours e2e (Playwright)
    needs: web
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/download-artifact@v4
        with:
          name: dist-web
      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: npm
      - run: npm ci
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Lecteur audio des parcours
        # docs/development.md : les e2e jouent vraiment via le greffon radio.
        run: sudo apt-get update && sudo apt-get install -y --no-install-recommends mpv ffmpeg
      - run: cargo build --workspace
      - run: npx playwright install --with-deps chromium
        working-directory: web/app
      - run: npm run e2e
        working-directory: web/app
      - if: failure()
        uses: actions/upload-artifact@v4
        with:
          name: playwright-report
          path: |
            web/app/playwright-report
            web/app/test-results
```

`rust-cache` partage son cache entre `rust` et `e2e` (même clé de lock), donc le second `cargo build` est surtout du lien.

- [ ] **Step 2 : valider**

Run: `actionlint`, puis en WSL `bash scripts/ci-local.sh 2>&1 | sed -n '/== e2e ==/,$p'`
Expected: les 2 specs passent (`files.spec.ts`, `parcours.spec.ts`). Si `mpv` manque dans WSL : `sudo apt-get install mpv` — c'est la même dépendance que le job.

- [ ] **Step 3 : commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: les parcours e2e contre le coeur compile, rapport publie en cas d'echec"
```

---

### Task 4 : le job `release` — les binaires armv7 sur tag

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: artefact `dist-web` ; la liste `PLUGINS` de `deploy/deploy.sh:15` dit ce que l'artefact doit contenir (le plan « ménage deploy » la dérive de `plugins.example.toml` ; ici on prend simplement tout `ritornello-*` du dossier cible).

- [ ] **Step 1 : ajouter le job**

```yaml
  release:
    name: Binaires armv7 (cross)
    if: startsWith(github.ref, 'refs/tags/v')
    needs: [rust, e2e]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/download-artifact@v4
        with:
          name: dist-web
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo install cross --locked
      - run: cross build --release --workspace --target armv7-unknown-linux-gnueabihf
      - name: Rassembler ce que deploy.sh attend
        run: |
          mkdir -p livraison
          cp target/armv7-unknown-linux-gnueabihf/release/ritornello-core livraison/
          cp target/armv7-unknown-linux-gnueabihf/release/ritornello-plugin-* livraison/
          ls -l livraison
      - uses: actions/upload-artifact@v4
        with:
          name: ritornello-armv7-${{ github.ref_name }}
          path: livraison
```

`cross` utilise docker, présent sur `ubuntu-latest`. Le `cargo install cross` coûte ~1 min ; acceptable pour un job qui ne tourne que sur tag.

- [ ] **Step 2 : valider**

Run: `actionlint`. En WSL, si `cross` et docker y sont : `cross build --release --workspace --target armv7-unknown-linux-gnueabihf` (c'est l'étape 3/3 de `deploy/build.sh`, déjà exercée à chaque déploiement — pas besoin de la rejouer si elle l'a été récemment).
Expected: pas d'erreur `actionlint`.

- [ ] **Step 3 : commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: les binaires armv7 en artefact sur tag v*"
```

---

### Task 5 : doc, badge, et le contrat CI ↔ script local

**Files:**
- Modify: `docs/development.md` (après la liste des commandes, ~l. 193)
- Modify: `README.md` (badge en tête)

- [ ] **Step 1 : rédiger**

`docs/development.md`, nouvelle sous-section « Intégration continue » :

- le workflow `.github/workflows/ci.yml` exécute les cinq commandes ci-dessus sur Ubuntu, en quatre jobs (`web` → `rust`, `e2e` ; `release` sur tag) ;
- pourquoi Ubuntu et pas Windows (sockets Unix dans les tests du SDK), pourquoi `ffmpeg` et `mpv` sont installés (tests qui se sautent, e2e qui jouent) ;
- pourquoi le job `rust` refuse un dist absent (le bouchon de `build.rs`) ;
- `scripts/ci-local.sh` est le miroir : « si l'un change, l'autre suit » ;
- le flake connu et la règle : pas de retry aveugle.

`README.md` : `![CI](https://github.com/<owner>/ritornello/actions/workflows/ci.yml/badge.svg)` — `<owner>` à remplacer à la publication ; laisser tel quel avec un commentaire HTML `<!-- owner à fixer à la publication -->` plutôt qu'inventer un compte.

- [ ] **Step 2 : commit**

```bash
git add docs/development.md README.md
git commit -m "docs(dev): l'integration continue, ses quatre jobs, et le script qui en est le miroir"
```

---

### Task 6 (à la publication, hors worktree) : première exécution réelle

Pas de code. Quand le remote existe :

- [ ] pousser une branche, ouvrir une PR, lire les quatre jobs ;
- [ ] noter les durées ; si `rust` > 15 min, envisager `cargo nextest` (hors périmètre ici) ;
- [ ] si le test flaky de `core` tombe : l'identifier par son nom dans le journal et **le corriger** (leçon : hypothèse d'exécution rapide), pas ajouter un retry ;
- [ ] fixer `<owner>` du badge.

---

## Auto-revue

- **Couverture** : cinq commandes de la doc → `web` (3), `rust` (2 + build), `e2e` (1) ✔ ; livrable armv7 → `release` ✔ ; validation sans remote → `actionlint` + `ci-local.sh` ✔ ; doc ✔.
- **Cohérence** : nom d'artefact `dist-web` identique dans les quatre jobs ; chemins `web/app/dist` et `crates/*/ui/dist` identiques à ceux que `build.rs` lit (`../../web/app/dist`, `ui/dist/ui.js`).
- **Limite assumée** : le YAML n'aura tourné nulle part avant la publication ; `actionlint` valide la forme, `ci-local.sh` valide les commandes, la Task 6 valide l'ensemble.
