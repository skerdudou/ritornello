# Plugin fichiers réseau — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lire des fichiers audio depuis un partage SMB authentifié et des
supports locaux, avec des listes de lecture m3u constituées par ajout récursif
de répertoires, enregistrables et rechargeables.

**Architecture:** Un crate `ritornello-plugin-files` portant **deux binaires** —
le plugin (moitié Source + moitié Admin, deux tâches tokio indépendantes sur le
modèle du plugin radio) et un binaire **racine** de montage lancé par une unité
systemd oneshot via polkit. mpv tient la liste de lecture : le plugin lui donne
un m3u généré et pilote l'index. Le cœur gagne deux champs sur
`SourceAction::Play` et une couche de métadonnées lisant les tags que mpv lui
envoie déjà.

**Tech Stack:** Rust (workspace existant, `tokio`, `serde`, `toml`,
`async-trait`), Vue 3 + Vite pour la page, mpv JSON-IPC, `mount.cifs`
(`cifs-utils`), systemd + polkit. **Aucune nouvelle dépendance runtime** : pas
de crate D-Bus (on lance `systemctl` en processus fils, comme l'onglet
Système), pas de crate de tags (le cœur lit ceux de mpv).

**Spec:** `docs/superpowers/specs/2026-08-15-plugin-fichiers-reseau-design.md`

## Global Constraints

- **Base :** `main` à `b3cca84`. Branche de travail `worktree-fichiers-reseau`.
- **Compilation sans avertissement :** le projet construit avec `-D warnings`.
  Tout code mort doit être `#[cfg(test)]` ou supprimé.
- **Langues :** commentaires et documentation en **français**, messages de
  journal (`tracing`) en **anglais**, messages destinés à l'écran par
  **catalogue i18n**.
- **Erreurs utilisateur :** une validation est une fonction **pure** rendant une
  **erreur typée** ; la frontière HTTP résout `message(&Catalog)`. `Display`
  rend une phrase anglaise pour les journaux. Modèle de référence :
  `ValidationError` dans `crates/ritornello-plugin-radio/src/config.rs:19-60`.
- **Test anti-clé obligatoire :** chaque enum d'erreur doit avoir un test
  résolvant toutes ses variantes contre le catalogue anglais **embarqué** et
  refusant un message égal à sa propre clé. Modèle :
  `chaque_refus_resout_contre_le_catalogue_embarque` dans
  `crates/ritornello-core/src/status.rs`.
- **Écritures de fichiers :** toujours atomiques — écrire `.tmp` puis `rename`.
- **Présélections :** `preset` est un `u8`, plage 1–99. `preset_count` vaut
  `min(len, 99)`.
- **Plafond de liste :** 2000 pistes.
- **Extensions audio retenues :** `mp3`, `flac`, `ogg`, `oga`, `opus`, `m4a`,
  `aac`, `wav`, `wma`, `aiff`, `ape`, `wv`, `mpc` (comparaison insensible à la
  casse).
- **Point de montage imposé :** `/mnt/ritornello/<name>`, jamais lu depuis la
  configuration.
- **Page de plugin :** `vue` et `@ritornello/ui` fournis par l'import map (ne
  jamais les empaqueter), prop `base` **requise sans valeur par défaut**,
  gabarits **précompilés** (le Vue servi est runtime-only), **pas** de
  `vue-router`, noms de fichiers produits **à plat** (un seul segment d'URL).
- **Tests :** `cargo test --workspace`, `npm test --workspaces`,
  `npm run e2e -w app` (nécessite `cargo build --workspace`).
- **Commits :** un par tâche minimum, préfixe conventionnel, message en
  français expliquant le **pourquoi**.

## Phasage

Le plan se découpe en quatre phases, chacune livrant du logiciel qui marche :

| Phase | Tâches | Ce qui marche à la fin |
|---|---|---|
| 1. Cœur et protocole | 1 à 4 | Le cd ne dépend plus d'un reniflage d'URI ; **toute** source jouant un fichier taggé affiche artiste/titre/album. |
| 2. La Source, en local | 5 à 9 | Lecture d'une liste de fichiers locaux depuis la télécommande, avec reprise après redémarrage. |
| 3. Le montage réseau | 10 à 11 | Le partage SMB authentifié se monte et devient une racine comme une autre. |
| 4. Listes, page et livraison | 12 à 16 | Enregistrement et rechargement des listes, constitution au navigateur, déploiement documenté. |

## File Structure

**Modifié — cœur et protocole :**

| Fichier | Responsabilité |
|---|---|
| `crates/ritornello-proto/src/source.rs` | `Play { uri, start, finite }` et ses constructeurs. |
| `crates/ritornello-core/src/player/mpv.rs` | `set_playlist_pos`, extraction des tags à côté d'`icy_title`. |
| `crates/ritornello-core/src/core.rs` | Applique `start` et `finite` ; le reniflage `cdda://` disparaît. |
| `crates/ritornello-core/src/metadata.rs` | Couche d'arbitrage : plugin > tags > ICY. |
| `crates/ritornello-plugin-cd/src/main.rs` | Déclare `finite: true`. |
| `crates/ritornello-plugin-radio/src/main.rs` | Passe par le constructeur. |

**Créé — `crates/ritornello-plugin-files/` :**

| Fichier | Responsabilité |
|---|---|
| `Cargo.toml` | Une `[lib]` et deux `[[bin]]` : le plugin et le binaire de montage. |
| `src/lib.rs` | Déclare les modules partagés par les deux binaires. Sans elle, ils ne partageraient rien. |
| `src/roots.rs` | Modèle des racines, chargement TOML, **validation typée**. Partagé avec le binaire de montage. |
| `src/m3u.rs` | Analyse et rendu m3u, résolution des chemins. |
| `src/scan.rs` | Marche récursive, filtre d'extensions, garde anti-boucle, plafond. |
| `src/playlist.rs` | Modèle de liste : pistes, index, `select`/`next`/`prev`, m3u destiné à mpv. |
| `src/state.rs` | État persisté (liste courante, index). |
| `src/mount.rs` | Dialogue avec `systemctl`, état des montages. |
| `src/store.rs` | Listes enregistrées : interne et sur une racine. |
| `src/admin.rs` | Moitié Admin : `get_data`/`set_data`, tâche de scan. |
| `src/main.rs` | Moitié Source (`SourcePlugin`) et démarrage des deux moitiés. |
| `src/bin/media-mount.rs` | Binaire **racine** : réconcilie les montages. |
| `src/locales/en.toml` | Catalogue anglais embarqué. |
| `ui/` | Module Vue (Vite), trois volets. |

**Créé — déploiement :**

`deploy/ritornello-media-mount.service`, `deploy/51-ritornello-media.rules`,
`deploy/media-roots.example.toml`, `deploy/locales/files/fr.toml`.

---

# Phase 1 — Cœur et protocole

### Task 1 : `SourceAction::Play` gagne `start` et `finite`

**Files:**
- Modify: `crates/ritornello-proto/src/source.rs:43-51`
- Modify: `crates/ritornello-plugin-radio/src/main.rs` (sites de construction)
- Modify: `crates/ritornello-plugin-cd/src/main.rs` (sites de construction)
- Test: `crates/ritornello-proto/src/source.rs` (module `tests` existant)

**Interfaces:**
- Produces: `SourceAction::Play { uri: String, start: Option<i64>, finite: bool }`,
  `SourceAction::play(uri: impl Into<String>) -> SourceAction`,
  `SourceAction::starting_at(self, n: i64) -> SourceAction`,
  `SourceAction::finite(self) -> SourceAction`.

- [ ] **Step 1 : écrire les tests qui échouent**

Dans le module `tests` de `crates/ritornello-proto/src/source.rs` :

```rust
#[test]
fn play_sans_champs_neufs_reste_serialise_a_l_identique() {
    // La garantie de compatibilité : une trame émise par le plugin radio ne
    // doit pas changer d'un octet, sans quoi les tests d'intégration du cœur
    // et les traces de journalctl deviendraient illisibles à comparer.
    let a = SourceAction::play("http://icecast.radiofrance.fr/fip-midfi.mp3");
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        r#"{"action":"Play","data":{"uri":"http://icecast.radiofrance.fr/fip-midfi.mp3"}}"#
    );
}

#[test]
fn start_et_finite_font_le_tour() {
    let a = SourceAction::play("/var/lib/ritornello/plugin-files.m3u")
        .starting_at(4)
        .finite();
    let json = serde_json::to_string(&a).unwrap();
    assert!(json.contains(r#""start":4"#), "{json}");
    assert!(json.contains(r#""finite":true"#), "{json}");
    let back: SourceAction = serde_json::from_str(&json).unwrap();
    assert_eq!(back, a);
}

#[test]
fn une_trame_anterieure_se_relit_en_flux_live_depuis_le_debut() {
    // Un plugin antérieur n'émet ni `start` ni `finite` : les défauts doivent
    // reproduire exactement le comportement historique (flux live, début de
    // liste), sans quoi une mise à jour partielle changerait la lecture.
    let back: SourceAction =
        serde_json::from_str(r#"{"action":"Play","data":{"uri":"http://x"}}"#).unwrap();
    assert_eq!(back, SourceAction::Play { uri: "http://x".into(), start: None, finite: false });
}
```

- [ ] **Step 2 : lancer les tests, vérifier l'échec**

Run: `cargo test -p ritornello-proto source::tests -- --nocapture`
Expected: FAIL — `no function or associated item named 'play' found`.

- [ ] **Step 3 : modifier la variante et ajouter les constructeurs**

Dans `crates/ritornello-proto/src/source.rs`, remplacer `Play { uri: String }` :

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "data")]
pub enum SourceAction {
    Noop,
    Play {
        uri: String,
        /// Index de départ dans la liste que `uri` désigne, quand c'en est une.
        ///
        /// Absent = « commence au début », le comportement historique. Le cœur
        /// applique `playlist-pos` juste après `loadfile` : mesuré fiable, mpv
        /// résolvant un `.m3u` dès la commande, sans dépliage différé.
        ///
        /// C'est l'unique moyen pour une Source de reprendre une liste à la
        /// piste n — chiffre de la télécommande, ou reprise après redémarrage.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<i64>,
        /// Ce que `uri` désigne a une **fin normale** : un disque, une liste de
        /// fichiers. Quand mpv devient inactif, c'est la fin du contenu, pas
        /// une coupure de flux à relancer.
        ///
        /// Absent (= `false`) veut dire « flux live », le comportement
        /// historique : c'est ce qui garde les trames de la radio inchangées.
        /// Remplace le reniflage `uri.starts_with("cdda://")` du cœur, qui
        /// devinait ce que seule la Source sait.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        finite: bool,
    },
    Stop,
    PlayerNext,
    PlayerPrev,
}

impl SourceAction {
    /// Lecture d'une URI, aux défauts historiques : depuis le début, flux live.
    ///
    /// Passer par ce constructeur plutôt que par la variante littérale évite
    /// qu'un champ ajouté plus tard n'oblige à retoucher tous les appelants.
    pub fn play(uri: impl Into<String>) -> Self {
        SourceAction::Play { uri: uri.into(), start: None, finite: false }
    }

    /// Positionne la lecture sur l'élément d'index `n` de la liste. Sans effet
    /// sur une action qui n'est pas un `Play`.
    #[must_use]
    pub fn starting_at(self, n: i64) -> Self {
        match self {
            SourceAction::Play { uri, finite, .. } => {
                SourceAction::Play { uri, start: Some(n), finite }
            }
            autre => autre,
        }
    }

    /// Déclare un contenu fini, dont l'inactivité de mpv signale la fin et non
    /// une coupure. Sans effet sur une action qui n'est pas un `Play`.
    #[must_use]
    pub fn finite(self) -> Self {
        match self {
            SourceAction::Play { uri, start, .. } => {
                SourceAction::Play { uri, start, finite: true }
            }
            autre => autre,
        }
    }
}
```

- [ ] **Step 4 : réparer les sites de construction**

`crates/ritornello-plugin-radio/src/main.rs` — dans `play_preset` :

```rust
SourceOutcome::new(SourceAction::play(st.url.clone()))
```

`crates/ritornello-plugin-cd/src/main.rs` — les deux `Play` :

```rust
self.issue(SourceAction::play("cdda://").finite())
// et
self.issue(SourceAction::play(format!("cdda://{n}")).finite())
```

Puis corriger les tests du cœur qui construisent la variante littérale :

Run: `cargo build --workspace 2>&1 | grep -n "missing field" | head`

Chaque site signalé devient `SourceAction::play(...)`.

- [ ] **Step 5 : lancer les tests, vérifier le succès**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6 : commit**

```bash
git add -A
git commit -m "feat(proto): Play porte un index de depart et la finitude du contenu

Deux champs strictement additifs, defauts identiques au comportement
historique : une trame du plugin radio reste octet pour octet la meme. Les
constructeurs play/starting_at/finite evitent qu'un champ ajoute plus tard
oblige a retoucher tous les appelants."
```

---

### Task 2 : `finite` pilote la relance, le reniflage disparaît

**Files:**
- Modify: `crates/ritornello-core/src/core.rs:812-819`
- Test: `crates/ritornello-core/src/core.rs` (module `tests`)

**Interfaces:**
- Consumes: `SourceAction::Play { uri, start, finite }` (Task 1).
- Produces: le cœur pose `expecting_stream = !finite`.

- [ ] **Step 1 : écrire le test qui échoue**

Dans le module `tests` de `core.rs`, à côté du test existant qui épingle la
régression `cdda://` :

```rust
#[tokio::test]
async fn la_fin_d_un_contenu_fini_previent_la_source_au_lieu_de_relancer() {
    // Régression mesurée au banc mpv : en fin de liste de fichiers, mpv passe
    // `idle` exactement comme lors d'une coupure de flux. Sans `finite`, le
    // cœur reniflait l'URI (`cdda://`) et un chemin de fichier tombait du
    // mauvais côté : relance exponentielle au lieu de l'arrêt propre, la
    // liste repartant en boucle depuis la première piste.
    let (mut core, _rx) = core_de_test().await;
    core.appliquer_action(SourceAction::play("/var/lib/ritornello/liste.m3u").finite())
        .await
        .unwrap();
    assert!(!core.expecting_stream, "un contenu fini ne doit pas armer la relance");

    // Et le contrôle inverse : un flux live garde la relance.
    core.appliquer_action(SourceAction::play("http://icecast/fip.mp3")).await.unwrap();
    assert!(core.expecting_stream, "un flux live doit rester relançable");
}
```

Si l'utilitaire `core_de_test()` n'existe pas sous ce nom, employer celui que
les tests voisins utilisent déjà pour construire un `Core` avec un lecteur
factice (chercher `fn core_de_test`, `fn core_avec` ou équivalent en tête du
module `tests`).

- [ ] **Step 2 : lancer le test, vérifier l'échec**

Run: `cargo test -p ritornello-core la_fin_d_un_contenu_fini`
Expected: FAIL — `expecting_stream` vaut `true` pour le m3u (le reniflage ne
reconnaît que `cdda://`).

