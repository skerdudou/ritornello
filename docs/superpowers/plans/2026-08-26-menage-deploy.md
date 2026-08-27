# Ménage de `deploy.sh` — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** que `deploy/deploy.sh` n'ait plus qu'une source pour la liste des greffons, et ne porte plus la migration `mce → generic-input`.

**Architecture:** la liste `PLUGINS` (l. 15) est aujourd'hui dupliquée avec `deploy/plugins.example.toml` et la duplication est *compensée* par un contrôle qui refuse de déployer si elles divergent (l. 24-30). Or ce contrôle contient déjà l'extraction qui rend la duplication inutile : on dérive `PLUGINS` du TOML avec le même `sed`, et le contrôle disparaît avec la duplication. La ligne `rm -f …ritornello-plugin-mce` (l. 168) est un résidu de migration : le seul appareil en service a été redéployé des dizaines de fois depuis, le binaire `mce` n'y est plus.

**Tech Stack:** bash (`set -euo pipefail`), `sed`, `mapfile`.

**Spec:** conversation du 2026-08-26 (« mentions honorables »).

## Global Constraints

- Le script doit rester exécutable sans le Pi pour sa partie locale : le test consiste à **afficher** `PLUGINS` sans déployer.
- Les commentaires expliquent un pourquoi : celui de la l. 13-23 (les deux erreurs silencieuses) doit survivre, réécrit pour la nouvelle mécanique.
- `deploy/missing-plugins.awk` (l. 16 : « survives the mce → generic-input ») reste tel quel : il compare le `plugins.toml` installé au TOML d'exemple, sa mention de `mce` est historique et exacte.
- **Hypothèse à confirmer** avant le commit de la Task 2 : aucun appareil autre que celui de l'utilisateur ne tourne encore avec un `ritornello-plugin-mce` dans `/usr/local/lib/ritornello/plugins/`. Si un doute subsiste, garder la ligne et ne faire que la Task 1.

---

### Task 1 : une seule source pour la liste des greffons

**Files:**
- Modify: `deploy/deploy.sh:13-30`

- [ ] **Step 1 : constater l'état**

Run (WSL ou Git Bash) : `sed -n 's|^exec *= *".*/ritornello-plugin-\([^"]*\)".*|\1|p' deploy/plugins.example.toml | sort | tr '\n' ' '`
Expected: `cd console files generic-input mpd musicbrainz ouifm-metas radio radiofrance-metas` — les neuf mêmes noms que la l. 15 (dans un autre ordre ; l'ordre n'a pas d'importance, `scp` et `mv` prennent la liste en bloc).

- [ ] **Step 2 : remplacer**

Les lignes 13 à 30 deviennent :

```bash
# The plugin list drives the scp then the remote mv. It is derived from
# deploy/plugins.example.toml — the core's side of the same set — so the two
# cannot diverge: a plugin declared there without a binary built here gives the
# core an exec that does not exist, and one built here but absent there ships a
# plugin nothing launches. Both were the mistake of a plugin added in a hurry,
# and both were silent; deriving the list removes the second, and the scp below
# fails loudly on the first (no such file in target/).
mapfile -t PLUGINS < <(sed -n 's|^exec *= *".*/ritornello-plugin-\([^"]*\)".*|\1|p' \
  deploy/plugins.example.toml | sort)
if [ "${#PLUGINS[@]}" -eq 0 ]; then
  echo "deploy.sh: no plugin found in deploy/plugins.example.toml" >&2
  exit 1
fi
```

- [ ] **Step 3 : vérifier sans déployer**

Run : `bash -c 'source <(sed -n "11,30p" deploy/deploy.sh | sed "s|^cd .*||"); printf "%s\n" "${PLUGINS[@]}"'` depuis la racine du dépôt.
Expected: les neuf noms, un par ligne. Puis `bash -n deploy/deploy.sh` → aucune erreur de syntaxe.

Vérifier aussi que la liste sert bien aux deux endroits qui la consommaient : `rg 'PLUGINS' deploy/deploy.sh` → l'`scp` des binaires et `DEPLACE_PLUGINS=` (l. 164), rien d'autre.

- [ ] **Step 4 : commit**

```bash
git add deploy/deploy.sh
git commit -m "deploy: la liste des greffons derivee de plugins.example.toml, le controle de divergence n'a plus d'objet"
```

---

### Task 2 : retirer la migration `mce`

**Files:**
- Modify: `deploy/deploy.sh:168` et le commentaire l. 99 (« hand-edited exec (the mce -> generic-input migration) ») si sa phrase n'a plus de sens sans la ligne.

- [ ] **Step 1 : constater sur l'appareil** (c'est l'hypothèse des contraintes)

Run : `ssh <pi> ls /usr/local/lib/ritornello/plugins/`
Expected: aucun `ritornello-plugin-mce`. Si présent : lancer un déploiement normal d'abord (il le supprime), puis reprendre ici.

- [ ] **Step 2 : retirer la ligne**

Supprimer `&& sudo rm -f /usr/local/lib/ritornello/plugins/ritornello-plugin-mce \` (l. 168). Relire le commentaire l. 97-101 : s'il ne fait que citer `mce` comme exemple de ce que `missing-plugins.awk` sait traverser, le laisser ; s'il justifie la ligne supprimée, retirer la phrase.

- [ ] **Step 3 : vérifier**

Run : `bash -n deploy/deploy.sh && rg -n 'mce' deploy/`
Expected: syntaxe OK ; les seules mentions restantes de `mce` sont dans `missing-plugins.awk` (historique exact) et, éventuellement, le commentaire l. 99 conservé.

- [ ] **Step 4 : commit**

```bash
git add deploy/deploy.sh
git commit -m "deploy: la migration mce -> generic-input a fini son travail"
```

---

### Task 3 : doc

**Files:**
- Modify: `docs/installation.md` ou `docs/development.md` — là où la recette de déploiement mentionne la liste `PLUGINS` ou l'ajout d'un greffon (`rg -n 'PLUGINS|plugins.example' docs/`).

- [ ] Remplacer « ajouter le nom dans `PLUGINS` de `deploy.sh` **et** dans `plugins.example.toml` » par « ajouter l'entrée dans `plugins.example.toml` ; `deploy.sh` en dérive la liste ». S'il n'y a aucune mention, ne rien ajouter.
- [ ] commit `docs: ajouter un greffon ne se fait plus qu'a un endroit`.

---

## Auto-revue

- Deux résidus, deux commits, une doc. Le contrôle supprimé en Task 1 protégeait contre deux erreurs : l'une disparaît par construction, l'autre devient une erreur bruyante de `scp` — dit dans le commentaire.
- Hypothèse explicite sur `mce`, avec la vérification `ssh` avant de supprimer.