- [ ] **Step 3 : appliquer `finite`**

Dans `core.rs`, remplacer le bloc `SourceAction::Play` :

```rust
SourceAction::Play { uri, start: _, finite } => {
    // C'est la Source qui déclare si ce qu'elle lance est un flux live
    // susceptible de tomber, ou un contenu fini dont l'inactivité de mpv
    // signale simplement la fin. Le cœur le devinait auparavant en
    // reniflant `cdda://` : un chemin de fichier tombait du mauvais côté,
    // et la fin de liste relançait la lecture en boucle.
    self.expecting_stream = !finite;
    self.player.play(&uri).await?;
}
```

- [ ] **Step 4 : lancer les tests, vérifier le succès**

Run: `cargo test -p ritornello-core`
Expected: PASS, y compris le test de non-régression `cdda://` existant (le
plugin cd déclarant désormais `finite`).

- [ ] **Step 5 : commit**

```bash
git add -A
git commit -m "fix(core): la Source declare la finitude, le coeur cesse de renifler l URI

Le renflement expecting_stream = !uri.starts_with(\"cdda://\") devinait ce que
seule la Source sait. Mesure au banc mpv : en fin de liste de fichiers mpv
passe idle comme lors d une coupure, donc un chemin de fichier declenchait la
relance exponentielle et rejouait la liste en boucle."
```

---

### Task 3 : `start` positionne la piste

**Files:**
- Modify: `crates/ritornello-core/src/player/mpv.rs:255-272` (voisinage de `play`)
- Modify: `crates/ritornello-core/src/core.rs` (bloc `Play`)
- Test: `crates/ritornello-core/src/player/mpv.rs`, `crates/ritornello-core/src/core.rs`

**Interfaces:**
- Consumes: `SourceAction::Play { start, .. }` (Task 1).
- Produces: `Player::set_playlist_pos(&self, n: i64) -> Result<()>`.

- [ ] **Step 1 : écrire le test qui échoue**

Dans le module `tests` de `core.rs` (le lecteur factice enregistre les appels
dans `player_calls`) :

```rust
#[tokio::test]
async fn un_play_avec_index_charge_puis_positionne() {
    // Ordre imposé : `loadfile` d'abord, `playlist-pos` ensuite. Mesuré au banc
    // mpv 0.37 — la liste est résolue dès le loadfile, donc l'enchaînement
    // immédiat atterrit sur la bonne piste, sans attente ni sondage.
    let (mut core, _rx) = core_de_test().await;
    core.appliquer_action(
        SourceAction::play("/var/lib/ritornello/liste.m3u").starting_at(4).finite(),
    )
    .await
    .unwrap();
    let calls = player_calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec!["play /var/lib/ritornello/liste.m3u".to_string(), "playlist-pos 4".to_string()]
    );
}

#[tokio::test]
async fn un_play_sans_index_ne_positionne_rien() {
    // Le chemin de la radio : aucune commande superflue sur la socket mpv.
    let (mut core, _rx) = core_de_test().await;
    core.appliquer_action(SourceAction::play("http://icecast/fip.mp3")).await.unwrap();
    let calls = player_calls.lock().unwrap().clone();
    assert_eq!(calls, vec!["play http://icecast/fip.mp3".to_string()]);
}
```

Ajouter la méthode correspondante au lecteur factice du module `tests` (celui
qui implémente déjà `play`, `stop`, `next`, `prev`), en poussant
`format!("playlist-pos {n}")` dans `player_calls`.

- [ ] **Step 2 : lancer les tests, vérifier l'échec**

Run: `cargo test -p ritornello-core un_play_avec_index`
Expected: FAIL — méthode `set_playlist_pos` absente du trait.

- [ ] **Step 3 : ajouter la commande au lecteur**

Dans `crates/ritornello-core/src/player/mpv.rs`, à côté de `next()`/`prev()` :

```rust
/// Positionne la lecture sur l'élément d'index `n` de la liste courante.
///
/// Employé juste après un `loadfile` désignant un `.m3u` : mpv résout la
/// liste dès la commande de chargement, il n'y a donc pas de dépliage
/// différé à attendre.
pub async fn set_playlist_pos(&self, n: i64) -> Result<()> {
    self.ipc.command(&[json!("set_property"), json!("playlist-pos"), json!(n)]).await?;
    Ok(())
}
```

Déclarer la méthode dans le trait du lecteur employé par `Core` (chercher le
trait implémenté par `MpvPlayer` et par le lecteur factice des tests).

- [ ] **Step 4 : l'appliquer dans le cœur**

```rust
SourceAction::Play { uri, start, finite } => {
    self.expecting_stream = !finite;
    self.player.play(&uri).await?;
    if let Some(n) = start {
        self.player.set_playlist_pos(n).await?;
    }
}
```

- [ ] **Step 5 : lancer les tests, vérifier le succès**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6 : commit**

```bash
git add -A
git commit -m "feat(core): un Play peut demarrer a un index de liste

Charge puis positionne, dans cet ordre. Le banc mpv 0.37 a montre que la liste
est resolue des le loadfile : l enchainement immediat atterrit sur la bonne
piste, ce qui evite un m3u pivote ou une seconde action de protocole."
```

---

### Task 4 : le cœur lit les tags que mpv lui envoie

**Files:**
- Modify: `crates/ritornello-core/src/player/mpv.rs:45-60` et `113-135`
- Modify: `crates/ritornello-core/src/core.rs` (traitement d'`Event`)
- Modify: `crates/ritornello-core/src/metadata.rs` (arbitrage)
- Modify: `docs/plugins.md` (« deux couches » devient trois)
- Test: `crates/ritornello-core/src/player/mpv.rs`, `crates/ritornello-core/src/metadata.rs`

**Interfaces:**
- Produces: `pub fn file_tags(data: &Value) -> Option<Morceau>` dans `mpv.rs`
  (`Morceau` et non `Enrichment` : c'est lui qui porte le champ `origin`),
  variante `Event::FileTags(Morceau)`, origine `"tags"`.

- [ ] **Step 1 : écrire les tests qui échouent**

Dans le module `tests` de `mpv.rs` :

```rust
#[test]
fn les_tags_dun_fichier_local_donnent_les_trois_champs() {
    // Charge réelle relevée au banc sur un mp3 (ID3). FFmpeg normalise les
    // clés : flac, ogg, opus, m4a et wav remontent sous les mêmes noms, donc
    // une seule grammaire à connaître.
    let data = serde_json::json!({
        "title": "So What", "artist": "Miles Davis",
        "album": "Kind of Blue", "encoder": "Lavf60.16.100"
    });
    let m = file_tags(&data).unwrap();
    assert_eq!(m.title.as_deref(), Some("So What"));
    assert_eq!(m.artist.as_deref(), Some("Miles Davis"));
    assert_eq!(m.album.as_deref(), Some("Kind of Blue"));
}

#[test]
fn les_cles_de_conteneur_m4a_sont_ignorees() {
    // Relevé au banc : un m4a fait remonter des clés de conteneur qui n'ont
    // rien à faire dans un affichage. On pioche trois clés nommées, on
    // n'absorbe jamais l'objet.
    let data = serde_json::json!({
        "title": "So What", "major_brand": "M4A ", "handler_name": "SoundHandler",
        "vendor_id": "[0][0][0][0]", "compatible_brands": "M4A mp42isom"
    });
    let m = file_tags(&data).unwrap();
    assert_eq!(m.title.as_deref(), Some("So What"));
    assert_eq!(m.artist, None);
    assert_eq!(m.album, None);
}

#[test]
fn une_charge_icy_ne_produit_aucun_tag() {
    // La garde qui protège la radio : certaines stations renseignent un
    // `title` valant le NOM DE LA STATION à côté d'un `icy-title` portant le
    // vrai morceau. Préférer le premier serait une régression. La présence
    // d'une clé `icy-*` signe un flux : le chemin ICY garde la main.
    let data = serde_json::json!({
        "icy-br": "128", "icy-title": "Mandrillus Sphynx - Bikwix", "title": "OUI FM"
    });
    assert!(file_tags(&data).is_none());
    assert_eq!(icy_title(&data).as_deref(), Some("Mandrillus Sphynx - Bikwix"));
}

#[test]
fn une_charge_sans_rien_de_lisible_ne_produit_aucun_tag() {
    assert!(file_tags(&serde_json::json!({"encoder": "Lavf60.16.100"})).is_none());
    assert!(file_tags(&serde_json::json!({})).is_none());
    assert!(file_tags(&serde_json::Value::Null).is_none());
}
```

- [ ] **Step 2 : lancer les tests, vérifier l'échec**

Run: `cargo test -p ritornello-core file_tags`
Expected: FAIL — `cannot find function 'file_tags'`.

- [ ] **Step 3 : extraire les tags**

Dans `mpv.rs`, à côté d'`icy_title` :

```rust
/// Les trois champs affichables des tags d'un fichier, tels que mpv les expose
/// dans la propriété `metadata`.
///
/// FFmpeg **normalise** les clés : ID3 (mp3), Vorbis comments (flac, ogg,
/// opus), atomes iTunes (m4a) et RIFF INFO (wav) remontent tous sous
/// `title` / `artist` / `album`. Une seule grammaire suffit donc.
///
/// Deux précautions, relevées au banc :
/// - on **pioche trois clés nommées** au lieu d'absorber l'objet : un m4a y
///   met aussi `major_brand`, `handler_name`, `vendor_id` ;
/// - la présence d'une clé `icy-*` **signe un flux**, et rend `None` : une
///   station peut renseigner un `title` valant son propre nom à côté d'un
///   `icy-title` portant le morceau, et le préférer serait une régression.
pub fn file_tags(data: &Value) -> Option<Morceau> {
    let map = data.as_object()?;
    if map.keys().any(|k| k.to_ascii_lowercase().starts_with("icy-")) {
        return None;
    }
    let champ = |nom: &str| {
        map.iter()
            .find(|(cle, _)| cle.eq_ignore_ascii_case(nom))
            .and_then(|(_, v)| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let m = Morceau {
        artist: champ("artist"),
        title: champ("title"),
        album: champ("album"),
        duration_s: None,
        origin: Some("tags".to_string()),
    };
    (!m.est_vide()).then_some(m)
}
```

Dans la boucle de traduction des événements, à la ligne qui traite
`Some("metadata")`, produire **soit** un titre ICY **soit** des tags :

```rust
(Some("metadata"), data) => icy_title(data)
    .map(Event::IcyTitle)
    .or_else(|| file_tags(data).map(Event::FileTags)),
```

Ajouter la variante `FileTags(Morceau)` à l'enum `Event`.

- [ ] **Step 4 : arbitrer dans le cœur**

Dans `core.rs`, à côté du traitement d'`Event::IcyTitle` :

```rust
// Couche intermédiaire : ce que le fichier déclare de lui-même. Elle
// s'intercale entre l'ICY (le moins fiable) et les plugins `metadata` (qui
// vont chercher au loin ce que le fichier ne dit pas). Comme pour l'ICY,
// aucun effet sur `retry_count` : un tag n'est pas une preuve de lecture.
Event::FileTags(morceau) => self.handle_file_tags(morceau),
```

Dans `metadata.rs`, la règle d'arbitrage — un plugin l'emporte toujours, les
tags l'emportent sur l'ICY :

```rust
/// Les tags que le fichier porte lui-même.
///
/// Se comportent comme l'ICY vis-à-vis des plugins `metadata` — un plugin
/// gagne en toutes circonstances tant que l'identité ne change pas — mais
/// **priment sur l'ICY** : quand les deux existent, le fichier en sait plus
/// que l'en-tête d'un flux. En pratique ils ne coexistent jamais,
/// `file_tags` rendant `None` dès qu'une clé ICY est présente.
pub fn set_file_tags(&mut self, morceau: Morceau) {
    if self.plugin_a_repondu {
        return;
    }
    self.morceau = morceau;
}
```

Adapter les noms aux structures réellement présentes dans `metadata.rs`
(chercher la fonction qui traite aujourd'hui le titre ICY et suivre sa forme).

- [ ] **Step 5 : écrire le test d'arbitrage**

```rust
#[test]
fn un_plugin_lemporte_toujours_sur_les_tags_du_fichier() {
    // Même règle que face à l'ICY, et pour la même raison : ce qu'un plugin a
    // appris doit rester affiché, sans quoi l'écran changerait de forme à
    // chaque rafraîchissement de mpv.
    let mut m = etat_metadata_de_test();
    m.set_plugin_enrichment("musicbrainz", enrichissement("Miles Davis", "So What"));
    m.set_file_tags(Morceau {
        title: Some("piste 03".into()),
        origin: Some("tags".into()),
        ..Default::default()
    });
    assert_eq!(m.morceau().title.as_deref(), Some("So What"));
    assert_eq!(m.morceau().origin.as_deref(), Some("musicbrainz"));
}
```

- [ ] **Step 6 : lancer les tests, vérifier le succès**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 7 : mettre `docs/plugins.md` à jour**

Section « Now-playing metadata » : la liste « Two layers stack up » devient
trois, dans l'ordre ICY < tags du fichier < plugin, en disant que la couche
tags ne s'applique qu'en l'absence de clé ICY et pourquoi.

- [ ] **Step 8 : commit**

```bash
git add -A
git commit -m "feat(core): les tags du fichier joue alimentent le morceau affiche

mpv les envoie deja dans la propriete metadata, le coeur n en extrayait que
icy-title et jetait le reste. Trois cles nommees (FFmpeg normalise ID3, Vorbis,
atomes iTunes et RIFF INFO), jamais l objet entier : un m4a y met aussi des
cles de conteneur.

La couche ne s applique qu en l absence de cle icy-* : une station peut
renseigner un title valant son propre nom a cote d un icy-title portant le
morceau. Sert toute source jouant un fichier taggue, sans qu aucune n ait rien
a declarer."
```

---

# Phase 2 — La Source, en local

### Task 5 : le crate, les racines et leur validation

**Files:**
- Create: `crates/ritornello-plugin-files/Cargo.toml`
- Create: `crates/ritornello-plugin-files/src/roots.rs`
- Create: `crates/ritornello-plugin-files/src/locales/en.toml`
- Modify: `Cargo.toml` (membre du workspace)

**Interfaces:**
- Produces: `Root`, `RootKind`, `Roots`, `RootError`,
  `Roots::load(&Path) -> anyhow::Result<Roots>`,
  `Roots::validate(&self) -> Result<(), RootError>`,
  `Roots::by_name(&self, &str) -> Option<&Root>`,
  `Root::base_dir(&self) -> PathBuf`,
  `RootError::message(&self, &Catalog) -> String`.

- [ ] **Step 1 : déclarer le crate**

`crates/ritornello-plugin-files/Cargo.toml` :

```toml
[package]
name = "ritornello-plugin-files"
version = "0.1.0"
edition = "2021"

# Une cible `lib` est INDISPENSABLE ici : deux `[[bin]]` d'un même crate ne
# partagent pas leurs modules. Sans elle, le binaire de montage ne pourrait pas
# employer `roots.rs`, et l'argument qui justifie de les loger ensemble — le
# côté privilégié et le côté qui écrit la configuration lisent exactement la
# même grammaire — tomberait.
[lib]
name = "ritornello_plugin_files"
path = "src/lib.rs"

[[bin]]
name = "ritornello-plugin-files"
path = "src/main.rs"

[[bin]]
name = "ritornello-media-mount"
path = "src/bin/media-mount.rs"

[dependencies]
ritornello-proto = { path = "../ritornello-proto" }
ritornello-plugin-sdk = { path = "../ritornello-plugin-sdk" }
ritornello-i18n = { path = "../ritornello-i18n" }
anyhow = "1"
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "process", "fs", "sync"] }
tracing = "0.1"
tracing-subscriber = "0.3"

[dev-dependencies]
tempfile = "3"
```

Aligner les versions sur celles déjà employées par
`crates/ritornello-plugin-radio/Cargo.toml`. Ajouter
`"crates/ritornello-plugin-files"` aux `members` du `Cargo.toml` racine.

Créer `src/lib.rs`, qui porte tout ce que les deux binaires partagent :

```rust
//! Ce que le plugin et le binaire racine de montage ont en commun.
//!
//! Deux `[[bin]]` d'un même crate ne partagent pas leurs modules : cette
//! bibliothèque est ce qui garantit que le côté privilégié et le côté qui
//! écrit la configuration lisent exactement la même grammaire.

pub mod m3u;
pub mod mount_options;
pub mod playlist;
pub mod roots;
pub mod scan;

/// Catalogue anglais embarqué, replié sur quand la locale demandée manque.
pub const FILES_EN: &str = include_str!("locales/en.toml");
```

Les modules propres au plugin (`admin`, `mount`, `state`, `store`) restent
déclarés dans `main.rs` : le binaire de montage n'en a que faire, et les y
mettre lui imposerait des dépendances (tokio, le SDK) qu'un binaire lancé par
systemd n'a aucune raison de tirer.

Dans les tests de `roots.rs`, le catalogue se charge donc par `crate::FILES_EN`
— `crate` désignant ici la bibliothèque.

- [ ] **Step 2 : écrire les tests qui échouent**

`crates/ritornello-plugin-files/src/roots.rs`, module `tests` :

```rust
#[test]
fn un_nom_de_racine_hors_grammaire_est_refuse() {
    // Le nom devient un composant de chemin (/mnt/ritornello/<name>) ET un nom
    // de fichier d'identifiants. Tout ce qui n'est pas [a-z0-9-] ouvrirait une
    // traversée de répertoire du côté privilégié.
    for mauvais in ["../evasion", "Nas", "nas/musique", "", "nas musique", "-nas"] {
        let r = roots_avec(Root { name: mauvais.into(), ..racine_smb() });
        assert!(
            matches!(r.validate(), Err(RootError::BadName { .. })),
            "accepte a tort : {mauvais:?}"
        );
    }
}

#[test]
fn une_virgule_dans_l_hote_ou_le_partage_est_refusee() {
    // LA faille à ne pas manquer : les options de mount.cifs sont separees par
    // des virgules. Un hote « nas,uid=0 » injecterait une option dans la ligne
    // de montage executee par root.
    let r = roots_avec(Root { host: "nas,uid=0".into(), ..racine_smb() });
    assert!(matches!(r.validate(), Err(RootError::BadHost { .. })));
    let r = roots_avec(Root { share: "musique,rw".into(), ..racine_smb() });
    assert!(matches!(r.validate(), Err(RootError::BadShare { .. })));
}

#[test]
fn un_sous_chemin_qui_remonte_est_refuse() {
    let r = roots_avec(Root { subpath: Some("../../etc".into()), ..racine_smb() });
    assert!(matches!(r.validate(), Err(RootError::BadSubpath { .. })));
}

#[test]
fn deux_racines_de_meme_nom_sont_refusees() {
    // Elles se disputeraient le meme point de montage et le meme fichier
    // d'identifiants.
    let r = Roots { root: vec![racine_smb(), racine_smb()] };
    assert!(matches!(r.validate(), Err(RootError::DuplicateName { .. })));
}

#[test]
fn une_racine_locale_veut_un_chemin_absolu() {
    let r = roots_avec(Root {
        kind: RootKind::Local,
        path: Some("media/usb".into()),
        ..racine_locale()
    });
    assert!(matches!(r.validate(), Err(RootError::RelativeLocalPath { .. })));
}

#[test]
fn une_racine_valide_passe_et_son_repertoire_est_impose() {
    let r = roots_avec(racine_smb());
    assert!(r.validate().is_ok());
    // Le point de montage n'est JAMAIS lu depuis la configuration.
    assert_eq!(
        r.root[0].base_dir(),
        std::path::PathBuf::from("/mnt/ritornello/nas/Albums")
    );
}

#[test]
fn chaque_refus_resout_contre_le_catalogue_embarque() {
    // Catalog::get rend la cle quand il ne la trouve pas : sans ce test, une
    // faute de frappe afficherait « bad_share_name » a l'ecran.
    let catalog = Catalog::load("files", "en", std::path::Path::new("/inexistant"), crate::FILES_EN);
    let messages = [
        RootError::BadName { name: "x/y".into() }.message(&catalog),
        RootError::BadHost { host: "a,b".into() }.message(&catalog),
        RootError::BadShare { share: "a,b".into() }.message(&catalog),
        RootError::BadSubpath { subpath: "..".into() }.message(&catalog),
        RootError::DuplicateName { name: "nas".into() }.message(&catalog),
        RootError::RelativeLocalPath { path: "media/usb".into() }.message(&catalog),
        RootError::UnknownKind { kind: "nfs".into() }.message(&catalog),
    ];
    for m in &messages {
        assert!(m.contains(' '), "message reduit a une cle brute : {m:?}");
    }
}
```

Ajouter les fabriques de test `racine_smb()`, `racine_locale()` et
`roots_avec(root)`.

- [ ] **Step 3 : lancer les tests, vérifier l'échec**

Run: `cargo test -p ritornello-plugin-files roots`
Expected: FAIL — types absents.

- [ ] **Step 4 : implémenter le modèle et la validation**

```rust
use ritornello_i18n::Catalog;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Racine du plugin : un répertoire où il a le droit de regarder.
///
/// Un disque USB, un dossier local et un partage SMB sont la même chose pour
/// tout le reste du plugin ; le montage n'est qu'un détail du genre `Smb`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Root {
    pub name: String,
    pub kind: RootKind,
    /// Genre `Local` uniquement : chemin absolu du répertoire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub share: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub domain: String,
    /// Retire `ro` des options de montage. Faux par défaut : enregistrer une
    /// liste sur le partage est un choix explicite, pas un état de fait.
    #[serde(default)]
    pub writable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RootKind {
    Local,
    Smb,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Roots {
    #[serde(default)]
    pub root: Vec<Root>,
}

/// Erreur de validation typée : le texte utilisateur est produit à la
/// frontière via `message(&Catalog)`. `Display` fournit une version anglaise
/// pour les journaux internes, hors périmètre i18n.
#[derive(Debug, Clone, PartialEq)]
pub enum RootError {
    BadName { name: String },
    BadHost { host: String },
    BadShare { share: String },
    BadSubpath { subpath: String },
    DuplicateName { name: String },
    RelativeLocalPath { path: String },
    UnknownKind { kind: String },
}

/// Racine des points de montage. Constante, jamais lue depuis la
/// configuration : un chemin libre serait un chemin à valider.
pub const MOUNT_ROOT: &str = "/mnt/ritornello";

/// Grammaire d'un nom de racine : il devient un composant de chemin et un nom
/// de fichier d'identifiants.
fn nom_valide(nom: &str) -> bool {
    let mut chars = nom.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    nom.len() <= 32 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Un champ qui atterrit dans une ligne d'options `mount.cifs` : la virgule
/// sépare les options, l'espace casse l'analyse, `..` remonte l'arborescence.
fn champ_sur(valeur: &str) -> bool {
    !valeur.is_empty()
        && !valeur.contains(',')
        && !valeur.contains(char::is_whitespace)
        && !valeur.contains("..")
        && !valeur.contains('\0')
}

impl Roots {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let roots: Roots = toml::from_str(&text)?;
        roots.validate()?;
        Ok(roots)
    }

    pub fn validate(&self) -> Result<(), RootError> {
        let mut vus: Vec<&str> = Vec::new();
        for r in &self.root {
            if !nom_valide(&r.name) {
                return Err(RootError::BadName { name: r.name.clone() });
            }
            if vus.contains(&r.name.as_str()) {
                return Err(RootError::DuplicateName { name: r.name.clone() });
            }
            vus.push(&r.name);
            match r.kind {
                RootKind::Local => {
                    let p = r.path.clone().unwrap_or_default();
                    if !Path::new(&p).is_absolute() {
                        return Err(RootError::RelativeLocalPath { path: p });
                    }
                }
                RootKind::Smb => {
                    if !champ_sur(&r.host) {
                        return Err(RootError::BadHost { host: r.host.clone() });
                    }
                    if !champ_sur(&r.share) {
                        return Err(RootError::BadShare { share: r.share.clone() });
                    }
                    if let Some(s) = &r.subpath {
                        if !champ_sur(s) || s.starts_with('/') {
                            return Err(RootError::BadSubpath { subpath: s.clone() });
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn by_name(&self, nom: &str) -> Option<&Root> {
        self.root.iter().find(|r| r.name == nom)
    }
}

impl Root {
    /// Répertoire réellement parcouru. Pour un partage, le point de montage
    /// **imposé**, éventuellement suivi du sous-chemin déclaré.
    pub fn base_dir(&self) -> PathBuf {
        match self.kind {
            RootKind::Local => PathBuf::from(self.path.clone().unwrap_or_default()),
            RootKind::Smb => {
                let mut p = PathBuf::from(MOUNT_ROOT).join(&self.name);
                if let Some(s) = &self.subpath {
                    p = p.join(s);
                }
                p
            }
        }
    }

    /// Fichier d'identifiants consommé par `mount.cifs`.
    pub fn credentials_path(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.cred", self.name))
    }
}

impl RootError {
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            RootError::BadName { name } => catalog.get("bad_root_name").replace("{name}", name),
            RootError::BadHost { host } => catalog.get("bad_host").replace("{host}", host),
            RootError::BadShare { share } => catalog.get("bad_share").replace("{share}", share),
            RootError::BadSubpath { subpath } => {
                catalog.get("bad_subpath").replace("{path}", subpath)
            }
            RootError::DuplicateName { name } => {
                catalog.get("duplicate_root").replace("{name}", name)
            }
            RootError::RelativeLocalPath { path } => {
                catalog.get("relative_local_path").replace("{path}", path)
            }
            RootError::UnknownKind { kind } => catalog.get("unknown_kind").replace("{kind}", kind),
        }
    }
}
```

Écrire aussi `impl std::fmt::Display for RootError` (phrases anglaises pour les
journaux) et `impl std::error::Error for RootError {}`, sur le modèle de
`ValidationError` du plugin radio.

- [ ] **Step 5 : écrire le catalogue anglais**

`crates/ritornello-plugin-files/src/locales/en.toml` :

```toml
bad_root_name = "invalid root name \"{name}\": use lowercase letters, digits and dashes only"
bad_host = "invalid server address \"{host}\": no commas, spaces or \"..\""
bad_share = "invalid share name \"{share}\": no commas, spaces or \"..\""
bad_subpath = "invalid subfolder \"{path}\": it must be relative and must not go up"
duplicate_root = "a root named \"{name}\" already exists"
relative_local_path = "the local folder \"{path}\" must be an absolute path"
unknown_kind = "unknown root kind \"{kind}\""
```

`FILES_EN` est déjà exposé par `src/lib.rs` (étape 1) : les deux binaires et
tous les modules y accèdent par `crate::FILES_EN` ou
`ritornello_plugin_files::FILES_EN` selon le côté.

- [ ] **Step 6 : lancer les tests, vérifier le succès**

Run: `cargo test -p ritornello-plugin-files`
Expected: PASS.

- [ ] **Step 7 : commit**

```bash
git add -A
git commit -m "feat(plugin-files): le crate et le modele de racines valide

Une racine est un repertoire ou le plugin a le droit de regarder : disque
local, dossier de l appareil ou partage SMB, meme abstraction pour tout le
reste du plugin.

La validation est stricte parce qu elle protege un binaire racine : le point de
montage n est jamais lu depuis la configuration, et la virgule est refusee dans
l hote et le partage -- les options de mount.cifs etant separees par des
virgules, un hote « nas,uid=0 » injecterait une option dans la ligne executee
par root."
```

---

### Task 6 : lecture, écriture et résolution des m3u

**Files:**
- Create: `crates/ritornello-plugin-files/src/m3u.rs`
- Test: même fichier, module `tests`

**Interfaces:**
- Consumes: rien.
- Produces: `Entry { path: PathBuf, title: Option<String>, duration_s: Option<u32> }`,
  `Parsed { entries: Vec<Entry>, unresolved: Vec<String> }`,
  `parse(text: &str, m3u_dir: &Path, root: &Path) -> Parsed`,
  `render(entries: &[Entry], base: Option<&Path>) -> String`.

- [ ] **Step 1 : écrire les tests qui échouent**

```rust
#[test]
fn un_m3u_relatif_se_resout_contre_le_repertoire_du_fichier() {
    let dir = tempfile::tempdir().unwrap();
    let album = dir.path().join("Album");
    std::fs::create_dir_all(&album).unwrap();
    std::fs::write(album.join("01.mp3"), b"").unwrap();
    let texte = "#EXTM3U\n#EXTINF:245,Miles Davis - So What\nAlbum/01.mp3\n";
    let p = parse(texte, dir.path(), dir.path());
    assert_eq!(p.entries.len(), 1);
    assert_eq!(p.entries[0].path, album.join("01.mp3"));
    assert_eq!(p.entries[0].title.as_deref(), Some("Miles Davis - So What"));
    assert_eq!(p.entries[0].duration_s, Some(245));
    assert!(p.unresolved.is_empty());
}

#[test]
fn un_chemin_windows_ecrit_par_le_nas_se_rattrape_sous_la_racine() {
    // Un m3u produit par le NAS porte souvent des chemins qui n'ont de sens
    // que chez lui. On retire le prefixe de lecteur ou UNC et on tente sous la
    // racine, plutot que de jeter l'entree.
    let dir = tempfile::tempdir().unwrap();
    let album = dir.path().join("Musique/Album");
    std::fs::create_dir_all(&album).unwrap();
    std::fs::write(album.join("02.mp3"), b"").unwrap();
    let texte = "#EXTM3U\nZ:\\Musique\\Album\\02.mp3\n";
    let p = parse(texte, dir.path(), dir.path());
    assert_eq!(p.entries.len(), 1, "non resolu : {:?}", p.unresolved);
    assert_eq!(p.entries[0].path, album.join("02.mp3"));
}

#[test]
fn une_entree_introuvable_est_rapportee_et_non_jetee() {
    // Une liste qui retrecit sans rien dire est un defaut qu'on met des mois a
    // attribuer : l'entree doit remonter jusqu'a la page.
    let dir = tempfile::tempdir().unwrap();
    let texte = "#EXTM3U\n/volume1/music/absent.mp3\n";
    let p = parse(texte, dir.path(), dir.path());
    assert!(p.entries.is_empty());
    assert_eq!(p.unresolved, vec!["/volume1/music/absent.mp3".to_string()]);
}

#[test]
fn les_commentaires_et_lignes_vides_sont_ignores() {
    let dir = tempfile::tempdir().unwrap();
    let p = parse("#EXTM3U\n\n# un commentaire\n\n", dir.path(), dir.path());
    assert!(p.entries.is_empty());
    assert!(p.unresolved.is_empty());
}

#[test]
fn le_rendu_est_relatif_quand_une_base_est_donnee() {
    // C'est ce qui rend la liste reutilisable par un autre lecteur et
    // survivante a un changement de point de montage.
    let base = std::path::Path::new("/mnt/ritornello/nas");
    let entries = vec![Entry {
        path: base.join("Album/01.mp3"),
        title: Some("So What".into()),
        duration_s: Some(245),
    }];
    let texte = render(&entries, Some(base));
    assert_eq!(texte, "#EXTM3U\n#EXTINF:245,So What\nAlbum/01.mp3\n");
}

#[test]
fn le_rendu_est_absolu_sans_base() {
    // La liste destinee a mpv, ecrite dans le repertoire d'etat : elle ne doit
    // dependre d'aucun repertoire courant.
    let entries = vec![Entry {
        path: "/mnt/ritornello/nas/Album/01.mp3".into(),
        title: None,
        duration_s: None,
    }];
    assert_eq!(
        render(&entries, None),
        "#EXTM3U\n#EXTINF:-1,01\n/mnt/ritornello/nas/Album/01.mp3\n"
    );
}
```

- [ ] **Step 2 : lancer les tests, vérifier l'échec**

Run: `cargo test -p ritornello-plugin-files m3u`
Expected: FAIL — module absent.

- [ ] **Step 3 : implémenter**

```rust
//! Lecture et écriture de listes m3u.
//!
//! Deux objets distincts passent par ici, et les confondre serait une erreur :
//! la **liste utilisateur** (éditée, enregistrée, rechargeable, à chemins
//! relatifs quand c'est possible) et la **liste donnée à mpv** (générée, à
//! chemins absolus, jamais montrée).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub title: Option<String>,
    pub duration_s: Option<u32>,
}

impl Entry {
    /// Nom affichable : le titre `#EXTINF` s'il existe, sinon le nom du
    /// fichier sans extension. C'est ce que la Source déclare en
    /// `preset_name`, de sorte que l'écran ne soit jamais muet même sans
    /// aucune métadonnée.
    pub fn display_name(&self) -> String {
        self.title.clone().unwrap_or_else(|| {
            self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    pub entries: Vec<Entry>,
    /// Entrées qu'aucune règle n'a su résoudre. **Rapportées**, jamais
    /// supprimées en silence.
    pub unresolved: Vec<String>,
}

/// Résout une entrée brute, dans l'ordre : relative au répertoire du m3u,
/// absolue telle quelle, puis absolue d'un autre système ramenée sous la
/// racine.
fn resolve(brut: &str, m3u_dir: &Path, root: &Path) -> Option<PathBuf> {
    let brut = brut.trim();
    // 1. relative au répertoire du m3u
    let rel = m3u_dir.join(brut.replace('\\', "/"));
    if rel.is_file() {
        return Some(rel);
    }
    // 2. absolue telle quelle
    let abs = Path::new(brut);
    if abs.is_absolute() && abs.is_file() {
        return Some(abs.to_path_buf());
    }
    // 3. absolue d'un autre système : on retire un préfixe de lecteur
    //    (`Z:`), un préfixe UNC (`\\serveur\partage`) ou la racine, et on
    //    tente sous la racine locale.
    let normalise = brut.replace('\\', "/");
    let sans_prefixe = normalise
        .trim_start_matches(|c: char| c.is_ascii_alphabetic())
        .strip_prefix(':')
        .unwrap_or(&normalise);
    for candidat in sans_prefixe.split('/').enumerate().filter_map(|(i, _)| {
        let reste: Vec<&str> = sans_prefixe.split('/').skip(i).filter(|s| !s.is_empty()).collect();
        (!reste.is_empty()).then(|| root.join(reste.join("/")))
    }) {
        if candidat.is_file() {
            return Some(candidat);
        }
    }
    None
}

pub fn parse(text: &str, m3u_dir: &Path, root: &Path) -> Parsed {
    let mut out = Parsed::default();
    let mut en_attente: Option<(Option<u32>, Option<String>)> = None;
    for ligne in text.lines() {
        let ligne = ligne.trim();
        if ligne.is_empty() {
            continue;
        }
        if let Some(reste) = ligne.strip_prefix("#EXTINF:") {
            let (duree, titre) = match reste.split_once(',') {
                Some((d, t)) => (
                    d.trim().parse::<i64>().ok().filter(|n| *n > 0).map(|n| n as u32),
                    (!t.trim().is_empty()).then(|| t.trim().to_string()),
                ),
                None => (None, None),
            };
            en_attente = Some((duree, titre));
            continue;
        }
        if ligne.starts_with('#') {
            continue;
        }
        let (duree, titre) = en_attente.take().unwrap_or((None, None));
        match resolve(ligne, m3u_dir, root) {
            Some(path) => out.entries.push(Entry { path, title: titre, duration_s: duree }),
            None => out.unresolved.push(ligne.to_string()),
        }
    }
    out
}

pub fn render(entries: &[Entry], base: Option<&Path>) -> String {
    let mut s = String::from("#EXTM3U\n");
    for e in entries {
        let duree = e.duration_s.map(|d| d.to_string()).unwrap_or_else(|| "-1".into());
        s.push_str(&format!("#EXTINF:{duree},{}\n", e.display_name()));
        let chemin = base
            .and_then(|b| e.path.strip_prefix(b).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| e.path.to_string_lossy().into_owned());
        s.push_str(&chemin);
        s.push('\n');
    }
    s
}
```

- [ ] **Step 4 : lancer les tests, vérifier le succès**

Run: `cargo test -p ritornello-plugin-files m3u`
Expected: PASS. Si `un_chemin_windows_ecrit_par_le_nas_se_rattrape_sous_la_racine`
échoue, ajuster la boucle de la règle 3 : elle doit essayer les suffixes
successifs du chemin (`Musique/Album/02.mp3`, puis `Album/02.mp3`, puis
`02.mp3`) sous la racine.

- [ ] **Step 5 : commit**

```bash
git add -A
git commit -m "feat(plugin-files): analyse et rendu m3u, avec rattrapage des chemins etrangers

Un m3u ecrit par le NAS porte souvent des chemins qui n ont de sens que chez
lui (Z:\\Musique\\..., /volume1/music/...). Trois regles de resolution, et
surtout : ce qui reste irresolu est rapporte, jamais supprime en silence -- une
liste qui retrecit sans rien dire est un defaut qu on met des mois a attribuer."
```

---

### Task 7 : marche récursive

**Files:**
- Create: `crates/ritornello-plugin-files/src/scan.rs`
- Test: même fichier

**Interfaces:**
- Produces: `MAX_TRACKS: usize = 2000`, `is_audio(&Path) -> bool`,
  `walk(dir: &Path, cap: usize) -> Result<Vec<PathBuf>, ScanError>`,
  `ScanError { TooMany { cap }, Io { path, source } }`,
  `ScanError::message(&self, &Catalog) -> String`.

- [ ] **Step 1 : écrire les tests qui échouent**

```rust
#[test]
fn seuls_les_fichiers_audio_sont_retenus_quelle_que_soit_la_casse() {
    let dir = tempfile::tempdir().unwrap();
    for nom in ["a.mp3", "b.FLAC", "c.Opus", "pochette.jpg", "notes.txt", "sans-extension"] {
        std::fs::write(dir.path().join(nom), b"").unwrap();
    }
    let mut noms: Vec<String> = walk(dir.path(), MAX_TRACKS)
        .unwrap()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    noms.sort();
    assert_eq!(noms, vec!["a.mp3", "b.FLAC", "c.Opus"]);
}

#[test]
fn la_marche_est_recursive_et_ordonnee() {
    // L'ordre doit etre stable : deux ajouts du meme dossier produisent la
    // meme liste, sinon les numeros de presentation changeraient d'un jour a
    // l'autre.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("B/sous")).unwrap();
    std::fs::create_dir_all(dir.path().join("A")).unwrap();
    std::fs::write(dir.path().join("A/02.mp3"), b"").unwrap();
    std::fs::write(dir.path().join("A/01.mp3"), b"").unwrap();
    std::fs::write(dir.path().join("B/sous/03.mp3"), b"").unwrap();
    let trouves = walk(dir.path(), MAX_TRACKS).unwrap();
    let relatifs: Vec<String> = trouves
        .iter()
        .map(|p| p.strip_prefix(dir.path()).unwrap().to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(relatifs, vec!["A/01.mp3", "A/02.mp3", "B/sous/03.mp3"]);
}

#[test]
fn le_plafond_est_refuse_et_non_tronque_en_silence() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..5 {
        std::fs::write(dir.path().join(format!("{i}.mp3")), b"").unwrap();
    }
    assert!(matches!(walk(dir.path(), 3), Err(ScanError::TooMany { cap: 3 })));
}

#[cfg(unix)]
#[test]
fn une_boucle_de_liens_symboliques_ne_fait_pas_tourner_la_marche() {
    // Sans garde, un lien pointant vers un ancetre fait tourner la marche
    // jusqu'au plafond, en produisant des doublons de chemins de plus en plus
    // longs. Le symptome ressemble a une bibliotheque enorme, pas a un bug.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sous")).unwrap();
    std::fs::write(dir.path().join("sous/a.mp3"), b"").unwrap();
    std::os::unix::fs::symlink(dir.path(), dir.path().join("sous/boucle")).unwrap();
    let trouves = walk(dir.path(), MAX_TRACKS).unwrap();
    assert_eq!(trouves.len(), 1, "la boucle a ete suivie : {trouves:?}");
}
```

- [ ] **Step 2 : lancer les tests, vérifier l'échec**

Run: `cargo test -p ritornello-plugin-files scan`
Expected: FAIL — module absent.

- [ ] **Step 3 : implémenter**

```rust
//! Marche récursive d'un répertoire, avec filtre d'extensions, garde contre
//! les boucles de liens symboliques et plafond.

use ritornello_i18n::Catalog;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Plafond d'une liste. Protège trois choses à la fois : la charge utile JSON
/// servie à la page, l'écriture du m3u, et la playlist mpv.
pub const MAX_TRACKS: usize = 2000;

const EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "m4a", "aac", "wav", "wma", "aiff", "ape", "wv", "mpc",
];

#[derive(Debug)]
pub enum ScanError {
    TooMany { cap: usize },
    Io { path: String, source: std::io::Error },
}

pub fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTENSIONS.iter().any(|k| k.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// Parcourt `dir` récursivement et rend les fichiers audio, **triés** pour que
/// deux ajouts du même dossier produisent la même liste — sans quoi les
/// numéros de présélection changeraient d'un jour à l'autre.
///
/// La garde anti-boucle mémorise les répertoires **canonisés** déjà visités :
/// un lien symbolique pointant vers un ancêtre ferait sinon tourner la marche
/// jusqu'au plafond, avec un symptôme qui ressemble à une bibliothèque énorme
/// plutôt qu'à un défaut.
pub fn walk(dir: &Path, cap: usize) -> Result<Vec<PathBuf>, ScanError> {
    let mut out = Vec::new();
    let mut vus: HashSet<PathBuf> = HashSet::new();
    marche(dir, cap, &mut out, &mut vus)?;
    out.sort();
    Ok(out)
}

fn marche(
    dir: &Path,
    cap: usize,
    out: &mut Vec<PathBuf>,
    vus: &mut HashSet<PathBuf>,
) -> Result<(), ScanError> {
    let canon = dir
        .canonicalize()
        .map_err(|e| ScanError::Io { path: dir.display().to_string(), source: e })?;
    if !vus.insert(canon) {
        return Ok(());
    }
    let lecture = std::fs::read_dir(dir)
        .map_err(|e| ScanError::Io { path: dir.display().to_string(), source: e })?;
    let mut sous_dossiers = Vec::new();
    for entree in lecture {
        let entree =
            entree.map_err(|e| ScanError::Io { path: dir.display().to_string(), source: e })?;
        let chemin = entree.path();
        // `metadata` (et non `symlink_metadata`) : un lien vers un dossier
        // réel doit être suivi, c'est la boucle qu'on refuse, pas le lien.
        let meta = match std::fs::metadata(&chemin) {
            Ok(m) => m,
            Err(_) => continue, // lien cassé, permission refusée : on passe
        };
        if meta.is_dir() {
            sous_dossiers.push(chemin);
        } else if meta.is_file() && is_audio(&chemin) {
            if out.len() >= cap {
                return Err(ScanError::TooMany { cap });
            }
            out.push(chemin);
        }
    }
    sous_dossiers.sort();
    for d in sous_dossiers {
        marche(&d, cap, out, vus)?;
    }
    Ok(())
}

impl ScanError {
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            ScanError::TooMany { cap } => {
                catalog.get("too_many_tracks").replace("{cap}", &cap.to_string())
            }
            ScanError::Io { path, .. } => catalog.get("scan_io_error").replace("{path}", path),
        }
    }
}
```

Ajouter au catalogue anglais :

```toml
too_many_tracks = "this folder holds more than {cap} tracks: narrow it down or add its subfolders one by one"
scan_io_error = "could not read \"{path}\": the share may be unreachable"
```

Ajouter `ScanError` au test `chaque_refus_resout_contre_le_catalogue_embarque`
de la tâche 5 (ou écrire son propre test du même modèle dans `scan.rs`).

- [ ] **Step 4 : lancer les tests, vérifier le succès**

Run: `cargo test -p ritornello-plugin-files scan`
Expected: PASS.

- [ ] **Step 5 : commit**

```bash
git add -A
git commit -m "feat(plugin-files): marche recursive triee, plafonnee et sans boucle

Le tri rend l ajout reproductible : sans lui, deux ajouts du meme dossier
donneraient des numeros de preselection differents d un jour a l autre.

La garde anti-boucle memorise les repertoires canonises : un lien symbolique
vers un ancetre ferait sinon tourner la marche jusqu au plafond, avec un
symptome qui ressemble a une bibliotheque enorme plutot qu a un defaut."
```

---

### Task 8 : le modèle de liste et la moitié Source

**Files:**
- Create: `crates/ritornello-plugin-files/src/playlist.rs`
- Create: `crates/ritornello-plugin-files/src/main.rs`
- Test: les deux fichiers

**Interfaces:**
- Consumes: `m3u::{Entry, render}` (Task 6), `SourceAction::play/starting_at/finite` (Task 1).
- Produces: `Playlist { entries: Vec<Entry>, index: usize }`,
  `Playlist::preset_count(&self) -> u8`,
  `Playlist::select(&mut self, n: u8) -> bool`,
  `Playlist::current(&self) -> Option<&Entry>`,
  `Playlist::write_for_mpv(&self, path: &Path) -> std::io::Result<()>`,
  `FilesSource` implémentant `SourcePlugin`,
  identité `{"kind":"file","path":"<absolu>"}`.

- [ ] **Step 1 : écrire les tests qui échouent**

`playlist.rs` :

```rust
#[test]
fn le_compte_de_preselections_est_plafonne_a_99() {
    // `preset` est un u8 et la plage va de 1 a 99. Au-dela, les pistes restent
    // atteignables par next/prev et par la liste web, mais aucun chiffre ne
    // les designe -- ce n'est pas contourne, c'est declare honnetement.
    let p = liste_de(150);
    assert_eq!(p.preset_count(), 99);
    let p = liste_de(12);
    assert_eq!(p.preset_count(), 12);
    assert_eq!(Playlist::default().preset_count(), 0);
}

#[test]
fn selectionner_hors_bornes_echoue_sans_bouger_l_index() {
    let mut p = liste_de(3);
    p.index = 1;
    assert!(!p.select(0), "le zero n'est pas une presentation");
    assert!(!p.select(4));
    assert_eq!(p.index, 1, "un echec ne doit pas deplacer la lecture");
    assert!(p.select(3));
    assert_eq!(p.index, 2);
}
```

`main.rs` :

```rust
#[tokio::test]
async fn activer_une_liste_vide_ne_lance_rien_et_le_dit() {
    let mut s = source_de_test(Playlist::default());
    let out = s.activate().await;
    assert!(matches!(out.action, SourceAction::Noop));
    assert_eq!(out.preset_count, Some(0));
    assert!(out.status.is_some(), "le statut doit dire pourquoi rien ne joue");
}

#[tokio::test]
async fn activer_reprend_a_la_piste_memorisee() {
    // La reprise apres redemarrage : sans `start`, la lecture repartirait a la
    // premiere piste a chaque demarrage de l'appareil.
    let mut p = liste_de(5);
    p.index = 3;
    let mut s = source_de_test(p);
    let out = s.activate().await;
    match out.action {
        SourceAction::Play { start, finite, .. } => {
            assert_eq!(start, Some(3));
            assert!(finite, "une liste de fichiers a une fin normale");
        }
        autre => panic!("attendu un Play, obtenu {autre:?}"),
    }
    assert_eq!(out.preset, Some(4));
}

#[tokio::test]
async fn une_piste_inexistante_donne_un_message_ephemere_sans_couper_la_lecture() {
    // Meme regle que la preselection vide de la radio : rien n'a ete lance,
    // donc la piste precedente joue toujours et doit reparaitre a l'ecran.
    // Surtout : aucune declaration d'identite, sans quoi les metadonnees du
    // morceau en cours seraient effacees.
    let mut s = source_de_test(liste_de(3));
    let out = s.select(9).await;
    assert!(matches!(out.action, SourceAction::Noop));
    assert!(out.transient, "le message doit s'effacer de lui-meme");
    assert!(out.identity.is_none(), "declarer un arret serait faux");
    assert_eq!(out.preset_count, Some(3));
}

#[tokio::test]
async fn le_statut_est_redeclare_a_chaque_trame() {
    // PIEGE : `status` a la convention INVERSE de `preset`. Absent veut dire
    // « pas de statut », et non « garde le precedent ». Une Source qui
    // l'omettrait verrait son affichage s'effacer tout seul.
    let mut s = source_de_test(liste_de(3));
    for out in [s.activate().await, s.select(2).await, s.next().await] {
        assert!(out.status.is_some(), "statut omis : l'ecran s'effacerait");
    }
}

#[tokio::test]
async fn l_avance_automatique_recale_index_identite_et_nom() {
    // Chemin reel : mpv passe a la piste suivante seul, le coeur relaie
    // PlayerTrack(n), et seule la Source sait ce que « piste n » designe.
    let mut s = source_de_test(liste_de(5));
    let out = s.player_track(2).await;
    assert_eq!(out.preset, Some(3));
    assert!(out.preset_name.is_some());
    assert!(matches!(out.identity, Some(IdentityUpdate::Playing(_))));
}

#[tokio::test]
async fn un_index_negatif_est_ecarte() {
    // mpv dit -1 en fin de liste. Le coeur le transmet tel quel, la Source
    // l'ecarte : sans cela, l'index deviendrait absurde.
    let mut s = source_de_test(liste_de(3));
    let out = s.player_track(-1).await;
    assert!(matches!(out.action, SourceAction::Noop));
    assert!(out.identity.is_none());
}

#[tokio::test]
async fn la_fin_de_liste_declare_que_plus_rien_ne_joue() {
    // Le coeur envoie Stop quand mpv devient inactif. Sans recalage, la
    // derniere piste resterait affichee avec ses metadonnees indefiniment.
    let mut s = source_de_test(liste_de(3));
    let out = s.stop().await;
    assert_eq!(out.identity, Some(IdentityUpdate::Nothing));
}
```

- [ ] **Step 2 : lancer les tests, vérifier l'échec**

Run: `cargo test -p ritornello-plugin-files`
Expected: FAIL — `Playlist` et `FilesSource` absents.

- [ ] **Step 3 : implémenter le modèle**

```rust
use crate::m3u::{render, Entry};
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct Playlist {
    pub entries: Vec<Entry>,
    pub index: usize,
}

impl Playlist {
    /// Combien de pistes portent un chiffre de télécommande. `preset` étant un
    /// `u8` de plage 1–99, au-delà les pistes restent atteignables par
    /// next/prev et par la liste web, mais aucun chiffre ne les désigne.
    pub fn preset_count(&self) -> u8 {
        self.entries.len().min(99) as u8
    }

    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.index)
    }

    /// Positionne sur la présélection `n` (1-based). Rend `false` — sans
    /// déplacer la lecture — quand elle n'existe pas.
    pub fn select(&mut self, n: u8) -> bool {
        if n == 0 || usize::from(n) > self.entries.len() {
            return false;
        }
        self.index = usize::from(n) - 1;
        true
    }

    /// Écrit la liste destinée à mpv : chemins **absolus**, pour qu'elle ne
    /// dépende d'aucun répertoire courant. Écriture atomique.
    pub fn write_for_mpv(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("m3u.tmp");
        std::fs::write(&tmp, render(&self.entries, None))?;
        std::fs::rename(tmp, path)
    }
}
```

- [ ] **Step 4 : implémenter la moitié Source**

Dans `main.rs`, suivre le squelette de
`crates/ritornello-plugin-radio/src/main.rs` (deux `tokio::spawn`, `log_half`,
mode dégradé quand `--admin-socket` est absent). Le cœur du travail :

```rust
impl FilesSource {
    /// Identité de ce qui joue : le fichier, désigné par son chemin absolu.
    /// Opaque pour le cœur, qui ne fait que la comparer et la relayer.
    fn identite(path: &Path) -> serde_json::Value {
        serde_json::json!({ "kind": "file", "path": path.to_string_lossy() })
    }

    /// Statut permanent de la source. **Redéclaré à chaque trame** : `status`
    /// a la convention inverse de `preset`, absent voulant dire « pas de
    /// statut » et non « garde le précédent ».
    fn statut(&self) -> String {
        self.catalog.read().unwrap().get("status_files").to_string()
    }

    /// Lance la liste à l'index courant, après avoir réécrit le m3u de mpv.
    fn jouer(&mut self) -> SourceOutcome {
        let count = self.playlist.preset_count();
        let Some(entry) = self.playlist.current().cloned() else {
            let vide = self.catalog.read().unwrap().get("no_playlist").to_string();
            return SourceOutcome::new(SourceAction::Noop)
                .status(vide)
                .preset_count(0)
                .plays_nothing();
        };
        if let Err(e) = self.playlist.write_for_mpv(&self.mpv_playlist_path) {
            tracing::warn!("writing the mpv playlist: {e}");
        }
        SourceOutcome::new(
            SourceAction::play(self.mpv_playlist_path.to_string_lossy().to_string())
                .starting_at(self.playlist.index as i64)
                .finite(),
        )
        .plays(Self::identite(&entry.path))
        .preset((self.playlist.index + 1).min(99) as u8)
        .preset_name(entry.display_name())
        .preset_count(count)
        .status(self.statut())
    }
}

#[async_trait::async_trait]
impl SourcePlugin for FilesSource {
    async fn activate(&mut self) -> SourceOutcome {
        self.jouer()
    }

    async fn deactivate(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Stop).plays_nothing().status(self.statut())
    }

    async fn select(&mut self, n: u8) -> SourceOutcome {
        if self.playlist.select(n) {
            self.persist();
            self.jouer()
        } else {
            // Rien n'a été lancé : la piste précédente joue toujours. Message
            // éphémère, et surtout AUCUNE déclaration d'identité — un
            // `plays_nothing()` ici ferait cesser les plugins `metadata` et
            // viderait le titre affiché alors que le son continue.
            let vide = self.catalog.read().unwrap().get("empty_track").to_string();
            SourceOutcome::new(SourceAction::Noop)
                .status(vide)
                .transient()
                .preset_count(self.playlist.preset_count())
        }
    }

    async fn next(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::PlayerNext).status(self.statut())
    }

    async fn prev(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::PlayerPrev).status(self.statut())
    }

    async fn eject(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Noop).status(self.statut())
    }

    async fn player_track(&mut self, n: i64) -> SourceOutcome {
        // mpv dit -1 en fin de liste : le cœur le transmet tel quel, la Source
        // l'écarte.
        let Ok(i) = usize::try_from(n) else {
            return SourceOutcome::new(SourceAction::Noop);
        };
        if i >= self.playlist.entries.len() {
            return SourceOutcome::new(SourceAction::Noop);
        }
        self.playlist.index = i;
        self.persist();
        let entry = self.playlist.entries[i].clone();
        SourceOutcome::new(SourceAction::Noop)
            .plays(Self::identite(&entry.path))
            .preset((i + 1).min(99) as u8)
            .preset_name(entry.display_name())
            .preset_count(self.playlist.preset_count())
            .status(self.statut())
    }

    async fn stop(&mut self) -> SourceOutcome {
        SourceOutcome::new(SourceAction::Noop).plays_nothing().status(self.statut())
    }

    async fn set_locale(&mut self, locale: String) {
        *self.catalog.write().unwrap() =
            Catalog::load("files", &locale, &self.locales_root, FILES_EN);
    }
}
```

Compléter le catalogue anglais :

```toml
status_files = "FILES"
no_playlist = "NO PLAYLIST"
empty_track = "NO SUCH TRACK"
```

- [ ] **Step 5 : lancer les tests, vérifier le succès**

Run: `cargo test -p ritornello-plugin-files`
Expected: PASS.

- [ ] **Step 6 : commit**

```bash
git add -A
git commit -m "feat(plugin-files): la moitie Source, mpv tenant la liste

Le plugin ecrit un m3u a chemins absolus et laisse mpv enchainer : l avance
automatique passe alors par playlist-pos, exactement comme pour un disque.

Deux pieges traites explicitement : une piste inexistante ne declare aucune
identite (le morceau en cours continue, effacer ses metadonnees serait faux),
et le statut est redeclare a chaque trame -- sa convention est l inverse de
celle de preset, absent voulant dire « pas de statut »."
```

---

### Task 9 : persistance et reprise après redémarrage

**Files:**
- Create: `crates/ritornello-plugin-files/src/state.rs`
- Modify: `crates/ritornello-plugin-files/src/main.rs` (chargement au démarrage)
- Test: `state.rs`

**Interfaces:**
- Produces: `State { playlist: Vec<StoredEntry>, index: usize }`,
  `state::load(&Path) -> State`, `state::save(&Path, &State) -> anyhow::Result<()>`,
  `state::update(&Path, impl FnOnce(&mut State)) -> anyhow::Result<()>`.

- [ ] **Step 1 : écrire les tests qui échouent**

```rust
#[test]
fn un_etat_absent_ou_illisible_donne_un_etat_vide_sans_paniquer() {
    // Premiere installation, ou /var/lib efface : le plugin doit demarrer, pas
    // refuser de se lancer.
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(load(&dir.path().join("absent.json")).index, 0);
    let abime = dir.path().join("abime.json");
    std::fs::write(&abime, b"{ ceci n'est pas du json").unwrap();
    assert!(load(&abime).playlist.is_empty());
}

#[test]
fn la_liste_et_l_index_survivent_a_un_aller_retour() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("plugin-files.json");
    let etat = State {
        playlist: vec![StoredEntry {
            path: "/mnt/ritornello/nas/Album/01.mp3".into(),
            title: Some("So What".into()),
            duration_s: Some(245),
        }],
        index: 0,
    };
    save(&f, &etat).unwrap();
    let relu = load(&f);
    assert_eq!(relu.index, 0);
    assert_eq!(relu.playlist.len(), 1);
    assert_eq!(relu.playlist[0].title.as_deref(), Some("So What"));
}

#[test]
fn update_ne_perd_pas_les_champs_qu_il_ne_touche_pas() {
    // La moitie Admin ecrit la liste dans ce meme fichier ; un `save`
    // reconstruit par la moitie Source l'effacerait.
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("plugin-files.json");
    save(&f, &State { playlist: vec![entree_de_test()], index: 0 }).unwrap();
    update(&f, |s| s.index = 1).unwrap();
    let relu = load(&f);
    assert_eq!(relu.index, 1);
    assert_eq!(relu.playlist.len(), 1, "la liste a ete effacee par l'update");
}
```

- [ ] **Step 2 : lancer les tests, vérifier l'échec**

Run: `cargo test -p ritornello-plugin-files state`
Expected: FAIL — module absent.

- [ ] **Step 3 : implémenter**

```rust
//! État persisté du plugin : la liste courante et la piste en cours.
//!
//! Même motif que `crates/ritornello-plugin-radio/src/state.rs`, y compris
//! l'`update` qui préserve les champs qu'il ne touche pas — la moitié Admin
//! écrit dans ce même fichier, et un `save` reconstruit par la moitié Source
//! l'effacerait.

use crate::m3u::Entry;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub playlist: Vec<StoredEntry>,
    #[serde(default)]
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredEntry {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<u32>,
}

impl From<&StoredEntry> for Entry {
    fn from(s: &StoredEntry) -> Self {
        Entry { path: s.path.clone(), title: s.title.clone(), duration_s: s.duration_s }
    }
}

impl From<&Entry> for StoredEntry {
    fn from(e: &Entry) -> Self {
        StoredEntry { path: e.path.clone(), title: e.title.clone(), duration_s: e.duration_s }
    }
}

/// Un fichier absent ou illisible rend l'état vide, sans paniquer : une
/// première installation, ou un `/var/lib` effacé, doit laisser le plugin
/// démarrer et non refuser de se lancer.
pub fn load(path: &Path) -> State {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Écriture atomique : `.tmp` puis `rename`, pour qu'une coupure ne laisse
/// jamais un fichier tronqué que le démarrage suivant jetterait.
pub fn save(path: &Path, etat: &State) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(etat)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Relit, modifie, réécrit — et préserve donc ce que l'appelant ne touche pas.
pub fn update(path: &Path, f: impl FnOnce(&mut State)) -> anyhow::Result<()> {
    let mut etat = load(path);
    f(&mut etat);
    save(path, &etat)
}
```

- [ ] **Step 4 : brancher la reprise dans `main`**

Au démarrage, charger l'état, reconstruire la `Playlist`, et **écarter les
pistes dont le fichier a disparu** (partage non monté au boot) en les
journalisant — sans quoi mpv échouerait piste après piste.

```rust
let etat = state::load(&state_path);
let entries: Vec<Entry> = etat.playlist.iter().map(Entry::from).collect();
let manquantes = entries.iter().filter(|e| !e.path.is_file()).count();
if manquantes > 0 {
    tracing::warn!(
        "{manquantes} of {} tracks are missing at startup: the share may not be mounted yet",
        entries.len()
    );
}
```

Ne **pas** les supprimer de l'état : un partage momentanément absent
effacerait sinon la liste de l'utilisateur.

- [ ] **Step 5 : lancer les tests, vérifier le succès**

Run: `cargo test -p ritornello-plugin-files`
Expected: PASS.

- [ ] **Step 6 : commit**

```bash
git add -A
git commit -m "feat(plugin-files): la liste et la piste survivent au redemarrage

Meme motif que la preselection de la radio, y compris l update qui preserve
les champs qu il ne touche pas -- la moitie Admin ecrit dans le meme fichier.

Les pistes absentes au demarrage sont journalisees mais conservees : un
partage momentanement injoignable effacerait sinon la liste de l utilisateur."
```

---

# Phase 3 — Le montage réseau

### Task 10 : le binaire racine de montage

**Files:**
- Create: `crates/ritornello-plugin-files/src/bin/media-mount.rs`
- Create: `crates/ritornello-plugin-files/src/mount_options.rs`
- Test: `mount_options.rs`

**Interfaces:**
- Consumes: `roots::{Root, RootKind, Roots, MOUNT_ROOT}` (Task 5).
- Produces: `mount_command(root: &Root, creds_dir: &Path, uid: u32, gid: u32) -> Vec<String>`.

- [ ] **Step 1 : écrire les tests qui échouent**

```rust
#[test]
fn la_ligne_de_montage_impose_le_point_et_les_options() {
    // Le point de montage ne vient JAMAIS de la configuration, et la liste
    // d'options est fermee : aucun passe-plat vers mount -o.
    let r = Root {
        name: "nas".into(),
        kind: RootKind::Smb,
        host: "192.168.1.20".into(),
        share: "musique".into(),
        subpath: Some("Albums".into()),
        user: "steven".into(),
        domain: String::new(),
        writable: false,
        path: None,
    };
    let cmd = mount_command(&r, std::path::Path::new("/etc/ritornello/media-credentials"), 998, 998);
    assert_eq!(cmd[0], "mount");
    assert_eq!(cmd[1], "-t");
    assert_eq!(cmd[2], "cifs");
    assert_eq!(cmd[3], "//192.168.1.20/musique");
    // Le sous-chemin n'entre PAS dans le point de montage : il est parcouru
    // par le plugin sous le point monte.
    assert_eq!(cmd[4], "/mnt/ritornello/nas");
    assert_eq!(cmd[5], "-o");
    let options: Vec<&str> = cmd[6].split(',').collect();
    assert!(options.contains(&"ro"));
    assert!(options.contains(&"soft"));
    assert!(options.contains(&"iocharset=utf8"));
    assert!(options.contains(&"uid=998"));
    assert!(options.contains(&"gid=998"));
    assert!(options
        .contains(&"credentials=/etc/ritornello/media-credentials/nas.cred"));
    // Aucune version figee : la negociation du noyau vaut mieux.
    assert!(!options.iter().any(|o| o.starts_with("vers=")), "{options:?}");
}

#[test]
fn une_racine_inscriptible_perd_le_ro_et_rien_d_autre() {
    let r = Root { writable: true, ..racine_smb() };
    let cmd = mount_command(&r, std::path::Path::new("/c"), 1, 1);
    let options: Vec<&str> = cmd[6].split(',').collect();
    assert!(!options.contains(&"ro"), "{options:?}");
    assert!(options.contains(&"soft"), "soft doit rester : un NAS endormi ne doit pas bloquer");
}
```

La fabrique `racine_smb()` doit être **redéfinie localement** dans ce module de
tests : les utilitaires d'un module `#[cfg(test)]` ne sont pas visibles depuis
un autre module, celui de `roots.rs` ne sert donc pas ici.

- [ ] **Step 2 : lancer les tests, vérifier l'échec**

Run: `cargo test -p ritornello-plugin-files mount_options`
Expected: FAIL — fonction absente.

- [ ] **Step 3 : implémenter la construction de la ligne**

```rust
//! Construction de la ligne de montage, isolée pour être testable **sans
//! privilège** : c'est le code qui décide de ce que root exécutera.

use crate::roots::{Root, MOUNT_ROOT};
use std::path::Path;

/// Options de montage, **liste fermée**. Aucun passe-plat vers `mount -o` :
/// une option venue de la configuration serait une option choisie par
/// quiconque atteint l'IHM web.
///
/// `soft` parce qu'un NAS endormi doit rendre une erreur d'entrée-sortie
/// plutôt que bloquer indéfiniment un processus. Le risque de corruption qui
/// déconseille `soft` en écriture ne s'applique pas à un montage `ro` ; il
/// est assumé sur une racine déclarée inscriptible, qui ne sert qu'à déposer
/// un m3u.
///
/// Aucun `vers=` : la négociation du noyau vaut mieux qu'une version figée
/// qui vieillirait mal.
pub fn mount_command(root: &Root, creds_dir: &Path, uid: u32, gid: u32) -> Vec<String> {
    let mut options = vec![
        "soft".to_string(),
        "iocharset=utf8".to_string(),
        format!("uid={uid}"),
        format!("gid={gid}"),
        format!("credentials={}", root.credentials_path(creds_dir).display()),
    ];
    if !root.writable {
        options.insert(0, "ro".to_string());
    }
    if !root.domain.is_empty() {
        options.push(format!("domain={}", root.domain));
    }
    vec![
        "mount".to_string(),
        "-t".to_string(),
        "cifs".to_string(),
        format!("//{}/{}", root.host, root.share),
        format!("{MOUNT_ROOT}/{}", root.name),
        "-o".to_string(),
        options.join(","),
    ]
}
```

- [ ] **Step 4 : écrire le binaire**

`src/bin/media-mount.rs` — il **revalide** tout, la validation faite côté
plugin ne comptant pas comme une garantie :

```rust
//! Binaire **racine** : réconcilie les montages déclarés.
//!
//! Lancé par `ritornello-media-mount.service`, lui-même démarré par le plugin
//! via `systemctl` et autorisé par polkit. Il consomme une configuration
//! écrite par un processus **non privilégié** : il revalide donc tout, la
//! validation faite côté plugin ne comptant pas comme une garantie.

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let roots_path = std::env::var("RITORNELLO_FILES_ROOTS")
        .unwrap_or_else(|_| "/etc/ritornello/media-roots.toml".into());
    let creds_dir = std::env::var("RITORNELLO_FILES_CREDENTIALS")
        .unwrap_or_else(|_| "/etc/ritornello/media-credentials".into());

    let roots = Roots::load(std::path::Path::new(&roots_path))?;
    roots.validate()?; // ceinture et bretelles : `load` valide déjà

    // Démonter ce qui n'est plus déclaré, monter ce qui manque. Idempotent,
    // donc rejouable sans précaution, y compris au démarrage de la machine.
    demonter_les_disparus(&roots)?;
    for r in roots.root.iter().filter(|r| r.kind == RootKind::Smb) {
        let point = std::path::PathBuf::from(MOUNT_ROOT).join(&r.name);
        if est_monte(&point) {
            continue;
        }
        std::fs::create_dir_all(&point)?;
        let cmd = mount_command(r, std::path::Path::new(&creds_dir), uid(), gid());
        let sortie = std::process::Command::new(&cmd[0]).args(&cmd[1..]).output()?;
        if !sortie.status.success() {
            tracing::error!(
                "mounting {}: {}",
                r.name,
                String::from_utf8_lossy(&sortie.stderr).trim()
            );
        }
    }
    Ok(())
}
```

`est_monte` lit `/proc/mounts` et cherche le point de montage en deuxième
colonne. `uid()`/`gid()` lisent l'utilisateur `ritornello` dans `/etc/passwd`
(pas de dépendance `nix` : une lecture de fichier suffit et se teste).

- [ ] **Step 5 : lancer les tests, vérifier le succès**

Run: `cargo test -p ritornello-plugin-files`
Expected: PASS.

- [ ] **Step 6 : commit**

```bash
git add -A
git commit -m "feat(plugin-files): le binaire racine de montage, et sa ligne testee

La construction de la ligne de montage vit dans un module a part, testable
sans privilege : c est le code qui decide de ce que root executera.

Le binaire revalide tout ce qu il lit. La configuration est ecrite par un
processus non privilegie -- la validation faite de ce cote la ne compte pas
comme une garantie."
```

---

### Task 11 : parler à systemd depuis le plugin

**Files:**
- Create: `crates/ritornello-plugin-files/src/mount.rs`
- Create: `deploy/ritornello-media-mount.service`
- Create: `deploy/51-ritornello-media.rules`
- Modify: `deploy/50-ritornello-power.rules` (le commentaire devenu faux)
- Test: `mount.rs`

**Interfaces:**
- Produces: `MountState { Mounted, NotMounted }`,
  `mount::state(root: &Root) -> MountState`,
  `mount::reconcile(unit: &str) -> Result<(), String>` (l'erreur porte la
  sortie de `systemctl`, **verbatim**).

- [ ] **Step 1 : écrire les tests**

```rust
#[test]
fn un_point_de_montage_absent_de_proc_mounts_est_non_monte() {
    // /proc/mounts est parse par une fonction pure : le test n'a pas besoin de
    // monter quoi que ce soit.
    let contenu = "//192.168.1.20/musique /mnt/ritornello/nas cifs ro,relatime 0 0\n\
                   /dev/sda1 /media/usb ext4 rw 0 0\n";
    assert!(est_monte_dans(contenu, std::path::Path::new("/mnt/ritornello/nas")));
    assert!(!est_monte_dans(contenu, std::path::Path::new("/mnt/ritornello/autre")));
}

#[test]
fn un_point_de_montage_avec_espace_echappe_est_reconnu() {
    // /proc/mounts echappe l'espace en \040. Sans ce traitement, un partage
    // « Ma Musique » passerait pour non monte, et le plugin le remonterait
    // en boucle.
    let contenu = "//nas/x /mnt/ritornello/ma\\040musique cifs ro 0 0\n";
    assert!(est_monte_dans(contenu, std::path::Path::new("/mnt/ritornello/ma musique")));
}
```

- [ ] **Step 2 : lancer les tests, vérifier l'échec**

Run: `cargo test -p ritornello-plugin-files mount`
Expected: FAIL — module absent.

- [ ] **Step 3 : implémenter**

```rust
/// Vrai si `point` figure comme point de montage dans le contenu de
/// `/proc/mounts`.
///
/// La deuxième colonne échappe l'espace en `\040` (et la tabulation en
/// `\011`) : sans le traitement, un partage « Ma Musique » passerait pour non
/// monté et le plugin le remonterait en boucle.
pub fn est_monte_dans(proc_mounts: &str, point: &Path) -> bool {
    proc_mounts.lines().any(|l| {
        l.split_whitespace()
            .nth(1)
            .map(|p| p.replace("\\040", " ").replace("\\011", "\t"))
            .map(|p| Path::new(&p) == point)
            .unwrap_or(false)
    })
}

/// Demande à systemd de réconcilier les montages.
///
/// `systemctl` en processus fils, et non une crate D-Bus : c'est ainsi que
/// l'onglet Système parle à systemd et à logind, et cela évite une dépendance
/// entière pour un appel.
///
/// En cas d'échec, l'erreur porte la sortie de `systemctl` **verbatim** : un
/// refus polkit y est explicite et actionnable (« installer
/// 51-ritornello-media.rules »), là où un message maison la rendrait opaque.
/// systemd n'offrant pas d'équivalent au `CanPowerOff` de logind, il n'y a pas
/// de sonde de capacité possible — on tente, et on rapporte.
pub async fn reconcile(unit: &str) -> Result<(), String> {
    let sortie = tokio::process::Command::new("systemctl")
        .arg("start")
        .arg(unit)
        .output()
        .await
        .map_err(|e| format!("systemctl unavailable: {e}"))?;
    if sortie.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&sortie.stderr).trim().to_string();
    Err(if err.is_empty() { format!("systemctl failed ({})", sortie.status) } else { err })
}
```

- [ ] **Step 4 : écrire l'unité et la règle polkit**

`deploy/ritornello-media-mount.service` :

```ini
[Unit]
Description=Ritornello: mount the declared media shares
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
RemainAfterExit=no
ExecStart=/usr/local/lib/ritornello/ritornello-media-mount

[Install]
WantedBy=multi-user.target
```

`deploy/51-ritornello-media.rules` :

```javascript
// The Files plugin lets the shares be declared from its web page, and asks
// systemd to (re)mount them. The service runs as the unprivileged
// `ritornello` user with NoNewPrivileges=true, so sudo and any setuid path
// are structurally unavailable: starting a unit through polkit is the
// mechanism, and this file is the authorisation.
//
// Scoped to ONE unit on purpose. `manage-units` without the unit check would
// grant the web UI the right to start, stop and restart every unit on the
// machine. The unit it does grant runs a binary that mounts what
// /etc/ritornello/media-roots.toml declares -- a file the web UI writes --
// so that binary revalidates everything it reads: forced mount point, closed
// option list, no comma allowed in a host or share name.
//
// This is deliberately NOT in 50-ritornello-power.rules, whose comment states
// that manage-units is not granted. That statement remains true of the power
// actions; this file is the separate, narrower grant.
polkit.addRule(function (action, subject) {
  if (
    subject.user === "ritornello" &&
    action.id === "org.freedesktop.systemd1.manage-units" &&
    action.lookup("unit") === "ritornello-media-mount.service"
  ) {
    return polkit.Result.YES;
  }
});
```

Dans `deploy/50-ritornello-power.rules`, corriger le commentaire devenu faux :

```
// Nothing else is granted HERE: not `manage-units`, because restarting
// Ritornello itself needs no privilege (the process exits and systemd
// restarts it, the unit saying Restart=always). The Files plugin does need a
// narrow `manage-units` grant for a single mount unit -- see
// 51-ritornello-media.rules.
```

- [ ] **Step 5 : lancer les tests, vérifier le succès**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6 : commit**

```bash
git add -A
git commit -m "feat(plugin-files): montage pilote par systemd, autorisation polkit restreinte

systemctl en processus fils, comme l onglet Systeme, plutot qu une crate
D-Bus entiere pour un appel. Pas de sonde de capacite : systemd n offre pas
d equivalent au CanPowerOff de logind, donc on tente et on rapporte la sortie
de systemctl verbatim -- un refus polkit y est explicite et actionnable.

La regle vit dans un fichier separe et nomme l unite : manage-units sans
verification d unite donnerait a l IHM web le droit de piloter toutes les
unites de la machine. Le commentaire de 50-ritornello-power.rules, qui
affirmait que manage-units n etait jamais accorde, est corrige."
```

---

# Phase 4 — Listes, page et livraison

### Task 12 : les listes enregistrées

**Files:**
- Create: `crates/ritornello-plugin-files/src/store.rs`
- Test: même fichier

**Interfaces:**
- Consumes: `m3u::{Entry, parse, render}` (Task 6), `roots::{Root, Roots}` (Task 5).
- Produces: `Saved { name: String, where_: Location }`,
  `Location { Internal, Root(String) }`,
  `store::list(internal_dir: &Path, roots: &Roots) -> Vec<Saved>`,
  `store::save(entries: &[Entry], name: &str, dest: &Location, internal_dir: &Path, roots: &Roots) -> Result<(), StoreError>`,
  `store::load(name: &str, from: &Location, internal_dir: &Path, roots: &Roots) -> Result<m3u::Parsed, StoreError>`,
  `StoreError { BadPlaylistName { name }, ReadOnlyRoot { root }, UnknownRoot { name }, Io { path } }`,
  `StoreError::message(&self, &Catalog) -> String`.

- [ ] **Step 1 : écrire les tests qui échouent**

```rust
#[test]
fn un_nom_de_liste_qui_traverse_est_refuse() {
    // Le nom devient un nom de fichier, ecrit soit dans /var/lib, soit sur le
    // partage : « ../../etc/cron.d/x » ne doit jamais atteindre le disque.
    let (dir, roots) = decor();
    for mauvais in ["../evasion", "a/b", "", ".", "..", "x\0y"] {
        assert!(
            matches!(
                store::save(&[], mauvais, &Location::Internal, dir.path(), &roots),
                Err(StoreError::BadPlaylistName { .. })
            ),
            "accepte a tort : {mauvais:?}"
        );
    }
}

#[test]
fn enregistrer_sur_une_racine_en_lecture_seule_est_refuse_avec_une_phrase() {
    // Le montage est `ro` par defaut : il faut le dire clairement plutot que
    // de laisser remonter une erreur d'entree-sortie du noyau.
    let (dir, roots) = decor(); // « nas » est writable = false
    let err = store::save(&[], "Jazz", &Location::Root("nas".into()), dir.path(), &roots)
        .unwrap_err();
    assert!(matches!(err, StoreError::ReadOnlyRoot { .. }));
}

#[test]
fn une_liste_enregistree_en_interne_se_recharge_a_l_identique() {
    let (dir, roots) = decor();
    let fichiers = trois_fichiers(&dir);
    let entries: Vec<Entry> = fichiers
        .iter()
        .map(|p| Entry { path: p.clone(), title: None, duration_s: None })
        .collect();
    store::save(&entries, "Jazz", &Location::Internal, dir.path(), &roots).unwrap();
    let relu = store::load("Jazz", &Location::Internal, dir.path(), &roots).unwrap();
    assert_eq!(relu.entries.len(), 3);
    assert!(relu.unresolved.is_empty());
    assert_eq!(relu.entries[0].path, fichiers[0]);
}

#[test]
fn une_liste_enregistree_sur_une_racine_porte_des_chemins_relatifs() {
    // C'est ce qui la rend relisible par un autre lecteur et survivante a un
    // changement de point de montage.
    let (dir, roots) = decor_inscriptible();
    let base = roots.by_name("nas").unwrap().base_dir();
    let entries = vec![Entry { path: base.join("Album/01.mp3"), title: None, duration_s: None }];
    store::save(&entries, "Jazz", &Location::Root("nas".into()), dir.path(), &roots).unwrap();
    let texte = std::fs::read_to_string(base.join("Jazz.m3u")).unwrap();
    assert!(texte.contains("Album/01.mp3"), "{texte}");
    assert!(!texte.contains(base.to_str().unwrap()), "chemin absolu ecrit : {texte}");
}

#[test]
fn lister_montre_l_interne_et_les_racines_ensemble() {
    let (dir, roots) = decor_inscriptible();
    store::save(&[], "Jazz", &Location::Internal, dir.path(), &roots).unwrap();
    store::save(&[], "Rock", &Location::Root("nas".into()), dir.path(), &roots).unwrap();
    let mut noms: Vec<String> =
        store::list(dir.path(), &roots).into_iter().map(|s| s.name).collect();
    noms.sort();
    assert_eq!(noms, vec!["Jazz", "Rock"]);
}

#[test]
fn chaque_refus_de_store_resout_contre_le_catalogue_embarque() {
    let catalog =
        Catalog::load("files", "en", std::path::Path::new("/inexistant"), crate::FILES_EN);
    let messages = [
        StoreError::BadPlaylistName { name: "../x".into() }.message(&catalog),
        StoreError::ReadOnlyRoot { root: "nas".into() }.message(&catalog),
        StoreError::UnknownRoot { name: "absent".into() }.message(&catalog),
        StoreError::Io { path: "/x".into() }.message(&catalog),
    ];
    for m in &messages {
        assert!(m.contains(' '), "message reduit a une cle brute : {m:?}");
    }
}
```

**Sur les fixtures** : `decor()` et `decor_inscriptible()` doivent bâtir leurs
racines **dans un `tempfile::tempdir()`**, donc en `RootKind::Local`. Une
racine `Smb` a pour `base_dir()` `/mnt/ritornello/<name>`, où la suite de tests
ne peut évidemment pas écrire. `decor()` déclare la racine `nas` avec
`writable = false`, `decor_inscriptible()` la même avec `writable = true` : le
drapeau est vérifié quel que soit le genre, ce qui rend la règle éprouvable
sans montage.

- [ ] **Step 2 : lancer les tests, vérifier l'échec**

Run: `cargo test -p ritornello-plugin-files store`
Expected: FAIL — module absent.

- [ ] **Step 3 : implémenter**

```rust
//! Listes enregistrées : dans le stockage interne, ou sur une racine.
//!
//! Le format est le m3u, au même titre que ce qu'on charge : une liste
//! déposée sur le NAS doit y être relisible par n'importe quel autre lecteur.

use crate::m3u::{self, Entry};
use crate::roots::Roots;
use ritornello_i18n::Catalog;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum Location {
    Internal,
    Root(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub name: String,
    pub location: Location,
}

#[derive(Debug)]
pub enum StoreError {
    BadPlaylistName { name: String },
    ReadOnlyRoot { root: String },
    UnknownRoot { name: String },
    Io { path: String },
}

/// Un nom de liste devient un nom de fichier, écrit soit dans `/var/lib`, soit
/// **sur le partage**. Tout ce qui pourrait traverser est refusé : pas de
/// séparateur, pas de nom réservé, pas d'octet nul.
fn nom_de_liste_valide(nom: &str) -> bool {
    !nom.is_empty()
        && nom.len() <= 64
        && nom != "."
        && nom != ".."
        && !nom.contains('/')
        && !nom.contains('\\')
        && !nom.contains('\0')
        && !nom.starts_with('.')
}

fn dossier(dest: &Location, internal_dir: &Path, roots: &Roots) -> Result<PathBuf, StoreError> {
    match dest {
        Location::Internal => Ok(internal_dir.to_path_buf()),
        Location::Root(nom) => {
            let r = roots
                .by_name(nom)
                .ok_or_else(|| StoreError::UnknownRoot { name: nom.clone() })?;
            // Le montage est `ro` par défaut : le dire clairement vaut mieux
            // que de laisser remonter une erreur d'entrée-sortie du noyau,
            // que personne ne saurait attribuer.
            if !r.writable {
                return Err(StoreError::ReadOnlyRoot { root: nom.clone() });
            }
            Ok(r.base_dir())
        }
    }
}

pub fn save(
    entries: &[Entry],
    name: &str,
    dest: &Location,
    internal_dir: &Path,
    roots: &Roots,
) -> Result<(), StoreError> {
    if !nom_de_liste_valide(name) {
        return Err(StoreError::BadPlaylistName { name: name.to_string() });
    }
    let dir = dossier(dest, internal_dir, roots)?;
    // Chemins relatifs quand la destination est une racine : c'est ce qui rend
    // la liste relisible ailleurs et survivante à un changement de point de
    // montage. En interne, la base n'a pas de sens : chemins absolus.
    let base = matches!(dest, Location::Root(_)).then(|| dir.clone());
    let texte = m3u::render(entries, base.as_deref());
    let fichier = dir.join(format!("{name}.m3u"));
    let tmp = fichier.with_extension("m3u.tmp");
    std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(&tmp, texte))
        .and_then(|_| std::fs::rename(&tmp, &fichier))
        .map_err(|_| StoreError::Io { path: fichier.display().to_string() })
}

pub fn load(
    name: &str,
    from: &Location,
    internal_dir: &Path,
    roots: &Roots,
) -> Result<m3u::Parsed, StoreError> {
    if !nom_de_liste_valide(name) {
        return Err(StoreError::BadPlaylistName { name: name.to_string() });
    }
    // Charger ne demande aucune écriture : une racine en lecture seule est
    // parfaitement légitime ici, on ne passe donc pas par `dossier`.
    let dir = match from {
        Location::Internal => internal_dir.to_path_buf(),
        Location::Root(nom) => roots
            .by_name(nom)
            .ok_or_else(|| StoreError::UnknownRoot { name: nom.clone() })?
            .base_dir(),
    };
    let fichier = dir.join(format!("{name}.m3u"));
    let texte = std::fs::read_to_string(&fichier)
        .map_err(|_| StoreError::Io { path: fichier.display().to_string() })?;
    Ok(m3u::parse(&texte, &dir, &dir))
}

/// Toutes les listes visibles, l'interne et les racines confondues. Une racine
/// injoignable est **ignorée sans erreur** : le NAS endormi ne doit pas
/// empêcher de voir ses listes internes.
pub fn list(internal_dir: &Path, roots: &Roots) -> Vec<Saved> {
    let mut out = Vec::new();
    let mut ramasse = |dir: &Path, loc: Location| {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("m3u"))
            {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    out.push(Saved { name: stem.to_string(), location: loc.clone() });
                }
            }
        }
    };
    ramasse(internal_dir, Location::Internal);
    for r in &roots.root {
        ramasse(&r.base_dir(), Location::Root(r.name.clone()));
    }
    out
}

impl StoreError {
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            StoreError::BadPlaylistName { name } => {
                catalog.get("bad_playlist_name").replace("{name}", name)
            }
            StoreError::ReadOnlyRoot { root } => {
                catalog.get("read_only_root").replace("{name}", root)
            }
            StoreError::UnknownRoot { name } => catalog.get("unknown_root").replace("{name}", name),
            StoreError::Io { path } => catalog.get("store_io_error").replace("{path}", path),
        }
    }
}
```

Compléter le catalogue anglais :

```toml
bad_playlist_name = "invalid playlist name \"{name}\": no slashes, and it must not start with a dot"
read_only_root = "the root \"{name}\" is mounted read-only: tick \"allow writing\" on it to save playlists there"
unknown_root = "no root named \"{name}\""
store_io_error = "could not write or read \"{path}\""
```

- [ ] **Step 4 : lancer les tests, vérifier le succès**

Run: `cargo test -p ritornello-plugin-files store`
Expected: PASS.

- [ ] **Step 5 : commit**

```bash
git add -A
git commit -m "feat(plugin-files): enregistrer et recharger des listes, en interne ou sur le partage

Le format est le m3u des deux cotes : une liste deposee sur le NAS doit y etre
relisible par n importe quel autre lecteur, donc a chemins relatifs.

Le nom de liste est valide parce qu il devient un nom de fichier ecrit sur le
partage. Et enregistrer sur une racine montee en lecture seule est refuse par
une phrase, plutot que de laisser remonter une erreur d entree-sortie du noyau
que personne ne saurait attribuer."
```

---

### Task 13 : la moitié Admin

**Files:**
- Create: `crates/ritornello-plugin-files/src/admin.rs`
- Modify: `crates/ritornello-plugin-files/src/main.rs` (branchement)
- Test: `admin.rs`

**Interfaces:**
- Consumes: tout ce qui précède.
- Produces: `FilesAdmin` implémentant `AdminPlugin` ;
  `get_data` rend `{ roots, playlist, scan, saved, mounts }` ;
  `set_data` accepte `{ op: "...", ... }`.

- [ ] **Step 1 : définir le contrat de données**

`get_data` :

```json
{
  "roots": [{"name":"nas","kind":"smb","host":"…","share":"…","subpath":"Albums",
             "user":"steven","domain":"","writable":false,"mounted":true}],
  "playlist": [{"path":"…","name":"Piste 1","duration_s":245,"missing":false}],
  "index": 3,
  "scan": {"running": true, "found": 412, "dir": "Albums/Jazz"},
  "saved": [{"name":"Jazz","where":"internal"},{"name":"Rock","where":"nas"}],
  "unresolved": []
}
```

`set_data`, discriminé par `op` : `save_roots`, `mount`, `browse`,
`add_dir`, `add_file`, `remove`, `move`, `clear`, `save_playlist`,
`load_playlist`, `search`.

Le mot de passe **n'est jamais rendu** par `get_data` : il ne voyage que dans
`save_roots`, en entrée. Un champ vide en entrée veut dire « garde celui déjà
enregistré » — sans quoi rouvrir la page et enregistrer effacerait le mot de
passe.

- [ ] **Step 2 : écrire les tests qui échouent**

```rust
#[tokio::test]
async fn get_data_ne_rend_jamais_le_mot_de_passe() {
    // Il n'a aucune raison de traverser le reseau vers le navigateur, et la
    // page n'en a pas besoin pour afficher l'etat d'un partage.
    let admin = admin_de_test();
    let data = admin.get_data().await;
    let texte = serde_json::to_string(&data).unwrap();
    assert!(!texte.contains("password"), "{texte}");
    assert!(!texte.contains("secret-du-nas"), "{texte}");
}

#[tokio::test]
async fn un_mot_de_passe_vide_conserve_celui_deja_enregistre() {
    // Sinon rouvrir la page et cliquer « Enregistrer » suffirait a casser le
    // montage, sans que rien ne l'annonce.
    let mut admin = admin_de_test();
    admin.set_data(serde_json::json!({
        "op": "save_roots",
        "roots": [{"name":"nas","kind":"smb","host":"h","share":"s","user":"u","password":""}]
    }))
    .await
    .unwrap();
    let cred = std::fs::read_to_string(admin.creds_dir.join("nas.cred")).unwrap();
    assert!(cred.contains("password=secret-du-nas"), "{cred}");
}

#[tokio::test]
async fn une_racine_invalide_est_refusee_avec_une_phrase_pas_une_cle() {
    let mut admin = admin_de_test();
    let err = admin
        .set_data(serde_json::json!({
            "op": "save_roots",
            "roots": [{"name":"nas","kind":"smb","host":"nas,uid=0","share":"s","user":"u"}]
        }))
        .await
        .unwrap_err();
    assert!(err.contains(' '), "cle brute renvoyee a l'ecran : {err}");
    assert!(err.contains("nas,uid=0"), "le refus doit nommer ce qui cloche : {err}");
}

#[tokio::test]
async fn le_fichier_d_identifiants_est_ecrit_en_0600() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut admin = admin_de_test();
        admin.set_data(serde_json::json!({
            "op": "save_roots",
            "roots": [{"name":"nas","kind":"smb","host":"h","share":"s","user":"u","password":"p"}]
        }))
        .await
        .unwrap();
        let meta = std::fs::metadata(admin.creds_dir.join("nas.cred")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}

#[tokio::test]
async fn un_scan_en_cours_est_remplace_par_le_suivant() {
    // Deux clics successifs ne doivent pas laisser deux marches concurrentes
    // saturer un partage lent.
    let mut admin = admin_de_test();
    admin.set_data(json_add_dir("/a")).await.ok();
    admin.set_data(json_add_dir("/b")).await.ok();
    let data = admin.get_data().await;
    assert_eq!(data["scan"]["dir"], "/b");
}
```

- [ ] **Step 3 : implémenter**

Suivre `crates/ritornello-plugin-radio/src/admin.rs` : même forme
`AdminPlugin`, mêmes assets servis (`ui.js`, `ui.css`), même `GetCatalog`.
La tâche de scan est un `tokio::task::JoinHandle` rangé dans l'état ; en
lancer un nouveau **avorte** le précédent (`handle.abort()`), la progression
vivant dans un `Arc<Mutex<ScanProgress>>` partagé.

L'écriture du fichier d'identifiants :

```rust
/// Écrit le fichier consommé par `mount.cifs`. Permissions posées **avant**
/// l'écriture du contenu : créer puis restreindre laisserait une fenêtre où le
/// mot de passe serait lisible par tous.
fn ecrire_identifiants(path: &Path, user: &str, password: &str, domain: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("cred.tmp");
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?
    };
    #[cfg(not(unix))]
    let mut f = std::fs::File::create(&tmp)?;
    use std::io::Write;
    writeln!(f, "username={user}")?;
    writeln!(f, "password={password}")?;
    if !domain.is_empty() {
        writeln!(f, "domain={domain}")?;
    }
    f.sync_all()?;
    drop(f);
    std::fs::rename(tmp, path)
}
```

- [ ] **Step 4 : lancer les tests, vérifier le succès**

Run: `cargo test -p ritornello-plugin-files`
Expected: PASS.

- [ ] **Step 5 : commit**

```bash
git add -A
git commit -m "feat(plugin-files): la moitie Admin, scan asynchrone et identifiants

Le mot de passe ne traverse jamais dans le sens serveur vers navigateur, et un
champ vide en entree conserve celui deja enregistre : sans cela, rouvrir la
page et cliquer Enregistrer suffirait a casser le montage sans rien annoncer.

Le fichier d identifiants recoit ses permissions a la creation, pas apres :
creer puis restreindre laisserait une fenetre ou le mot de passe serait
lisible par tous.

Un nouveau scan avorte le precedent : deux clics ne doivent pas laisser deux
marches concurrentes saturer un partage lent."
```

---

### Task 14 : la page Vue

**Files:**
- Create: `crates/ritornello-plugin-files/ui/` (package.json, vite.config.ts, src/App.vue, src/main.ts, src/volets/*.vue)
- Modify: `crates/ritornello-plugin-files/build.rs`
- Test: `crates/ritornello-plugin-files/ui/src/*.test.ts` (vitest)

**Interfaces:**
- Consumes: le contrat `get_data`/`set_data` de la tâche 13.
- Produces: `ui/dist/ui.js` et `ui/dist/ui.css`, **noms à plat**.

- [ ] **Step 1 : copier la charpente**

Partir de `crates/ritornello-plugin-radio/ui/` : `package.json`,
`vite.config.ts` (sortie à plat, `vue` et `@ritornello/ui` en `external`),
`build.rs` qui embarque `dist/ui.js` et `dist/ui.css`. Ajouter le workspace npm
(déjà couvert par le glob `crates/*/ui` de la racine).

- [ ] **Step 2 : écrire le point d'entrée du module**

`ui/src/main.ts` — c'est ce que le shell importe, et les deux exports sont
l'intégralité du contrat côté module :

```ts
import App from "./App.vue";

/** Version du contrat attendue par le shell (voir web/kit/src/contract.ts). */
export const contract = 1;

export default App;
export { App };
```

`ui/src/App.vue` déclare `base` **requis sans défaut** :

```ts
const props = defineProps<{ catalog: Record<string, string>; base: string }>();
```

En options API (nécessaire pour que le test puisse lire `App.props`) :

```js
props: {
  catalog: { type: Object, required: true },
  // Pas de valeur par défaut, volontairement : le nom sous lequel un plugin
  // est servi vient de plugins.toml, donc du déploiement. Un module qui
  // reconstruirait « /plugins/files/ » serait faux — silencieusement — dès
  // qu'un opérateur le déclare sous un autre nom.
  base: { type: String, required: true },
}
```

- [ ] **Step 3 : écrire le test de garde du contrat**

```ts
import { describe, expect, it } from "vitest";
import App, { contract } from "./main";

describe("contrat du module", () => {
  it("declare la version du contrat attendue par le shell", () => {
    expect(contract).toBe(1);
  });

  it("exige base sans valeur par defaut", () => {
    // Le nom sous lequel un plugin est servi vient de plugins.toml, donc du
    // deploiement : un module qui reconstruirait /plugins/files/ serait faux,
    // silencieusement, des qu'un operateur le declare sous un autre nom.
    expect(App.props.base.required).toBe(true);
    expect(App.props.base.default).toBeUndefined();
  });
});
```

- [ ] **Step 4 : écrire les trois volets**

`VoletRacines.vue`, `VoletParcourir.vue`, `VoletListe.vue`, montés par
`App.vue`. Toutes les URL construites depuis `base` :
`api.get(\`${base}api/data\`)`, jamais `./api/data` — un chemin relatif se
résout contre l'URL du navigateur et désignerait `/plugins/api/data`.

Points d'attention :
- l'arbre est **paresseux** : une requête `browse` par niveau ouvert ;
- pendant un scan, la page rappelle `get_data` toutes les secondes et affiche
  `scan.found` et `scan.dir` ; le protocole admin ne pousse rien ;
- la liste des pistes est **virtualisée** au-delà de 200 lignes ;
- les pistes `missing: true` sont marquées, non masquées ;
- les entrées `unresolved` d'un m3u chargé sont affichées dans un encart.

- [ ] **Step 5 : lancer les tests**

Run: `npm test --workspaces` puis `npm run build --workspaces`
Expected: PASS, et `crates/ritornello-plugin-files/ui/dist/` ne contient que
des fichiers **à plat**.

- [ ] **Step 6 : commit**

```bash
git add -A
git commit -m "feat(plugin-files): la page de gestion, trois volets

Racines, parcours paresseux avec recherche, liste editable. Toutes les URL
sont construites depuis la prop base : un chemin relatif se resoudrait contre
l URL du navigateur et designerait /plugins/api/data, que le coeur interprete
comme un plugin nomme « api » -- donc un 404.

La page sonde pendant un scan parce que le protocole admin ne pousse rien, et
les pistes introuvables sont marquees plutot que masquees."
```

---

### Task 15 : déploiement et documentation

**Files:**
- Modify: `deploy/deploy.sh`, `deploy/build.sh`, `deploy/plugins.example.toml`
- Create: `deploy/media-roots.example.toml`, `deploy/locales/files/fr.toml`
- Modify: `docs/plugins.md`, `docs/installation.md`, `README.md`

- [ ] **Step 1 : entrée dans `plugins.example.toml`**

```toml
# Lecture de fichiers audio depuis un partage reseau ou un support local.
# Sa page declare les partages ; le montage passe par
# ritornello-media-mount.service, autorise par 51-ritornello-media.rules.
[[plugin]]
name = "files"
kind = "source"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-files"
```

- [ ] **Step 2 : `deploy.sh`**

Le binaire du plugin suit le chemin des autres (copie dans
`/usr/local/lib/ritornello/plugins/`). S'y ajoute, avant le
`systemctl restart ritornello` final :

```bash
# Binaire racine de montage : hors du repertoire des plugins, il n'est pas
# lance par le coeur mais par systemd.
scp "${SSHOPTS[@]}" "$BUILD/ritornello-media-mount" "$PI:/tmp/ritornello-media-mount"
ssh "${SSHOPTS[@]}" "$PI" 'sudo install -m 0755 -o root -g root \
  /tmp/ritornello-media-mount /usr/local/lib/ritornello/ritornello-media-mount \
  && rm -f /tmp/ritornello-media-mount'

# Unite de montage et autorisation polkit.
scp "${SSHOPTS[@]}" deploy/ritornello-media-mount.service "$PI:/tmp/"
scp "${SSHOPTS[@]}" deploy/51-ritornello-media.rules "$PI:/tmp/"
ssh "${SSHOPTS[@]}" "$PI" 'sudo install -m 0644 -o root -g root \
    /tmp/ritornello-media-mount.service /etc/systemd/system/ \
  && sudo install -m 0644 -o root -g root \
    /tmp/51-ritornello-media.rules /etc/polkit-1/rules.d/ \
  && rm -f /tmp/ritornello-media-mount.service /tmp/51-ritornello-media.rules'

# Points de montage et identifiants. Le repertoire d'identifiants appartient au
# service (il y ecrit depuis la page) et n'est lisible que par lui ; le binaire
# racine, lui, lit tout.
ssh "${SSHOPTS[@]}" "$PI" 'sudo mkdir -p /mnt/ritornello /etc/ritornello/media-credentials \
  && sudo chown ritornello: /etc/ritornello/media-credentials \
  && sudo chmod 0700 /etc/ritornello/media-credentials'

# Remonte les partages au demarrage de la machine.
ssh "${SSHOPTS[@]}" "$PI" 'sudo systemctl daemon-reload \
  && sudo systemctl enable ritornello-media-mount.service'
```

Adapter `$BUILD` et `${SSHOPTS[@]}` aux noms réellement employés dans le script
(lire l'en-tête de `deploy/deploy.sh`). Ajouter `cifs-utils` à la liste de
paquets vérifiée par le script si elle existe.

`deploy/build.sh` : ajouter la construction du module Vue du plugin — le glob
`crates/*/ui` des workspaces npm le couvre déjà si le `package.json` du module
déclare un script `build`.

- [ ] **Step 3 : documentation**

`docs/plugins.md` : une section `ritornello-plugin-files` sur le modèle des
autres — ce que fait la page, les variables d'environnement, le plafond des 99
présélections, et **le fait qu'un `plugins.toml` existant n'est jamais
écrasé**, donc qu'une installation en service ne verra pas la source tant que
sa ligne n'aura pas été ajoutée à la main.

`docs/installation.md` : ajouter `cifs-utils` aux paquets, décrire la
déclaration d'un partage depuis la page, et le diagnostic d'un refus polkit.

`README.md` : la source `files` dans les points saillants et dans le diagramme
mermaid.

- [ ] **Step 4 : commit**

```bash
git add -A
git commit -m "docs: la source fichiers, son montage et son installation"
```

---

### Task 16 : parcours e2e

**Files:**
- Create: `web/app/e2e/files.spec.ts`

- [ ] **Step 1 : écrire le parcours**

Sur une instance locale, avec une racine **locale** pointant vers un répertoire
de fixtures contenant trois fichiers audio (générés par `ffmpeg` dans le
harnais, ou trois fichiers courts versionnés) :

1. ouvrir `/plugins/files/` ;
2. déclarer la racine locale, enregistrer, vérifier qu'elle apparaît ;
3. parcourir, ajouter le dossier récursivement, vérifier les trois pistes ;
4. enregistrer la liste sous un nom, recharger la page, la recharger depuis le
   stockage interne, vérifier qu'on retrouve trois pistes ;
5. revenir à l'accueil et vérifier que la grille de présélections affiche
   trois numéros.

- [ ] **Step 2 : lancer**

Run: `cargo build --workspace && npm run e2e -w app`
Expected: PASS.

- [ ] **Step 3 : commit**

```bash
git add -A
git commit -m "test(e2e): le parcours complet de constitution d une liste locale

Racine locale plutot que SMB : le parcours doit tourner sans NAS ni montage,
donc sans privilege, sur la machine de developpement comme en integration."
```

---

## Vérification finale

Avant de proposer la fusion :

- [ ] `cargo test --workspace` — vert
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — vert
- [ ] `npm test --workspaces` — vert
- [ ] `npm run build --workspaces` — vert, sorties à plat
- [ ] `cargo build --workspace && npm run e2e -w app` — vert
- [ ] Les trois points du §13 de la spec vérifiés **sur le Pi** : propagation
      du montage dans l'espace de noms durci, présence de `cifs-utils`,
      négociation du dialecte SMB avec le NAS visé.
