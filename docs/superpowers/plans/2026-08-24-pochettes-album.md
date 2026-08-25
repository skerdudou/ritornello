# Pochettes d'album et protocole `metadata` en étages — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Afficher la pochette de ce qui joue — fichier posé à côté, image embarquée, image de la station, Cover Art Archive — en refondant le protocole `metadata` pour que chaque contributeur déclare s'il écrase ou complète.

**Architecture:** `NowPlaying` transporte désormais l'état partiel du morceau (`known`), et `Enrichment` porte un `cover` (URL externe ou chemin local) plus un `fill_only` qui dit l'intention. Le cœur arbitre champ par champ, télécharge l'image dans une tâche détachée, la retient dans un cache mémoire borné et la sert sur `/api/cover/{clé}` — le navigateur ne contacte jamais l'extérieur.

**Tech Stack:** Rust (workspace cargo), axum 0.7, reqwest 0.12 + rustls, lofty 0.25, tokio, serde ; Vue 3 + vitest pour l'IHM.

**Spec:** `docs/superpowers/specs/2026-08-24-pochettes-album-design.md` — à lire avant la première tâche. Le plan argumente depuis elle.

## Global Constraints

- **`cargo` n'existe que dans WSL** sur cette machine. Toute commande Rust se lance ainsi, depuis n'importe quel répertoire :
  `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p <crate>'`
  Vérifié au moment d'écrire ce plan : `cargo 1.98.0`, et `cargo test -p ritornello-proto` passe (51 tests).
- **`cargo test` sur le cœur ne demande plus que les IHM soient construites.** Mesuré dans ce worktree le 2026-08-24 : `cargo test -p ritornello-core` passe ses 317 tests sans aucun `npm run build`, le script de construction embarquant un bouchon et le signalant par un avertissement (« IHM web non construite : bouchon embarque a la place »). Ces avertissements sont donc normaux et ne sont pas des constats de revue. Seule la tâche 11 a réellement besoin de la chaîne npm, pour `vitest`.
- **Toute tâche qui touche un type partagé du protocole finit par `cargo check --workspace --tests`, pas seulement par `cargo test -p <crate>`.** Amendement ajouté après la tâche 1, où il a coûté un tour complet de correction : ajouter un champ à une structure publique invalide **tous** ses littéraux, et la tâche 1 en a cassé 40 répartis dans le SDK, le cœur et les trois greffons. Un `-p` ne voit rien de tout cela.
- **Tests web** : `npx vitest run` **depuis `web/app`** (jamais depuis la racine, sinon vitest ratisse 41 fichiers sans plugin Vue). Un worktree neuf a besoin de deux jonctions, à créer avec `New-Item -ItemType Junction` (pas besoin d'admin) :
  - `web/app/node_modules/vue-router` → `C:\projets\perso\ritornello\web\app\node_modules\vue-router`
  - `<worktree>/node_modules/@ritornello/ui` → `<worktree>/web/kit`
  **Ne jamais joindre `vite`** : deux instances coexistent alors et tous les `.vue` échouent.
- **Messages de commit : français, sans accents**, préfixe conventionnel avec les crates touchés — `feat(core,proto): ...`, `test(files): ...`. C'est la convention de tout l'historique.
- **Commentaires de code en français**, messages de log et de journal **en anglais** : c'est la règle du chantier i18n, et un test refuse qu'une clé de catalogue atteigne l'écran.
- **Parité en/fr obligatoire** pour toute clé i18n neuve, vérifiée par un test Rust. Anglais : `crates/ritornello-core/src/locales/en.toml`. Français : `deploy/locales/core/fr.toml`.
- **Compatibilité des trames** : tout champ neuf est `#[serde(default)]`, et les champs optionnels sont `skip_serializing_if`. Un test par structure doit prouver qu'une trame sans les champs neufs se relit et qu'une trame muette est identique à l'octet près à l'actuelle.
- **`front-500`, jamais `front` nu** pour le Cover Art Archive : mesuré à 75 249 octets contre 2 670 705.

---

### Task 1: Les champs du protocole

**Files:**
- Modify: `crates/ritornello-proto/src/metadata.rs` (structures `NowPlaying`, `Enrichment`, `Morceau` ; tests en fin de fichier)

**Interfaces:**
- Consumes: rien.
- Produces: `Known { artist: Option<String>, title: Option<String>, album: Option<String>, duration_s: Option<u32>, cover: bool }` (dérive `Default`, `Clone`, `Debug`, `PartialEq`, `Serialize`, `Deserialize`) ; `CoverRef::Url { url: String }` et `CoverRef::Path { path: String }` ; `NowPlaying.known: Known` ; `Enrichment.cover: Option<CoverRef>` ; `Enrichment.fill_only: bool` ; `Morceau.cover_href: Option<String>` ; `Morceau.cover_origin: Option<String>` ; `Enrichment::cleaned()` qui valide `cover`.

- [ ] **Step 1: Write the failing tests**

Ajouter en fin du module `tests` de `crates/ritornello-proto/src/metadata.rs` :

```rust
    #[test]
    fn known_fait_un_aller_retour_et_se_relit_absent() {
        let np = NowPlaying {
            source: "files".into(),
            identity: Some(json!({"kind": "file", "path": "/mnt/nas/a.flac"})),
            known: Known {
                artist: Some("Lou Reed".into()),
                title: Some("Oooh Baby".into()),
                album: None,
                duration_s: Some(218),
                cover: true,
            },
        };
        let back: NowPlaying = serde_json::from_str(&serde_json::to_string(&np).unwrap()).unwrap();
        assert_eq!(back, np);

        // Une trame ecrite par un binaire anterieur n'a pas de `known` : elle
        // doit se relire, sinon la refonte ne peut pas se deployer greffon par
        // greffon.
        let ancienne = r#"{"source":"radio","identity":{"kind":"stream"}}"#;
        let relue: NowPlaying = serde_json::from_str(ancienne).unwrap();
        assert_eq!(relue.known, Known::default());
        assert!(!relue.known.cover);
    }

    #[test]
    fn cover_ref_a_deux_formes_distinctes() {
        let url = CoverRef::Url { url: "https://coverartarchive.org/release/x/front-500".into() };
        let json = serde_json::to_string(&url).unwrap();
        assert!(json.contains(r#""kind":"url""#), "{json}");
        assert_eq!(serde_json::from_str::<CoverRef>(&json).unwrap(), url);

        let chemin = CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() };
        let json = serde_json::to_string(&chemin).unwrap();
        assert!(json.contains(r#""kind":"path""#), "{json}");
        assert_eq!(serde_json::from_str::<CoverRef>(&json).unwrap(), chemin);
    }

    #[test]
    fn cleaned_refuse_une_url_qui_n_est_pas_https_vers_un_hote() {
        // Ces valeurs viennent du reseau : le champ `coverUrl` de la trame SSE
        // d'OUI FM est ecrit par un tiers, et c'est le coeur qui irait la
        // chercher. Sans ce filtre, une trame hostile fait emettre a l'appareil
        // une requete vers l'adresse de son choix sur le reseau local.
        for mauvaise in [
            "http://example.org/a.jpg",
            "https://192.168.1.1/admin",
            "https://[::1]/a.jpg",
            "file:///etc/shadow",
            "ftp://example.org/a.jpg",
            "pas une url",
            "",
        ] {
            let e = Enrichment {
                identity: json!(1),
                cover: Some(CoverRef::Url { url: mauvaise.into() }),
                ..Default::default()
            }
            .cleaned();
            assert!(e.cover.is_none(), "acceptee a tort : {mauvaise:?}");
        }
        let bonne = Enrichment {
            identity: json!(1),
            cover: Some(CoverRef::Url { url: " https://coverartarchive.org/x/front-500 ".into() }),
            ..Default::default()
        }
        .cleaned();
        assert_eq!(
            bonne.cover,
            Some(CoverRef::Url { url: "https://coverartarchive.org/x/front-500".into() })
        );
    }

    #[test]
    fn cleaned_refuse_un_chemin_relatif_ou_sans_extension_dimage() {
        for mauvais in ["relatif/folder.jpg", "/mnt/nas/notes.txt", "/mnt/nas/folder", ""] {
            let e = Enrichment {
                identity: json!(1),
                cover: Some(CoverRef::Path { path: mauvais.into() }),
                ..Default::default()
            }
            .cleaned();
            assert!(e.cover.is_none(), "accepte a tort : {mauvais:?}");
        }
        for bon in ["/mnt/nas/Album/folder.jpg", "/mnt/nas/A/Cover.JPEG", "/x/front.webp"] {
            let e = Enrichment {
                identity: json!(1),
                cover: Some(CoverRef::Path { path: bon.into() }),
                ..Default::default()
            }
            .cleaned();
            assert!(e.cover.is_some(), "refuse a tort : {bon:?}");
        }
    }

    #[test]
    fn une_pochette_seule_reste_une_non_reponse_pour_le_texte() {
        // Meme convention que `duration_s` : une pochette seule ne doit pas
        // gagner l'arbitrage du texte.
        let e = Enrichment {
            identity: json!(1),
            cover: Some(CoverRef::Url { url: "https://coverartarchive.org/x/front-500".into() }),
            ..Default::default()
        };
        assert!(e.is_empty());
    }

    #[test]
    fn fill_only_fait_le_tour_et_vaut_faux_par_defaut() {
        // Le defaut est « ecrase » : c'est la regle actuelle du projet, et
        // c'est ce qui evite de toucher aux trois greffons livres.
        let sans: Enrichment = serde_json::from_str(r#"{"identity":{"k":1}}"#).unwrap();
        assert!(!sans.fill_only);
        let e = Enrichment { identity: json!(1), fill_only: true, ..Default::default() };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""fill_only":true"#), "{json}");
        assert!(serde_json::from_str::<Enrichment>(&json).unwrap().fill_only);
        // Muet quand faux : la trame d'un greffon qui ecrase ne grossit pas.
        let defaut = Enrichment { identity: json!(1), ..Default::default() };
        assert!(!serde_json::to_string(&defaut).unwrap().contains("fill_only"));
    }

    #[test]
    fn morceau_tait_la_pochette_quand_il_n_y_en_a_pas() {
        let json = serde_json::to_string(&PlayerState::default()).unwrap();
        assert!(!json.contains("cover_href"), "{json}");
        assert!(!json.contains("cover_origin"), "{json}");

        let etat = PlayerState {
            source: "files".into(),
            morceau: Morceau {
                cover_href: Some("/api/cover/1a2b3c".into()),
                cover_origin: Some("files".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let json = serde_json::to_string(&etat).unwrap();
        assert!(json.contains(r#""cover_href":"/api/cover/1a2b3c""#), "{json}");
        assert!(json.contains(r#""cover_origin":"files""#), "{json}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-proto'`
Expected: FAIL à la compilation — `cannot find type Known`, `cannot find type CoverRef`, `no field known on NowPlaying`.

- [ ] **Step 3: Write the implementation**

Dans `crates/ritornello-proto/src/metadata.rs`, ajouter après l'enum `IdentityUpdate` :

```rust
/// L'état partiel du morceau, tel qu'un greffon a besoin de le voir.
///
/// Un type dédié plutôt que `Morceau` : ce dernier porte `cover_href` et
/// `cover_origin`, qui sont des URL **locales de l'appareil** — elles n'ont
/// aucun sens pour un greffon et l'inviteraient à croire qu'il peut les lire.
///
/// Un champ à `None` est un champ que personne n'a encore rempli. C'est ce qui
/// permet à un greffon de ne travailler que sur ce qui manque.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Known {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<u32>,
    /// Une pochette est **déjà tenue**. Un booléen, jamais l'image : un greffon
    /// n'a pas besoin de la voir pour décider s'il doit en chercher une, et la
    /// transmettre alourdirait chaque trame pour rien.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cover: bool,
}

/// Ce qu'un contributeur a trouvé comme pochette, à charge pour le cœur
/// d'aller la chercher.
///
/// Deux formes **explicitement distinctes** plutôt qu'une chaîne que le cœur
/// devinerait : le chemin sert au `folder.jpg` posé sur un partage, qui existe
/// déjà sur le disque — rien à extraire, aucun fichier temporaire.
///
/// Jamais des octets : le canal des greffons reste textuel, donc lisible à
/// l'œil dans un `journalctl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoverRef {
    /// URL externe, à télécharger. `https` uniquement, vers un nom d'hôte.
    Url { url: String },
    /// Chemin absolu d'un fichier image déjà présent sur le disque.
    Path { path: String },
}

/// Extensions acceptées pour un `CoverRef::Path`.
const EXTENSIONS_IMAGE: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

impl CoverRef {
    /// Normalise et **valide**. `None` = à jeter.
    ///
    /// Ces valeurs arrivent d'un autre processus et le cœur va agir dessus : il
    /// faut les traiter comme des entrées, pas comme des données de confiance.
    fn validee(self) -> Option<Self> {
        match self {
            Self::Url { url } => {
                let url = url.trim();
                let reste = url.strip_prefix("https://")?;
                let hote = reste.split(['/', '?', '#']).next().unwrap_or("");
                if hote.is_empty() || hote.contains('@') {
                    return None;
                }
                // Une adresse IP littérale est refusée : un nom d'hôte, et rien
                // d'autre. `[::1]` est écarté par le crochet, `192.168.1.1` par
                // le fait que tous ses libellés sont numériques.
                let sans_port = hote.split(':').next().unwrap_or("");
                if sans_port.starts_with('[')
                    || (!sans_port.is_empty()
                        && sans_port.split('.').all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_digit())))
                {
                    return None;
                }
                if !sans_port.contains('.') {
                    return None;
                }
                Some(Self::Url { url: url.to_string() })
            }
            Self::Path { path } => {
                let path = path.trim();
                if !path.starts_with('/') {
                    return None;
                }
                let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
                EXTENSIONS_IMAGE.contains(&ext.as_str()).then(|| Self::Path { path: path.to_string() })
            }
        }
    }
}
```

Puis, dans `NowPlaying`, ajouter le champ :

```rust
    /// Ce qui est **déjà connu** du morceau, tous étages confondus.
    ///
    /// `#[serde(default)]` : une trame écrite par un binaire antérieur se
    /// relit, et un greffon qui ignore le champ fonctionne exactement comme
    /// avant — c'est ce qui rend la refonte déployable greffon par greffon.
    #[serde(default)]
    pub known: Known,
```

Dans `Enrichment`, ajouter :

```rust
    /// La pochette que ce contributeur a trouvée.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<CoverRef>,
    /// Ce contributeur ne fait que **compléter** : il ne remplace aucun champ
    /// déjà renseigné.
    ///
    /// Défaut `false` = il écrase, ce qui est la règle actuelle du projet (« a
    /// plugin takes precedence over ICY and over file tags under all
    /// circumstances ») et ce qui évite de toucher aux greffons livrés.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fill_only: bool,
```

Dans `Morceau`, ajouter :

```rust
    /// URL **locale** de la pochette, à mettre telle quelle dans un `src`.
    /// Toujours de la forme `/api/cover/{clé}` : l'IHM ne contacte jamais
    /// l'extérieur.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_href: Option<String>,
    /// Qui a fourni cette pochette : le nom de la Source, `"tags"`, ou le nom
    /// du greffon. Une seconde origine, parce que le texte et l'image peuvent
    /// venir de deux contributeurs différents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_origin: Option<String>,
```

Et dans `Enrichment::cleaned()`, avant le `self` final :

```rust
        self.cover = self.cover.take().and_then(CoverRef::validee);
```

`is_empty()` **ne change pas** : il ne compte que les trois champs de texte, donc une pochette seule reste une non-réponse.

- [ ] **Step 4: Run tests to verify they pass**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-proto'`
Expected: PASS, et les 51 tests préexistants passent toujours.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-proto/src/metadata.rs
git commit -m "feat(proto): etat partiel, reference de pochette et intention declaree"
```

---

### Task 2: La pochette dans la notification d'une Source

Une Source déclare ses métadonnées sur **son propre canal**, sans devenir un greffon `metadata` : le canal existe déjà, `SourceMessage` accepte `id: None` comme notification spontanée.

**Files:**
- Modify: `crates/ritornello-proto/src/source.rs` (`SourceMessage`, vers la ligne 138)
- Modify: `crates/ritornello-plugin-sdk/src/server.rs` (`Notification` ligne 105, son builder, et la recopie vers `SourceMessage` vers la ligne 312)

**Interfaces:**
- Consumes: `CoverRef` (Task 1).
- Produces: `SourceMessage.cover: Option<CoverRef>` ; `Notification.cover: Option<CoverRef>` ; `Notification::cover(CoverRef) -> Self` (builder chaînable, comme `preset`).

- [ ] **Step 1: Write the failing tests**

Dans le module `tests` de `crates/ritornello-proto/src/source.rs` :

```rust
    #[test]
    fn le_message_de_source_porte_une_pochette_et_reste_muet_sans_elle() {
        let msg = SourceMessage {
            id: None,
            cover: Some(ritornello_proto_cover_de_test()),
            ..Default::default()
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""kind":"path""#), "{json}");
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cover, msg.cover);

        // Additif : une trame muette reste identique a l'octet pres a ce
        // qu'elle etait avant ce chantier.
        let muet = SourceMessage::default();
        assert!(!serde_json::to_string(&muet).unwrap().contains("cover"));
    }

    /// Fabrique locale : evite de repeter le chemin dans plusieurs tests.
    fn ritornello_proto_cover_de_test() -> crate::CoverRef {
        crate::CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() }
    }
```

Si `SourceMessage` ne dérive pas `Default`, remplacer `..Default::default()` par la construction explicite de tous ses champs — l'implémenteur lit la structure et complète.

Dans le module `tests` de `crates/ritornello-plugin-sdk/src/server.rs` :

```rust
    #[test]
    fn la_notification_porte_une_pochette_par_son_constructeur() {
        let n = Notification::new()
            .cover(ritornello_proto::CoverRef::Path { path: "/mnt/nas/A/cover.jpg".into() });
        assert_eq!(
            n.cover,
            Some(ritornello_proto::CoverRef::Path { path: "/mnt/nas/A/cover.jpg".into() })
        );
        // Les autres champs ne bougent pas : c'est le piege d'un builder.
        assert_eq!(n.preset, None);
        assert_eq!(n.status, None);
        assert!(!n.transient);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-proto -p ritornello-plugin-sdk'`
Expected: FAIL — `no field cover on SourceMessage`, `no method cover on Notification`.

- [ ] **Step 3: Write the implementation**

Dans `SourceMessage` (`crates/ritornello-proto/src/source.rs`) :

```rust
    /// Pochette que la Source a trouvée pour ce qu'elle joue.
    ///
    /// C'est ce qui permet à une Source de déclarer ses métadonnées **sans
    /// devenir un greffon `metadata`** : elle a l'information, elle la dit sur
    /// son canal. Envoyée en notification (`id: None`) plutôt qu'en réponse au
    /// `Play`, parce que la trouver peut demander un `readdir` sur un partage
    /// SMB, et que la lecture ne doit pas attendre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<crate::CoverRef>,
```

Dans `Notification` (`crates/ritornello-plugin-sdk/src/server.rs`) :

```rust
    /// Voir `SourceMessage::cover`.
    pub cover: Option<ritornello_proto::CoverRef>,
```

Son builder, à côté de `preset` :

```rust
    /// Voir `SourceMessage::cover`.
    pub fn cover(mut self, c: ritornello_proto::CoverRef) -> Self {
        self.cover = Some(c);
        self
    }
```

Et la recopie vers `SourceMessage`, dans le bras `notification` du `select!` (vers la ligne 312), en ajoutant à côté de `status: n.status` :

```rust
                            cover: n.cover,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-proto -p ritornello-plugin-sdk'`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-proto/src/source.rs crates/ritornello-plugin-sdk/src/server.rs
git commit -m "feat(proto,plugin-sdk): une Source declare sa pochette sur son canal"
```

---

### Task 3: Le cache de pochettes et la route qui les sert

Indépendante des tâches 4 à 12 : elle ne touche qu'un module neuf et le routeur. À faire tôt, parce que la tâche 5 en a besoin.

**Files:**
- Create: `crates/ritornello-core/src/cover.rs`
- Modify: `crates/ritornello-core/src/main.rs` (déclarer `mod cover;`)
- Modify: `crates/ritornello-core/src/status.rs` (`AppState` ligne 73, `router` ligne 105)
- Modify: `crates/ritornello-core/Cargo.toml` (ajouter `reqwest`)

**Interfaces:**
- Consumes: `CoverRef` (Task 1).
- Produces:
  - `pub fn cle(r: &CoverRef) -> String` — empreinte hexadécimale stable, calculable **avant** le téléchargement.
  - `pub enum Pochette { Octets(Vec<u8>, &'static str), Fichier(std::path::PathBuf) }`
  - `pub struct CoverCache` avec `pub fn new() -> Self`, `pub async fn insere(&self, cle: String, p: Pochette)`, `pub async fn contient(&self, cle: &str) -> bool`
  - `pub async fn recupere(r: &CoverRef) -> Option<Pochette>` — télécharge ou valide un fichier local ; `None` = échec silencieux.
  - `pub async fn cover_get(State<AppState>, Path<String>) -> Response` — le gestionnaire de route.
  - `AppState.covers: std::sync::Arc<CoverCache>`

- [ ] **Step 1: Write the failing tests**

Créer `crates/ritornello-core/src/cover.rs` avec **seulement** son module de tests dans un premier temps :

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::CoverRef;

    #[test]
    fn la_cle_est_stable_et_distingue_deux_sources() {
        let a = CoverRef::Url { url: "https://x.org/a.jpg".into() };
        let b = CoverRef::Url { url: "https://x.org/b.jpg".into() };
        assert_eq!(cle(&a), cle(&a), "la cle doit etre stable : elle est publiee dans une URL");
        assert_ne!(cle(&a), cle(&b));
        // Une forme differente pour la meme chaine ne doit pas collisionner.
        assert_ne!(cle(&a), cle(&CoverRef::Path { path: "/https://x.org/a.jpg".into() }));
        // Hexadecimal, donc sans surprise dans un chemin d'URL.
        assert!(cle(&a).chars().all(|c| c.is_ascii_hexdigit()), "{}", cle(&a));
    }

    #[tokio::test]
    async fn le_cache_est_borne_et_oublie_la_plus_ancienne() {
        let cache = CoverCache::new();
        for i in 0..6 {
            cache.insere(format!("k{i}"), Pochette::Octets(vec![i as u8], "image/jpeg")).await;
        }
        // Quatre entrees : la pochette courante et quelques precedentes. Un Pi
        // n'a pas a garder plus, et rien ne survit au redemarrage.
        assert!(!cache.contient("k0").await);
        assert!(!cache.contient("k1").await);
        assert!(cache.contient("k5").await);
    }

    #[tokio::test]
    async fn un_fichier_local_qui_n_est_pas_une_image_est_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let faux = dir.path().join("folder.jpg");
        std::fs::write(&faux, b"ceci n'est pas une image").unwrap();
        let r = CoverRef::Path { path: faux.to_string_lossy().into_owned() };
        assert!(
            recupere(&r).await.is_none(),
            "les octets d'en-tete doivent etre verifies : sans cela, un contributeur mal ecrit \
             ferait servir n'importe quel fichier du systeme sur une route HTTP publique"
        );

        let vrai = dir.path().join("cover.jpg");
        // En-tete JPEG minimal : SOI + marqueur APP0.
        std::fs::write(&vrai, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = CoverRef::Path { path: vrai.to_string_lossy().into_owned() };
        match recupere(&r).await {
            Some(Pochette::Fichier(p)) => assert_eq!(p, vrai),
            autre => panic!("une image locale doit rester un chemin, pas des octets : {autre:?}"),
        }
    }

    #[tokio::test]
    async fn la_route_rend_404_sur_une_cle_inconnue() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let app = crate::status::router(crate::status::AppState::pour_tests());
        let resp = app
            .oneshot(Request::get("/api/cover/inexistante").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
```

`AppState::pour_tests()` : réutiliser le constructeur de test déjà présent dans `status.rs` (chercher `fn app_state` ou `app_state_with_now_playing` dans son module `tests` et l'employer tel quel ; l'ajout du champ `covers` obligera à le compléter d'une ligne). `tempfile` est déjà une dépendance de dev du cœur — vérifier dans `Cargo.toml`, l'ajouter à `[dev-dependencies]` sinon.

- [ ] **Step 2: Run tests to verify they fail**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && npm run build --workspaces >/dev/null && cargo test -p ritornello-core cover'`
Expected: FAIL à la compilation — `cannot find function cle`, `cannot find type CoverCache`.

- [ ] **Step 3: Write the implementation**

En tête de `crates/ritornello-core/src/cover.rs` :

```rust
//! La pochette de ce qui joue : la chercher, la retenir, la servir.
//!
//! C'est **l'appareil** qui va chercher l'image, jamais le navigateur. Trois
//! raisons : la page ne doit charger aucune ressource externe — principe déjà
//! posé pour les pages d'admin ; l'image devient disponible à un futur
//! afficheur graphique ; et une pochette embarquée dans un fichier, que seul
//! l'appareil peut lire, n'aurait aucune URL à donner au navigateur.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use ritornello_proto::CoverRef;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use tokio::sync::RwLock;

/// Plafond d'une image venue du réseau. Écarte le `front` nu du Cover Art
/// Archive, mesuré à 2 670 705 octets là où `front-500` en rend 75 249.
const PLAFOND_RESEAU: usize = 2 * 1024 * 1024;

/// Nombre d'entrées retenues : la pochette courante et quelques précédentes.
const ENTREES: usize = 4;

/// Ce que le cœur retient d'une pochette.
///
/// Deux natures, et c'est délibéré : une pochette **locale** n'entre pas en
/// mémoire. Un `folder.jpg` de trois mégaoctets est banal sur un NAS, et le
/// charger en RAM sur un Pi pour une image que le navigateur cachera de son
/// côté serait du gaspillage.
#[derive(Debug, Clone)]
pub enum Pochette {
    /// Venue du réseau : les octets sont en mémoire.
    Octets(Vec<u8>, &'static str),
    /// Locale : seul le chemin est retenu, la route relit le fichier.
    Fichier(PathBuf),
}

/// Empreinte de la source, publiée dans l'URL locale.
///
/// `DefaultHasher` et non `sha2` : une collision ferait afficher la mauvaise
/// pochette et rien d'autre, ce qui ne justifie pas une dépendance
/// cryptographique. Calculable **avant** le téléchargement, ce qui permet de
/// dédupliquer deux demandes pour la même image.
pub fn cle(r: &CoverRef) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match r {
        CoverRef::Url { url } => {
            0u8.hash(&mut h);
            url.hash(&mut h);
        }
        CoverRef::Path { path } => {
            1u8.hash(&mut h);
            path.hash(&mut h);
        }
    }
    format!("{:016x}", h.finish())
}

#[derive(Default)]
pub struct CoverCache {
    entrees: RwLock<VecDeque<(String, Pochette)>>,
}

impl CoverCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insere(&self, cle: String, p: Pochette) {
        let mut e = self.entrees.write().await;
        e.retain(|(k, _)| k != &cle);
        e.push_back((cle, p));
        while e.len() > ENTREES {
            e.pop_front();
        }
    }

    pub async fn contient(&self, cle: &str) -> bool {
        self.entrees.read().await.iter().any(|(k, _)| k == cle)
    }

    async fn lit(&self, cle: &str) -> Option<Pochette> {
        self.entrees.read().await.iter().find(|(k, _)| k == cle).map(|(_, p)| p.clone())
    }
}

/// Octets d'en-tête d'une image reconnue. Vérifiés avant de servir un fichier
/// local : sans cela, un contributeur mal écrit ferait servir n'importe quel
/// fichier du système sur une route HTTP publique.
fn type_image(octets: &[u8]) -> Option<&'static str> {
    if octets.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if octets.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if octets.len() >= 12 && &octets[0..4] == b"RIFF" && &octets[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Va chercher la pochette. `None` = échec, et l'échec est **silencieux** :
/// l'appareil n'affiche simplement pas d'image.
pub async fn recupere(r: &CoverRef) -> Option<Pochette> {
    match r {
        CoverRef::Path { path } => {
            let chemin = PathBuf::from(path);
            let mut fichier = tokio::fs::File::open(&chemin).await.ok()?;
            let mut tete = [0u8; 12];
            use tokio::io::AsyncReadExt;
            let lus = fichier.read(&mut tete).await.ok()?;
            type_image(&tete[..lus])?;
            // Le plafond ne s'applique pas au local : il protège d'un tiers sur
            // le réseau, et un fichier du NAS est de confiance. Ses octets
            // d'en-tête ont été vérifiés, c'est ce qui compte.
            Some(Pochette::Fichier(chemin))
        }
        CoverRef::Url { url } => {
            let client = reqwest::Client::builder()
                .user_agent(concat!(
                    "ritornello/",
                    env!("CARGO_PKG_VERSION"),
                    " (https://github.com/skerdudou/ritornello)"
                ))
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .ok()?;
            let mut reponse = client.get(url).send().await.ok()?;
            if !reponse.status().is_success() {
                tracing::debug!("cover fetch returned {}", reponse.status());
                return None;
            }
            let mime = reponse
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            if !mime.starts_with("image/") {
                tracing::debug!("cover fetch refused: content-type {mime:?}");
                return None;
            }
            // Plafond appliqué **en lisant par morceaux** : contrôler le
            // `Content-Length` annoncé ne protège de rien, il est déclaratif.
            let mut octets = Vec::new();
            while let Some(morceau) = reponse.chunk().await.ok()? {
                if octets.len() + morceau.len() > PLAFOND_RESEAU {
                    tracing::debug!("cover fetch refused: over {PLAFOND_RESEAU} bytes");
                    return None;
                }
                octets.extend_from_slice(&morceau);
            }
            let mime = type_image(&octets)?;
            Some(Pochette::Octets(octets, mime))
        }
    }
}

/// `GET /api/cover/{clé}`. La clé est une empreinte, donc le contenu ne change
/// jamais sous elle : la réponse est immuable.
pub async fn cover_get(
    State(state): State<crate::status::AppState>,
    Path(cle): Path<String>,
) -> Response {
    let Some(p) = state.covers.lit(&cle).await else {
        return (StatusCode::NOT_FOUND, "inconnue").into_response();
    };
    let entetes = |mime: &str| {
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_string()),
            (header::ETAG, format!("\"{cle}\"")),
        ]
    };
    match p {
        Pochette::Octets(octets, mime) => (entetes(mime), octets).into_response(),
        Pochette::Fichier(chemin) => match tokio::fs::read(&chemin).await {
            Ok(octets) => {
                let mime = type_image(&octets).unwrap_or("application/octet-stream");
                (entetes(mime), octets).into_response()
            }
            // Le partage a pu disparaître entre la découverte et la requête.
            Err(e) => {
                tracing::debug!("cover file unreadable: {e}");
                (StatusCode::NOT_FOUND, "illisible").into_response()
            }
        },
    }
}
```

Dans `crates/ritornello-core/Cargo.toml`, section `[dependencies]` :

```toml
# Le cœur va chercher les pochettes lui-même : le navigateur ne contacte
# jamais l'extérieur. `rustls` comme les greffons, pas d'OpenSSL sur le Pi.
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "stream"] }
```

Dans `crates/ritornello-core/src/main.rs`, à côté des autres `mod` : `mod cover;`

Dans `AppState` (`status.rs`) :

```rust
    /// Pochettes retenues, servies sur `/api/cover/{clé}`. Un `Arc` : la
    /// tâche de téléchargement du cœur y insère, le routeur y lit.
    pub covers: Arc<crate::cover::CoverCache>,
```

Dans `router()` (`status.rs`), après `/api/command` :

```rust
        .route("/api/cover/:cle", get(crate::cover::cover_get))
```

Compléter tous les constructeurs d'`AppState` (production dans `main.rs`, et ceux du module `tests` de `status.rs`) avec `covers: Arc::new(crate::cover::CoverCache::new()),`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-core cover'`
Expected: PASS.

Puis la suite complète du cœur, pour prouver qu'aucun constructeur n'a été oublié :
Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-core'`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-core/src/cover.rs crates/ritornello-core/src/main.rs crates/ritornello-core/src/status.rs crates/ritornello-core/Cargo.toml Cargo.lock
git commit -m "feat(core): cache borne de pochettes et route qui les sert"
```

---

### Task 4: L'arbitrage en étages, avec les intentions

Le cœur de la refonte. Aujourd'hui `Metadonnees::etat()` renvoie l'enrichissement du gagnant **en bloc**. Il doit désormais composer.

**Une précision de conception que la spec laisse implicite et qu'il faut tenir :** le premier contributeur qui **écrase** fournit son bloc — comme aujourd'hui — et les contributeurs `fill_only` ne remplissent que les champs restés vides. On ne compose **pas** champ par champ entre deux contributeurs qui écrasent : cela mélangerait deux lectures du même flux (artiste de l'un, album de l'autre) et ferait afficher un morceau qui n'existe pas. Conséquence heureuse : le texte des trois greffons livrés ne change pas d'un iota.

**Files:**
- Modify: `crates/ritornello-core/src/metadata.rs` (structure `Metadonnees` ligne 33, `set_identity`, `etat`, nouveau `cover_retenue`)

**Interfaces:**
- Consumes: `Enrichment.fill_only`, `Enrichment.cover`, `CoverRef` (Task 1).
- Produces sur `Metadonnees` :
  - `pub fn set_cover_source(&mut self, c: Option<CoverRef>, origine: &str) -> bool`
  - `pub fn set_cover_tags(&mut self, c: Option<CoverRef>) -> bool`
  - `pub fn cover_retenue(&self) -> Option<(CoverRef, String)>` — la référence et son origine
  - `pub fn known(&self) -> ritornello_proto::Known`
  - `pub fn set_cover_href(&mut self, cle: Option<String>)` — la clé publiée, une fois les octets en main
  - `etat()` inchangé de signature, composition nouvelle

- [ ] **Step 1: Write the failing tests**

Dans le module `tests` de `crates/ritornello-core/src/metadata.rs` :

```rust
    /// Fabrique : un enrichissement qui ecrase, avec les champs donnes.
    fn ecrase(id: &Value, artist: Option<&str>, album: Option<&str>) -> Enrichment {
        Enrichment {
            identity: id.clone(),
            artist: artist.map(str::to_string),
            title: Some("T".into()),
            album: album.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn un_contributeur_qui_ecrase_fournit_son_bloc_et_le_fill_only_comble() {
        let id = json!({"kind": "stream"});
        let mut m = Metadonnees::new(vec!["specifique".into(), "generique".into()]);
        m.set_identity(Some(id.clone()));
        // Le specifique connait l'artiste, pas l'album.
        assert!(m.ajoute("specifique", ecrase(&id, Some("A"), None)));
        // Le generique complete : il ne remplace pas l'artiste, il remplit
        // l'album qui manquait.
        assert!(m.ajoute(
            "generique",
            Enrichment {
                identity: id.clone(),
                artist: Some("PAS LUI".into()),
                album: Some("ALBUM".into()),
                fill_only: true,
                ..Default::default()
            }
        ));
        let etat = m.etat();
        assert_eq!(etat.artist.as_deref(), Some("A"), "un fill_only ne remplace jamais");
        assert_eq!(etat.album.as_deref(), Some("ALBUM"), "un fill_only comble un trou");
        assert_eq!(etat.origin.as_deref(), Some("specifique"));
    }

    #[test]
    fn deux_contributeurs_qui_ecrasent_ne_sont_pas_melanges() {
        // Composer champ par champ entre deux qui ecrasent melangerait deux
        // lectures du meme flux et afficherait un morceau qui n'existe pas.
        let id = json!({"kind": "stream"});
        let mut m = Metadonnees::new(vec!["premier".into(), "second".into()]);
        m.set_identity(Some(id.clone()));
        m.ajoute("premier", ecrase(&id, Some("A"), None));
        m.ajoute("second", ecrase(&id, Some("B"), Some("ALBUM DU SECOND")));
        let etat = m.etat();
        assert_eq!(etat.artist.as_deref(), Some("A"));
        assert_eq!(etat.album, None, "le bloc du premier fait foi, trous compris");
    }

    #[test]
    fn la_pochette_suit_les_etages_source_puis_tags_puis_greffon() {
        let id = json!({"kind": "file", "path": "/mnt/nas/a.flac"});
        let mut m = Metadonnees::new(vec!["musicbrainz".into()]);
        m.set_identity(Some(id.clone()));

        // Le greffon seul : c'est lui qu'on retient.
        assert!(m.ajoute(
            "musicbrainz",
            Enrichment {
                identity: id.clone(),
                title: Some("T".into()),
                cover: Some(CoverRef::Url { url: "https://coverartarchive.org/x/front-500".into() }),
                fill_only: true,
                ..Default::default()
            }
        ));
        let (_, origine) = m.cover_retenue().expect("le greffon fournit une pochette");
        assert_eq!(origine, "musicbrainz");

        // La pochette embarquee, lue par le coeur, passe devant le greffon.
        assert!(m.set_cover_tags(Some(CoverRef::Path { path: "/tmp/embarquee.jpg".into() })));
        assert_eq!(m.cover_retenue().unwrap().1, ORIGINE_TAGS);

        // Le fichier pose a cote, declare par la Source, passe devant tout.
        assert!(m.set_cover_source(
            Some(CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() }),
            "files"
        ));
        let (r, origine) = m.cover_retenue().unwrap();
        assert_eq!(origine, "files");
        assert_eq!(r, CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() });
    }

    #[test]
    fn un_changement_didentite_vide_la_pochette_comme_le_reste() {
        let id = json!({"kind": "file", "path": "/a.flac"});
        let mut m = Metadonnees::new(vec![]);
        m.set_identity(Some(id));
        m.set_cover_source(Some(CoverRef::Path { path: "/a/folder.jpg".into() }), "files");
        m.set_cover_tags(Some(CoverRef::Path { path: "/b/embarquee.jpg".into() }));
        m.set_cover_href(Some("abcd".into()));
        assert!(m.set_identity(Some(json!({"kind": "file", "path": "/b.flac"}))));
        assert!(m.cover_retenue().is_none(), "laisser la pochette precedente serait plus trompeur que rien");
        assert!(m.etat().cover_href.is_none());
    }

    #[test]
    fn known_expose_ce_qui_est_connu_et_si_une_pochette_est_tenue() {
        let id = json!({"kind": "stream"});
        let mut m = Metadonnees::new(vec!["p".into()]);
        m.set_identity(Some(id.clone()));
        m.ajoute("p", ecrase(&id, Some("A"), None));
        let k = m.known();
        assert_eq!(k.artist.as_deref(), Some("A"));
        assert_eq!(k.album, None, "un champ vide est ce qui invite un contributeur a chercher");
        assert!(!k.cover);

        m.set_cover_tags(Some(CoverRef::Path { path: "/x/c.jpg".into() }));
        assert!(m.known().cover, "une pochette tenue doit faire taire un fill_only");
    }

    #[test]
    fn le_cover_href_publie_est_l_url_locale() {
        let id = json!({"kind": "file", "path": "/a.flac"});
        let mut m = Metadonnees::new(vec![]);
        m.set_identity(Some(id));
        m.set_cover_source(Some(CoverRef::Path { path: "/a/folder.jpg".into() }), "files");
        // Tant que les octets ne sont pas en main, rien n'est publie : l'IHM ne
        // doit jamais recevoir l'URL d'une image cassee.
        assert!(m.etat().cover_href.is_none());
        m.set_cover_href(Some("1a2b3c4d".into()));
        let etat = m.etat();
        assert_eq!(etat.cover_href.as_deref(), Some("/api/cover/1a2b3c4d"));
        assert_eq!(etat.cover_origin.as_deref(), Some("files"));
    }
```

Ajouter `use ritornello_proto::CoverRef;` aux imports du module de tests si besoin.

- [ ] **Step 2: Run tests to verify they fail**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-core metadata'`
Expected: FAIL — `no method set_cover_source`, `no method cover_retenue`, `no method known`.

- [ ] **Step 3: Write the implementation**

Dans `Metadonnees`, ajouter les champs :

```rust
    /// Pochette déclarée par la Source sur son canal, avec son origine.
    /// L'étage le plus bas, et pourtant le plus prioritaire pour l'image : le
    /// `folder.jpg` posé dans le répertoire est celui qu'on a choisi à la main.
    cover_source: Option<(CoverRef, String)>,
    /// Pochette embarquée dans le fichier, lue par le cœur.
    cover_tags: Option<CoverRef>,
    /// Clé du cache, une fois les octets en main. Tant qu'elle est `None`, rien
    /// n'est publié : l'IHM ne doit jamais recevoir l'URL d'une image cassée.
    cover_cle: Option<String>,
```

Dans `set_identity`, à côté des remises à zéro existantes :

```rust
        self.cover_source = None;
        self.cover_tags = None;
        self.cover_cle = None;
```

Les nouvelles méthodes :

```rust
    /// Retient la pochette déclarée par la Source. `true` si c'est du neuf.
    pub fn set_cover_source(&mut self, c: Option<CoverRef>, origine: &str) -> bool {
        let neuf = c.map(|r| (r, origine.to_string()));
        if self.cover_source == neuf {
            return false;
        }
        self.cover_source = neuf;
        // La référence retenue a changé : la clé publiée ne la décrit plus.
        self.cover_cle = None;
        true
    }

    /// Retient la pochette embarquée que le cœur a extraite. `true` si neuf.
    pub fn set_cover_tags(&mut self, c: Option<CoverRef>) -> bool {
        if self.cover_tags == c {
            return false;
        }
        self.cover_tags = c;
        self.cover_cle = None;
        true
    }

    /// La pochette qui gagne, et qui l'a fournie.
    ///
    /// L'ordre n'est pas une liste de priorités arbitraire : il découle des
    /// étages et des intentions. La Source d'abord — le fichier posé dans le
    /// répertoire est l'image choisie à la main. Le cœur ensuite, qui
    /// **complète** : il ne remplace pas ce que la Source a dit, et c'est ce
    /// qui donne au `folder.jpg` sa préséance sans qu'aucune convention n'ait
    /// à être inversée. Les greffons enfin, dans l'ordre de déclaration, un
    /// `fill_only` ne prenant la place de personne.
    pub fn cover_retenue(&self) -> Option<(CoverRef, String)> {
        if let Some((r, o)) = &self.cover_source {
            return Some((r.clone(), o.clone()));
        }
        if let Some(r) = &self.cover_tags {
            return Some((r.clone(), ORIGINE_TAGS.to_string()));
        }
        // Un greffon qui écrase d'abord, puis un `fill_only`. Deux passes
        // plutôt qu'une : sinon un `fill_only` déclaré haut dans
        // `plugins.toml` passerait devant un greffon spécialisé déclaré plus
        // bas, ce qui est exactement l'inverse de son intention.
        for ecrasant in [false, true] {
            for plugin in &self.ordre {
                if let Some(e) = self.enrichissements.get(plugin) {
                    if e.fill_only == ecrasant {
                        if let Some(r) = &e.cover {
                            return Some((r.clone(), plugin.clone()));
                        }
                    }
                }
            }
        }
        None
    }

    /// Publie la clé du cache. `None` = plus rien à montrer.
    pub fn set_cover_href(&mut self, cle: Option<String>) {
        self.cover_cle = cle;
    }

    /// Ce qui est déjà connu, tel qu'un contributeur a besoin de le voir.
    ///
    /// `cover` dit qu'une pochette est **tenue**, jamais laquelle : un
    /// contributeur n'a pas besoin de l'image pour décider s'il doit en
    /// chercher une.
    pub fn known(&self) -> ritornello_proto::Known {
        let m = self.etat();
        ritornello_proto::Known {
            artist: m.artist,
            title: m.title,
            album: m.album,
            duration_s: m.duration_s,
            cover: self.cover_retenue().is_some(),
        }
    }
```

Réécrire `etat()`. Garder toute sa documentation existante et lui ajouter l'explication de la composition :

```rust
    pub fn etat(&self) -> Morceau {
        let mut m = self.bloc_de_texte();
        // Les `fill_only` comblent les trous du bloc, sans jamais le
        // contredire. On ne compose pas champ par champ entre deux
        // contributeurs qui écrasent : cela mélangerait deux lectures du même
        // flux — l'artiste de l'un, l'album de l'autre — et afficherait un
        // morceau qui n'existe pas.
        for plugin in &self.ordre {
            let Some(e) = self.enrichissements.get(plugin) else { continue };
            if !e.fill_only {
                continue;
            }
            if m.artist.is_none() {
                m.artist = e.artist.clone();
            }
            if m.title.is_none() {
                m.title = e.title.clone();
            }
            if m.album.is_none() {
                m.album = e.album.clone();
            }
            if m.duration_s.is_none() {
                m.duration_s = e.duration_s;
            }
        }
        if let (Some(cle), Some((_, origine))) = (&self.cover_cle, self.cover_retenue()) {
            m.cover_href = Some(format!("/api/cover/{cle}"));
            m.cover_origin = Some(origine);
        }
        m
    }

    /// Le bloc de texte du contributeur retenu : le premier greffon qui
    /// **écrase**, sinon les tags du fichier, sinon l'ICY brut, sinon rien.
    fn bloc_de_texte(&self) -> Morceau {
        for plugin in &self.ordre {
            if let Some(e) = self.enrichissements.get(plugin) {
                if e.fill_only {
                    continue;
                }
                return Morceau {
                    artist: e.artist.clone(),
                    title: e.title.clone(),
                    album: e.album.clone(),
                    duration_s: e.duration_s,
                    origin: Some(plugin.clone()),
                    ..Default::default()
                };
            }
        }
        if let Some(tags) = &self.tags {
            return tags.clone();
        }
        match &self.icy {
            Some(icy) => Morceau {
                title: Some(icy.clone()),
                origin: Some(ORIGINE_ICY.to_string()),
                ..Default::default()
            },
            None => Morceau::default(),
        }
    }
```

Ajuster `gagnant()` pour ignorer les `fill_only` : c'est l'instrument de débogage du texte, et un compléteur n'est pas le gagnant.

- [ ] **Step 4: Run tests to verify they pass**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-core'`
Expected: PASS — tous les tests d'arbitrage préexistants inclus. Un test préexistant qui échouerait signalerait un changement de comportement non voulu : le lire avant de le modifier.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-core/src/metadata.rs
git commit -m "feat(core): arbitrage en etages, la pochette suit les intentions"
```

---

### Task 5: Le cœur télécharge, publie, et diffuse l'état partiel

**Files:**
- Modify: `crates/ritornello-core/src/core.rs` (`set_identity` vers la ligne 395, `etat_lecteur` ligne 583, nouveaux champs et méthodes)
- Modify: `crates/ritornello-core/src/main.rs` (un bras de `select!` pour les pochettes arrivées)

**Interfaces:**
- Consumes: `cover::{cle, recupere, CoverCache, Pochette}` (Task 3) ; `Metadonnees::{cover_retenue, set_cover_href, known}` (Task 4).
- Produces sur `Core` : `pub fn lance_pochette(&mut self)` (détache la récupération si la référence retenue n'est pas encore en cache) ; `pub async fn pochette_arrivee(&mut self, cle: String)` ; le `NowPlaying` émis porte `known`.

- [ ] **Step 1: Write the failing test**

Dans le module `tests` de `crates/ritornello-core/src/core.rs` :

```rust
    #[tokio::test]
    async fn le_now_playing_emis_porte_letat_partiel() {
        let (mut core, mut np_rx, _etat_rx, _tmp) = core_de_test().await;
        core.set_identity(Some(serde_json::json!({"kind": "stream", "url": "u"})));
        core.handle_icy_title("OUI FM".into());
        core.publie_etat();
        // Un contributeur doit voir ce qui est deja connu, sinon il ne peut ni
        // completer ni s'abstenir.
        let np = np_rx.borrow_and_update().clone();
        assert_eq!(np.known.title.as_deref(), Some("OUI FM"));
        assert!(!np.known.cover);
    }

    #[tokio::test]
    async fn une_pochette_arrivee_devient_une_url_locale_dans_letat() {
        let (mut core, _np_rx, mut etat_rx, tmp) = core_de_test().await;
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_cover_de_source(Some(r.clone()), "files");
        core.lance_pochette();
        // La recuperation est detachee : on l'attend explicitement dans le test
        // plutot que de dormir, pour ne pas fabriquer un flake.
        let cle = crate::cover::cle(&r);
        let p = crate::cover::recupere(&r).await.expect("l'image de test doit etre lisible");
        core.app_covers().insere(cle.clone(), p).await;
        core.pochette_arrivee(cle.clone()).await;

        let etat = etat_rx.borrow_and_update().clone();
        assert_eq!(etat.morceau.cover_href.as_deref(), Some(&format!("/api/cover/{cle}")[..]));
        assert_eq!(etat.morceau.cover_origin.as_deref(), Some("files"));
    }
```

`core_de_test()` : le constructeur de test existant du module (chercher la fonction qui rend `(Core<FakePlayer>, watch::Receiver<NowPlaying>, watch::Receiver<PlayerState>, tempfile::TempDir)` vers la ligne 1467) — l'employer tel quel. `app_covers()` est un accesseur de test à ajouter s'il n'y en a pas.

- [ ] **Step 2: Run test to verify it fails**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-core core::tests'`
Expected: FAIL — `no field known on NowPlaying` (construction), `no method lance_pochette`.

- [ ] **Step 3: Write the implementation**

Dans `Core`, ajouter les champs :

```rust
    /// Cache partagé avec le routeur : la tâche détachée y dépose, la route y lit.
    covers: std::sync::Arc<crate::cover::CoverCache>,
    /// Résultats des récupérations détachées, consommés par la boucle de `main`.
    pochette_tx: tokio::sync::mpsc::Sender<String>,
    /// Clé dont la récupération est en vol, pour ne pas la lancer deux fois.
    pochette_en_vol: Option<String>,
```

Le `NowPlaying` émis dans `set_identity` (ligne ~403) gagne `known: self.metadonnees.known(),`. Faire de même partout où un `NowPlaying` est construit dans le crate — `grep -n "NowPlaying {" crates/ritornello-core/src` pour n'en oublier aucun.

Ajouter :

```rust
    /// Retient la pochette qu'une Source vient de déclarer.
    pub fn set_cover_de_source(&mut self, c: Option<ritornello_proto::CoverRef>, origine: &str) {
        if self.metadonnees.set_cover_source(c, origine) {
            self.lance_pochette();
            self.publie_etat();
        }
    }

    /// Détache la récupération de la pochette retenue, si elle n'est pas déjà
    /// en cache ni en vol.
    ///
    /// Détachée, parce qu'un téléchargement de dix secondes ne doit pas retenir
    /// la boucle qui répond aux commandes. Et **abandonnée si l'identité
    /// change** : c'est `pochette_arrivee` qui vérifie, à l'arrivée, que la clé
    /// décrit encore ce qui joue — même garde-fou que l'écho d'identité du
    /// texte, pour la même raison.
    pub fn lance_pochette(&mut self) {
        let Some((r, _)) = self.metadonnees.cover_retenue() else {
            self.metadonnees.set_cover_href(None);
            return;
        };
        let cle = crate::cover::cle(&r);
        if self.pochette_en_vol.as_deref() == Some(cle.as_str()) {
            return;
        }
        let covers = self.covers.clone();
        let tx = self.pochette_tx.clone();
        self.pochette_en_vol = Some(cle.clone());
        tokio::spawn(async move {
            if covers.contient(&cle).await {
                let _ = tx.send(cle).await;
                return;
            }
            match crate::cover::recupere(&r).await {
                Some(p) => {
                    covers.insere(cle.clone(), p).await;
                    let _ = tx.send(cle).await;
                }
                // Échec silencieux : l'appareil n'affiche pas d'image, et c'est
                // tout. Un 404 du Cover Art Archive est le cas courant.
                None => tracing::debug!("no cover for {cle}"),
            }
        });
    }

    /// Une récupération a abouti. Publie l'URL locale, si elle décrit encore
    /// ce qui joue.
    pub async fn pochette_arrivee(&mut self, cle: String) {
        let Some((r, _)) = self.metadonnees.cover_retenue() else { return };
        if crate::cover::cle(&r) != cle {
            // La pochette du morceau précédent : sans cette vérification, elle
            // s'installerait sur le suivant.
            return;
        }
        self.pochette_en_vol = None;
        self.metadonnees.set_cover_href(Some(cle));
        self.publie_etat();
    }
```

Dans `handle_enrichment` et dans le chemin qui appelle `set_tags`, appeler `self.lance_pochette();` après que l'arbitrage a changé.

Dans `main.rs`, créer le canal (`tokio::sync::mpsc::channel(4)`), passer l'`Arc<CoverCache>` au `Core` **et** à l'`AppState` (le même), et ajouter le bras :

```rust
                    Some(cle) = pochette_rx.recv() => {
                        core.pochette_arrivee(cle).await;
                    }
```

Consommer aussi `SourceMessage.cover` là où les autres champs de la trame sont traités : `core.set_cover_de_source(msg.cover, &nom_de_la_source)`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-core'`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-core/src/core.rs crates/ritornello-core/src/main.rs
git commit -m "feat(core): recuperation detachee de la pochette et diffusion de l etat partiel"
```

---

### Task 6: La pochette embarquée, lue par le cœur

**Files:**
- Modify: `crates/ritornello-core/src/player/mpv.rs` (`OBSERVEES` ligne 300, nouvelle fonction d'extraction)
- Modify: `crates/ritornello-core/src/core.rs` (traitement de la propriété `path`)
- Modify: `crates/ritornello-core/Cargo.toml` (`lofty`)

**Interfaces:**
- Consumes: `Metadonnees::set_cover_tags` (Task 4), `Core::lance_pochette` (Task 5).
- Produces: `pub fn pochette_embarquee(chemin: &str) -> Option<CoverRef>` dans `mpv.rs` ; `OBSERVEES` passe à 6 entrées avec `"path"`.

Le fichier extrait est écrit dans un fichier temporaire du répertoire d'exécution du cœur, puis référencé par un `CoverRef::Path` : cela garde une seule nature de pochette locale, et le cache ne charge rien en mémoire.

- [ ] **Step 1: Write the failing test**

Dans le module `tests` de `crates/ritornello-core/src/player/mpv.rs` :

```rust
    #[test]
    fn la_propriete_path_est_observee() {
        // Sans elle, le coeur ne sait jamais quel fichier mpv joue, et la
        // pochette embarquee n'est jamais lue. Le coeur ne lit pas le chemin
        // dans l'identite : il a fait un principe de ne jamais l'interpreter.
        assert!(OBSERVEES.contains(&"path"), "sans elle, aucune pochette embarquee");
    }

    #[test]
    fn un_flux_ne_declenche_aucune_extraction() {
        // Tente uniquement sur un chemin sans schema.
        assert!(pochette_embarquee("https://icecast.radiofrance.fr/fip-midfi.mp3").is_none());
        assert!(pochette_embarquee("http://ouifm3.ice.infomaniak.ch/ouifm3.mp3").is_none());
        assert!(pochette_embarquee("/n/existe/pas.flac").is_none());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-core mpv'`
Expected: FAIL — `OBSERVEES` a 5 entrées, `cannot find function pochette_embarquee`.

- [ ] **Step 3: Write the implementation**

`OBSERVEES` passe à `[&str; 6]` avec `"path"` ajouté, et son commentaire mentionne à quoi elle sert.

```rust
/// Extrait la pochette embarquée du fichier joué, dans un fichier temporaire.
///
/// Un fichier plutôt que des octets en mémoire : cela garde **une seule
/// nature** de pochette locale côté cache, qui ne charge alors rien en RAM.
///
/// Tenté uniquement sur un chemin **sans schéma** : un flux n'a pas de tag, et
/// `lofty` n'a rien à ouvrir sur une URL.
pub fn pochette_embarquee(chemin: &str) -> Option<ritornello_proto::CoverRef> {
    if chemin.contains("://") {
        return None;
    }
    let fichier = lofty::probe::Probe::open(chemin).ok()?.read().ok()?;
    let image = lofty::file::TaggedFileExt::primary_tag(&fichier)
        .or_else(|| lofty::file::TaggedFileExt::first_tag(&fichier))?
        .pictures()
        .first()?
        .clone();
    let extension = match image.mime_type() {
        Some(m) if m.as_str().contains("png") => "png",
        Some(m) if m.as_str().contains("webp") => "webp",
        _ => "jpg",
    };
    // Nommé d'après le fichier source : deux pistes du même album partagent la
    // même image et n'écrivent donc pas deux fois.
    let mut cible = std::env::temp_dir();
    cible.push(format!("ritornello-cover-{}.{extension}", crate::cover::cle(
        &ritornello_proto::CoverRef::Path { path: chemin.to_string() }
    )));
    std::fs::write(&cible, image.data()).ok()?;
    Some(ritornello_proto::CoverRef::Path { path: cible.to_string_lossy().into_owned() })
}
```

Les noms exacts de l'API `lofty` 0.25 sont à confirmer à la compilation (`TaggedFileExt`, `pictures()`, `mime_type()`) ; le crate est déjà utilisé dans `ritornello-plugin-files/src/duree.rs`, s'en inspirer pour le style d'import.

Dans `Cargo.toml` du cœur : `lofty = "0.25.1"` — la même version que le greffon `files`, pour ne pas dupliquer une branche dans `Cargo.lock`.

Dans `core.rs`, au traitement de la propriété `path` reçue de mpv : appeler `mpv::pochette_embarquee(&chemin)`, passer le résultat à `self.metadonnees.set_cover_tags(...)`, puis `self.lance_pochette()` et `self.publie_etat()` si c'est du neuf. **Ne pas extraire quand `self.metadonnees.known().cover` est déjà vrai** : le cœur complète, il n'écrase pas, et c'est une lecture de fichier évitée.

- [ ] **Step 4: Run tests to verify they pass**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-core'`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-core/src/player/mpv.rs crates/ritornello-core/src/core.rs crates/ritornello-core/Cargo.toml Cargo.lock
git commit -m "feat(core): la pochette embarquee du fichier joue, lue par lofty"
```

---

### Task 7: `files` cherche le fichier posé à côté

**Files:**
- Create: `crates/ritornello-plugin-files/src/pochette.rs`
- Modify: `crates/ritornello-plugin-files/src/main.rs` (déclarer le module ; émettre la notification)

**Interfaces:**
- Consumes: `CoverRef` (Task 1), `Notification::cover` (Task 2).
- Produces: `pub fn cherche(fichier: &std::path::Path) -> Option<CoverRef>`

- [ ] **Step 1: Write the failing tests**

Créer `crates/ritornello-plugin-files/src/pochette.rs` avec son module de tests :

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Fabrique un repertoire avec les fichiers nommes, et rend son chemin.
    fn arbre(noms: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for nom in noms {
            let chemin = dir.path().join(nom);
            std::fs::create_dir_all(chemin.parent().unwrap()).unwrap();
            std::fs::write(&chemin, b"x").unwrap();
        }
        dir
    }

    fn trouve(dir: &tempfile::TempDir) -> Option<String> {
        match cherche(&dir.path().join("01 - piste.flac")) {
            Some(ritornello_proto::CoverRef::Path { path }) => {
                Some(std::path::Path::new(&path).file_name().unwrap().to_string_lossy().into_owned())
            }
            _ => None,
        }
    }

    #[test]
    fn l_ordre_de_preference_gagne_sur_l_ordre_alphabetique() {
        let dir = arbre(&["01 - piste.flac", "albumart.png", "cover.jpg", "front.jpg"]);
        assert_eq!(trouve(&dir).as_deref(), Some("cover.jpg"));
    }

    #[test]
    fn la_casse_ne_compte_pas() {
        let dir = arbre(&["01 - piste.flac", "Folder.JPG"]);
        assert_eq!(trouve(&dir).as_deref(), Some("Folder.JPG"));
    }

    #[test]
    fn une_image_unique_sans_nom_reconnaissable_est_prise() {
        let dir = arbre(&["01 - piste.flac", "scan001.png"]);
        assert_eq!(trouve(&dir).as_deref(), Some("scan001.png"));
    }

    #[test]
    fn une_image_unique_nommee_comme_un_dos_est_ecartee() {
        // Sans cette exclusion, on afficherait le dos du boitier. Et se taire
        // laisse le relai generique prendre la main.
        for dos in ["back.jpg", "Scan_verso.png", "inlay.jpg", "booklet.png", "cd.jpg"] {
            let dir = arbre(&["01 - piste.flac", dos]);
            assert_eq!(trouve(&dir), None, "{dos} ne devrait pas etre retenu");
        }
    }

    #[test]
    fn deux_images_sans_nom_reconnaissable_ne_tranchent_rien() {
        let dir = arbre(&["01 - piste.flac", "scan001.png", "scan002.png"]);
        assert_eq!(trouve(&dir), None);
    }

    #[test]
    fn l_exclusion_ne_s_applique_pas_a_la_liste_de_preference() {
        // `cd` est un motif d'exclusion, mais un fichier nomme `cover.jpg` est
        // retenu sans discussion : l'exclusion ne concerne que la regle qui
        // devine.
        let dir = arbre(&["01 - piste.flac", "cover.jpg", "back.jpg"]);
        assert_eq!(trouve(&dir).as_deref(), Some("cover.jpg"));
    }

    #[test]
    fn un_sous_repertoire_d_artwork_est_visite_sur_un_seul_niveau() {
        let dir = arbre(&["01 - piste.flac", "Artwork/front.jpg"]);
        assert_eq!(trouve(&dir).as_deref(), Some("front.jpg"));
        // Deux niveaux : on ne parcourt pas un NAS pour trouver une image.
        let profond = arbre(&["01 - piste.flac", "Artwork/haute-def/front.jpg"]);
        assert_eq!(trouve(&profond), None);
    }

    #[test]
    fn le_repertoire_passe_devant_le_sous_repertoire() {
        let dir = arbre(&["01 - piste.flac", "folder.jpg", "Artwork/cover.jpg"]);
        assert_eq!(trouve(&dir).as_deref(), Some("folder.jpg"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-plugin-files pochette'`
Expected: FAIL — `cannot find function cherche`.

- [ ] **Step 3: Write the implementation**

```rust
//! La pochette posée à côté des fichiers : `folder.jpg` et ses cousins.
//!
//! C'est le greffon qui fait ce travail, et non le cœur : c'est lui qui a monté
//! le partage et qui connaît la racine de la source déclarée. Et un
//! `folder.jpg` n'a rien à extraire — le chemin suffit, donc rien ne transite
//! en octets sur le canal.

use ritornello_proto::CoverRef;
use std::path::{Path, PathBuf};

/// Par ordre de préférence. `cover` d'abord : c'est le nom le plus explicite.
const PREFERENCES: [&str; 5] = ["cover", "folder", "front", "albumart", "album"];

/// Extensions reconnues.
const EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

/// Sous-répertoires d'artwork visités, sur **un seul** niveau.
const SOUS_REPERTOIRES: [&str; 4] = ["artwork", "scans", "covers", "art"];

/// Ce qui n'est pas la face avant.
///
/// Ne s'applique **qu'à la règle de l'image unique**, la seule qui devine : les
/// listes de préférence ne retiennent qu'un nom qu'elles connaissent, donc un
/// répertoire portant `front.jpg` et `back.jpg` est réglé par la préférence.
const EXCLUS: [&str; 8] =
    ["back", "verso", "inlay", "cd", "disc", "disque", "booklet", "matrix"];

/// Cherche la pochette du fichier joué. `None` = rien de sûr, on se tait.
pub fn cherche(fichier: &Path) -> Option<CoverRef> {
    let repertoire = fichier.parent()?;
    if let Some(p) = par_preference(repertoire) {
        return Some(chemin(p));
    }
    for sous in SOUS_REPERTOIRES {
        let Some(candidat) = sous_repertoire(repertoire, sous) else { continue };
        if let Some(p) = par_preference(&candidat) {
            return Some(chemin(p));
        }
    }
    image_unique(repertoire).map(chemin)
}

fn chemin(p: PathBuf) -> CoverRef {
    CoverRef::Path { path: p.to_string_lossy().into_owned() }
}

/// Le sous-répertoire d'artwork, quel que soit sa casse.
fn sous_repertoire(repertoire: &Path, nom: &str) -> Option<PathBuf> {
    std::fs::read_dir(repertoire)
        .ok()?
        .flatten()
        .find(|e| {
            e.file_name().to_string_lossy().eq_ignore_ascii_case(nom)
                && e.file_type().is_ok_and(|t| t.is_dir())
        })
        .map(|e| e.path())
}

/// Le premier nom de la liste de préférence présent dans le répertoire.
fn par_preference(repertoire: &Path) -> Option<PathBuf> {
    let images = images_de(repertoire);
    PREFERENCES.iter().find_map(|prefere| {
        images
            .iter()
            .find(|p| {
                p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(prefere))
            })
            .cloned()
    })
}

/// L'unique image du répertoire, si elle est unique **et** si son nom ne dit
/// pas qu'elle est autre chose que la face avant.
fn image_unique(repertoire: &Path) -> Option<PathBuf> {
    let images = images_de(repertoire);
    let [seule] = images.as_slice() else { return None };
    let tige = seule.file_stem()?.to_string_lossy().to_ascii_lowercase();
    EXCLUS.iter().all(|exclu| !tige.contains(exclu)).then(|| seule.clone())
}

fn images_de(repertoire: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(repertoire)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().is_some_and(|e| {
                    EXTENSIONS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str())
                })
        })
        .collect();
    // `read_dir` ne garantit aucun ordre : trier rend le choix reproductible.
    v.sort();
    v
}
```

Dans `main.rs` du greffon : déclarer `mod pochette;`, et dans `poll_notification`, après qu'un `Play` a fixé le fichier courant, émettre `Notification::new().cover(pochette::cherche(&chemin)?)`. La recherche fait un `read_dir` sur un partage : la faire **dans la notification** et non dans la réponse au `Play` est exactement ce que la décision 7 de la spec demande.

Vérifier que `tempfile` est en `[dev-dependencies]` du greffon.

- [ ] **Step 4: Run tests to verify they pass**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-plugin-files'`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-plugin-files/src/pochette.rs crates/ritornello-plugin-files/src/main.rs
git commit -m "feat(files): la pochette posee dans le repertoire, annoncee par notification"
```

---

### Task 8: `radiofrance-metas` — la pochette, jamais un placeholder

**Files:**
- Modify: `crates/ritornello-plugin-radiofrance-metas/src/live.rs` (`Direct`, `parse_direct` ligne 125, nouvelle fonction d'URL)
- Modify: `crates/ritornello-plugin-radiofrance-metas/src/main.rs` (poser `cover` sur l'`Enrichment`)

**Interfaces:**
- Consumes: `CoverRef` (Task 1).
- Produces: `Direct.cover: Option<String>` (l'UUID brut) ; `pub fn url_pochette(uuid: &str) -> String`.

- [ ] **Step 1: Write the failing tests**

Dans le module `tests` de `live.rs` — les deux fixtures nécessaires **existent déjà** (`REPONSE_FIP` ligne 296, `REPONSE_LOCALE_MUETTE` ligne 305) :

```rust
    #[test]
    fn l_url_de_pochette_suit_le_motif_mesure() {
        // Mesure du 2026-08-24 : ce motif rend un 301 vers le CDN, puis un
        // JPEG de 31 887 octets. `preset` est obligatoire — sans lui, 400.
        assert_eq!(
            url_pochette("24abdb92-7220-45c6-8434-a325278efa2b"),
            "https://api.radiofrance.fr/v1/services/embed/image/24abdb92-7220-45c6-8434-a325278efa2b?preset=400x400"
        );
    }

    #[test]
    fn la_pochette_d_un_vrai_morceau_est_retenue() {
        let d = parse_direct(REPONSE_FIP).unwrap();
        assert_eq!(d.cover.as_deref(), Some("5b93ce44-3ed6-4409-a2d7-4bd159c061f8"));
    }

    #[test]
    fn la_pochette_est_tue_quand_ce_n_est_pas_un_morceau() {
        // La station sert une image generique pour « Le direct » et pour ses
        // emissions. L'annoncer ferait taire le relai generique : un champ
        // rempli est un champ rempli, aucun etage superieur ne peut savoir
        // qu'il l'est mal. Le critere est `songUuid`, deja extrait.
        let d = parse_direct(REPONSE_LOCALE_MUETTE).unwrap();
        assert_eq!(d.song_uuid, None, "prealable du test");
        assert_eq!(d.cover, None);

        // Une entree « Le direct » : songUuid nul a cote d'un cover rempli.
        let direct = r#"{"now":{"firstLine":"Le direct","secondLine":"La radio la plus eclectique du monde","songUuid":null,"cover":"7eee98cb-3f59-4a3b-b921-6a4be85af542"},"delayToRefresh":70000}"#;
        assert_eq!(parse_direct(direct).unwrap().cover, None);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-plugin-radiofrance-metas'`
Expected: FAIL — `no field cover on Direct`, `cannot find function url_pochette`.

- [ ] **Step 3: Write the implementation**

Dans `Direct` :

```rust
    /// UUID de la pochette, **seulement quand un vrai morceau joue**.
    ///
    /// La station renseigne un `cover` même pour « Le direct » et pour ses
    /// émissions : c'est l'image générique de l'antenne. L'annoncer ferait
    /// taire le relai générique, puisqu'un champ rempli est un champ rempli et
    /// qu'aucun étage supérieur ne peut savoir qu'il l'est mal.
    pub cover: Option<String>,
```

Dans `parse_direct`, à la construction du `Direct` :

```rust
    let song_uuid = texte(now, "songUuid");
    // Le `songUuid` est le seul discriminant fiable entre un morceau et une
    // émission — mesuré sur quatre stations.
    let cover = song_uuid.as_ref().and_then(|_| texte(now, "cover"));
    Some(Direct { meta, song_uuid, cover, recontacter })
```

Et la fonction d'URL, à côté de `url_grille` :

```rust
/// URL de la pochette d'un morceau.
///
/// `preset` n'est pas optionnel : sans lui, l'API rend un 400. Avec, elle rend
/// un 301 vers le CDN, que le cœur suit. `400x400` est un compromis mesuré —
/// 31 887 octets, contre un original de taille non bornée.
pub fn url_pochette(uuid: &str) -> String {
    format!("https://api.radiofrance.fr/v1/services/embed/image/{uuid}?preset=400x400")
}
```

Dans `main.rs`, à la construction de l'`Enrichment`, ajouter :

```rust
            cover: direct.cover.as_deref().map(|u| CoverRef::Url { url: live::url_pochette(u) }),
            // Ce greffon lit le flux officiel de la station : il sait mieux que
            // l'ICY, par construction. Il écrase, donc `fill_only` reste faux.
            fill_only: false,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-plugin-radiofrance-metas'`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-plugin-radiofrance-metas/src/
git commit -m "feat(radiofrance-metas): la pochette du morceau, jamais celle de l antenne"
```

---

### Task 9: `ouifm-metas` — l'URL de la trame, sinon composée

**Files:**
- Modify: `crates/ritornello-plugin-ouifm-metas/src/flux.rs` (`Meta` ligne 13, `parse_data_line` ligne 71)
- Modify: `crates/ritornello-plugin-ouifm-metas/src/main.rs` (poser `cover`)

**Interfaces:**
- Consumes: `CoverRef` (Task 1).
- Produces: `Meta.cover: Option<String>` — l'URL finale, déjà composée.

- [ ] **Step 1: Write the failing tests**

Dans le module `tests` de `flux.rs`. La fixture `TRAME` ligne 205 porte déjà un `coverId`.

```rust
    #[test]
    fn le_cover_id_est_compose_selon_le_motif_du_lecteur() {
        // Motif pris dans le bundle `_app` de ouifm.fr/player, dans le code qui
        // lit ce meme flux SSE. Mesure du 2026-08-24 : JPEG de 35 613 octets.
        let m = parse_data_line(TRAME).unwrap();
        assert_eq!(
            m.cover.as_deref(),
            Some("https://www.lesindesradios.fr/servicesimb/images?version=6&iid=3134161803443976427/t/th/therollingstones/shesarainbow/214198016_1702973462000&width=400")
        );
    }

    #[test]
    fn une_url_toute_faite_dans_la_trame_est_preferee_si_l_hote_est_connu() {
        let connu = r#"data: {"title":"t","coverUrl":"https://www.lesindesradios.fr/x.jpg","coverId":"abc"}"#;
        assert_eq!(
            parse_data_line(connu).unwrap().cover.as_deref(),
            Some("https://www.lesindesradios.fr/x.jpg")
        );
        // Un hote inconnu est refuse : ce champ est ecrit par un tiers, et
        // c'est le coeur qui irait le chercher.
        let inconnu = r#"data: {"title":"t","coverUrl":"https://ailleurs.example/x.jpg","coverId":"abc"}"#;
        let compose = parse_data_line(inconnu).unwrap().cover.unwrap();
        assert!(compose.starts_with("https://www.lesindesradios.fr/"), "{compose}");
        assert!(compose.contains("iid=abc"), "{compose}");
    }

    #[test]
    fn sans_pochette_la_trame_reste_exploitable() {
        assert_eq!(parse_data_line(r#"data: {"title":"t"}"#).unwrap().cover, None);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-plugin-ouifm-metas'`
Expected: FAIL — `no field cover on Meta`.

- [ ] **Step 3: Write the implementation**

Dans `Meta` : `pub cover: Option<String>,`

Dans `parse_data_line`, avant la construction :

```rust
    // Le lecteur d'OUI FM fait exactement ceci : `coverUrl` s'il est là,
    // sinon `coverId` composé. Les deux cas sont réels sur le flux.
    let cover = texte(&v, "coverUrl")
        .filter(|u| u.starts_with(HOTE_IMAGES))
        .or_else(|| texte(&v, "coverId").map(|id| format!("{HOTE_IMAGES}/servicesimb/images?version=6&iid={id}&width=400")));
```

et le poser dans le `Meta`. Avec, en tête de fichier :

```rust
/// Hôte des images. Un `coverUrl` venu de la trame n'est accepté que s'il en
/// vient : ce champ est écrit par un tiers, et c'est le cœur qui irait le
/// chercher.
const HOTE_IMAGES: &str = "https://www.lesindesradios.fr";
```

Dans `main.rs`, à la construction de l'`Enrichment` : `cover: meta.cover.as_deref().map(|u| CoverRef::Url { url: u.to_string() }),` et `fill_only: false` avec le même commentaire que la tâche 8.

- [ ] **Step 4: Run tests to verify they pass**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-plugin-ouifm-metas'`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-plugin-ouifm-metas/src/
git commit -m "feat(ouifm-metas): la pochette du flux, url de la trame ou composee"
```

---

### Task 10: `musicbrainz` — le MBID du disque, et le relai générique

**Files:**
- Modify: `crates/ritornello-plugin-musicbrainz/src/musicbrainz.rs` (`DiscInfo` ligne 7, `parse_lookup` ligne 44, nouvelles fonctions)
- Modify: `crates/ritornello-plugin-musicbrainz/src/main.rs` (le relai générique)

**Interfaces:**
- Consumes: `CoverRef`, `Known` (Task 1).
- Produces: `DiscInfo.release_id: Option<String>` ; `pub fn url_caa(mbid: &str) -> String` ; `pub fn requete_release(artist: &str, album: &str) -> String` ; `pub fn premier_release_id(json: &str) -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

Dans `crates/ritornello-plugin-musicbrainz/tests/` (le répertoire existe, avec des fixtures) ou dans un module `tests` de `musicbrainz.rs` :

```rust
    #[test]
    fn l_url_du_cover_art_archive_demande_une_taille_bornee() {
        // `front` nu rend un PNG de 2 670 705 octets ; `front-500`, 75 249.
        assert_eq!(
            url_caa("e32a3f0b-1c19-3170-bb1c-650893774744"),
            "https://coverartarchive.org/release/e32a3f0b-1c19-3170-bb1c-650893774744/front-500"
        );
    }

    #[test]
    fn parse_lookup_retient_le_release_id() {
        // Le MBID est la cle de l'image, et il etait jete. Reutiliser la
        // fixture de lookup du repertoire tests/fixtures.
        let json = std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/lookup.json")
        )
        .expect("adapter au nom reel de la fixture");
        let info = parse_lookup(&json, nombre_de_pistes_de_la_fixture()).unwrap();
        assert!(
            info.release_id.as_deref().is_some_and(|id| id.len() == 36),
            "un MBID fait 36 caracteres, obtenu {:?}",
            info.release_id
        );
    }

    #[test]
    fn la_requete_de_release_echappe_les_guillemets() {
        // Mesure du 2026-08-24 : cette requete rend « Kind of Blue » au score 100.
        let q = requete_release("Miles Davis", "Kind of Blue");
        assert!(q.contains("artist:%22Miles%20Davis%22"), "{q}");
        assert!(q.contains("release:%22Kind%20of%20Blue%22"), "{q}");
        assert!(q.contains("fmt=json"), "{q}");
        assert!(q.contains("limit=1"), "{q}");
    }

    #[test]
    fn premier_release_id_lit_le_premier_resultat() {
        let json = r#"{"count":135,"releases":[{"id":"e32a3f0b-1c19-3170-bb1c-650893774744","score":100},{"id":"autre"}]}"#;
        assert_eq!(
            premier_release_id(json).as_deref(),
            Some("e32a3f0b-1c19-3170-bb1c-650893774744")
        );
        assert_eq!(premier_release_id(r#"{"releases":[]}"#), None);
        assert_eq!(premier_release_id("pas du json"), None);
    }
```

Et un test du garde-fou d'intention, dans `main.rs` du greffon :

```rust
    #[test]
    fn le_relai_generique_exige_un_artiste_et_un_album_et_se_tait_si_la_pochette_est_tenue() {
        use ritornello_proto::Known;
        // Jamais sur un titre ICY seul : c'est un texte brut, non decoupe, et
        // OUI FM emet « Titre - ARTISTE » dans l'ordre inverse de l'usage.
        assert!(!doit_chercher(&Known { title: Some("X - Y".into()), ..Default::default() }));
        assert!(!doit_chercher(&Known { artist: Some("A".into()), ..Default::default() }));
        assert!(!doit_chercher(&Known { album: Some("B".into()), ..Default::default() }));
        assert!(doit_chercher(&Known {
            artist: Some("A".into()),
            album: Some("B".into()),
            ..Default::default()
        }));
        // Une pochette deja tenue : l'appel serait jete.
        assert!(!doit_chercher(&Known {
            artist: Some("A".into()),
            album: Some("B".into()),
            cover: true,
            ..Default::default()
        }));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-plugin-musicbrainz'`
Expected: FAIL — `cannot find function url_caa`, `no field release_id`, `cannot find function doit_chercher`.

- [ ] **Step 3: Write the implementation**

Dans `DiscInfo` : `pub release_id: Option<String>,` — et dans `parse_lookup`, le renseigner depuis `release.get("id")`. Le champ est le seul ajout du chemin disque : aucune requête de plus, le lookup par TOC le portait déjà.

```rust
/// URL de la face avant d'une release.
///
/// `front-500` et non `front` : mesure du 2026-08-24, 75 249 octets contre
/// 2 670 705 pour l'original. Un 404 est le cas courant — beaucoup de releases
/// n'ont pas d'image — et le cœur le traite en silence.
pub fn url_caa(mbid: &str) -> String {
    format!("https://coverartarchive.org/release/{mbid}/front-500")
}

/// Requête de recherche d'une release par artiste et album.
pub fn requete_release(artist: &str, album: &str) -> String {
    let echappe = |s: &str| {
        s.chars()
            .map(|c| match c {
                ' ' => "%20".to_string(),
                '"' => "%22".to_string(),
                c => c.to_string(),
            })
            .collect::<String>()
    };
    format!(
        "https://musicbrainz.org/ws/2/release/?query=artist:%22{}%22%20AND%20release:%22{}%22&fmt=json&limit=1",
        echappe(artist),
        echappe(album)
    )
}

/// MBID du premier résultat. `None` = rien trouvé, ou réponse illisible.
pub fn premier_release_id(json: &str) -> Option<String> {
    serde_json::from_str::<Value>(json)
        .ok()?
        .get("releases")?
        .as_array()?
        .first()?
        .get("id")?
        .as_str()
        .map(str::to_string)
}
```

Dans `main.rs` du greffon, la décision, extraite pour être testable :

```rust
/// Faut-il chercher une pochette pour cet état partiel ?
///
/// Un artiste **et** un album, jamais un titre ICY seul : ce dernier est un
/// texte brut, non découpé exprès dans ce projet, et OUI FM émet
/// `Titre - ARTISTE` dans l'ordre inverse de l'usage — le donner à MusicBrainz
/// rendrait n'importe quoi avec assurance.
///
/// Et rien à faire si une pochette est déjà tenue : ce greffon **complète**,
/// donc l'appel serait jeté.
fn doit_chercher(known: &ritornello_proto::Known) -> bool {
    !known.cover && known.artist.is_some() && known.album.is_some()
}
```

Le chemin générique suit le motif déjà en place dans ce greffon pour le disque : une requête détachée dont le résultat revient par un `mpsc`, mémorisée pour ne pas être relancée. L'`Enrichment` qu'il produit porte `fill_only: true` et **seulement** `cover` (aucun champ de texte : il ne sait rien de plus que ce qu'on lui a donné).

Le chemin disque garde `fill_only: false` : il tient la TOC, donc il sait ce qui joue.

- [ ] **Step 4: Run tests to verify they pass**

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-plugin-musicbrainz'`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-plugin-musicbrainz/
git commit -m "feat(musicbrainz): mbid du disque retenu et relai generique de pochette"
```

---

### Task 11: La pochette dans la carte Player

**Files:**
- Modify: `web/app/src/types.ts` (le type de l'état poussé)
- Modify: `web/app/src/components/PlayerCard.vue` (bloc `data-now-playing`, vers la ligne 100)
- Test: `web/app/src/components/PlayerCard.test.ts`
- Modify: `crates/ritornello-core/src/locales/en.toml` et `deploy/locales/core/fr.toml`

**Interfaces:**
- Consumes: `cover_href`, `cover_origin` de la charge utile (Task 1).
- Produces: rien pour les tâches suivantes.

- [ ] **Step 1: Write the failing tests**

Dans `web/app/src/components/PlayerCard.test.ts` :

```ts
  it("affiche la pochette quand l'appareil en sert une", async () => {
    const wrapper = monte({ ...etatDeBase, title: 'So What', cover_href: '/api/cover/1a2b', cover_origin: 'files' })
    const img = wrapper.find('[data-pochette] img')
    expect(img.exists()).toBe(true)
    // L'IHM ne doit jamais pointer vers l'exterieur : le coeur sert l'image.
    expect(img.attributes('src')).toBe('/api/cover/1a2b')
    expect(wrapper.find('[data-cover-origin]').text()).toContain('files')
  })

  it('garde le carre en place quand il n y a pas de pochette', async () => {
    const wrapper = monte({ ...etatDeBase, title: 'So What' })
    // Le carre existe toujours : la pochette arrive apres le texte, parfois
    // plusieurs secondes apres, et un carre qui apparait decalerait tout.
    expect(wrapper.find('[data-pochette]').exists()).toBe(true)
    expect(wrapper.find('[data-pochette] img').exists()).toBe(false)
    expect(wrapper.find('[data-pochette-repli]').exists()).toBe(true)
  })
```

`monte()` et `etatDeBase` : réutiliser les utilitaires déjà présents en tête de ce fichier de test — les lire et s'y conformer plutôt que d'en créer.

- [ ] **Step 2: Run tests to verify they fail**

Run (depuis `web/app`) : `npx vitest run src/components/PlayerCard.test.ts`
Expected: FAIL — aucun élément `[data-pochette]`.

Si l'exécution échoue sur `Failed to resolve import "vue-router"` ou `@/lib/utils`, créer les deux jonctions décrites dans les contraintes globales — ce n'est pas un défaut du code.

- [ ] **Step 3: Write the implementation**

Dans `web/app/src/types.ts`, ajouter au type de l'état :

```ts
  /** URL locale de la pochette, servie par l'appareil. Jamais une URL externe. */
  cover_href: string | null
  /** Qui a fourni la pochette : nom de la Source, `tags`, ou nom du greffon. */
  cover_origin: string | null
```

Dans `PlayerCard.vue`, envelopper le bloc `data-now-playing` d'un conteneur horizontal, avec le carré à gauche :

```vue
      <div v-if="!riendAfficher(etat)" class="mt-3 flex gap-3 border-t border-border pt-3" data-now-playing>
        <!-- Le carre est la meme quand l'image manque : elle arrive apres le
             texte, parfois plusieurs secondes apres, et un carre qui apparait
             decalerait toute la carte. -->
        <div
          class="size-20 shrink-0 overflow-hidden rounded-md border border-border bg-muted"
          data-pochette
        >
          <img
            v-if="etat?.cover_href"
            :src="etat.cover_href"
            :alt="t('cover_alt')"
            class="size-full object-cover"
          />
          <div
            v-else
            class="flex size-full items-center justify-center text-muted-foreground"
            data-pochette-repli
            aria-hidden="true"
          >
            ♫
          </div>
        </div>
        <div class="min-w-0 flex-1">
          <!-- le contenu existant du bloc, inchange -->
        </div>
      </div>
```

Et le badge d'origine de la pochette, à côté de celui du texte :

```vue
          <Badge v-if="etat?.cover_origin" variant="secondary" class="text-[10px]" data-cover-origin>
            {{ etat.cover_origin }}
          </Badge>
```

Les deux catalogues, avec la **même clé** : `cover_alt = "Album cover"` dans `crates/ritornello-core/src/locales/en.toml`, `cover_alt = "Pochette de l'album"` dans `deploy/locales/core/fr.toml`. La parité est vérifiée par un test Rust — l'oublier fait échouer `cargo test -p ritornello-core`.

- [ ] **Step 4: Run tests to verify they pass**

Run (depuis `web/app`) : `npx vitest run`
Expected: PASS, tous les fichiers de test du répertoire.

Run: `wsl.exe -- bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/pochettes-album && cargo test -p ritornello-core'`
Expected: PASS — c'est ce qui prouve la parité des catalogues.

- [ ] **Step 5: Commit**

```bash
git add web/app/src crates/ritornello-core/src/locales/en.toml deploy/locales/core/fr.toml
git commit -m "feat(web,i18n): la pochette dans la carte Player, place reservee d avance"
```

---

### Task 12: La documentation

**Files:**
- Modify: `docs/plugins.md` (section « Now-playing metadata », vers la ligne 574 ; « Where it shows up », vers la ligne 753 ; « Writing a `metadata` plugin », vers la ligne 788)
- Modify: `docs/interface.md` (la forme de la charge utile poussée)

- [ ] **Step 1: Rewrite the layers section**

Dans `docs/plugins.md`, la description des « three layers stack up, and the later one wins » ne décrit plus le mécanisme. La remplacer par le modèle en étages, en anglais comme le reste du fichier, et couvrir :

- ce que `NowPlaying.known` transporte, et pourquoi un contributeur en a besoin (compléter, ou s'abstenir) ;
- `fill_only`, son défaut « écrase », et le fait que ce défaut préserve exactement la règle précédente — un greffon prime sur l'ICY et sur les tags en toutes circonstances ;
- que le premier contributeur qui écrase fournit son **bloc**, trous compris, et que les `fill_only` ne comblent que les vides — avec la raison : composer champ par champ entre deux contributeurs qui écrasent afficherait un morceau qui n'existe pas ;
- que l'ordre de `plugins.toml` ne départage plus que des pairs ;
- qu'une Source déclare ses métadonnées sur son propre canal, sans devenir un greffon `metadata`.

- [ ] **Step 2: Document the cover chain**

Ajouter une sous-section sur les pochettes : les cinq contributeurs, la route `/api/cover/{clé}`, le fait que l'appareil va chercher l'image et que la page ne charge rien d'externe, le cache en mémoire qui ne survit pas au redémarrage, et le tableau de la spec (ce qu'il y a → ce qui s'affiche → qui).

Mentionner explicitement les deux règles locales qui ne se devinent pas :

- `radiofrance-metas` se tait sur un `songUuid` nul, parce que la station sert une image générique pour « Le direct » ;
- `files` écarte une image unique nommée comme un dos, parce que sinon on afficherait le verso du boîtier.

- [ ] **Step 3: Update the pushed payload doc**

Dans `docs/interface.md`, ajouter `cover_href` et `cover_origin` à la description de la charge utile, en précisant que `cover_href` est **toujours** une URL locale de l'appareil.

- [ ] **Step 4: Verify nothing contradicts**

Relire la section « Now-playing metadata » en entier et vérifier qu'aucune phrase ne décrit encore l'ancien modèle — en particulier les formulations « the later one wins » et « A plugin takes precedence … under all circumstances », qui doivent devenir des conséquences du défaut de `fill_only` plutôt que des règles en soi.

- [ ] **Step 5: Commit**

```bash
git add docs/plugins.md docs/interface.md
git commit -m "docs(plugins): protocole en etages, intentions et chaine des pochettes"
```

---

## Self-review du plan

**Couverture de la spec**, section par section :

| Section de la spec | Tâche |
|---|---|
| `NowPlaying` porte l'état partiel (déc. 1, 2, 3) | 1 |
| Intention déclarée, `fill_only` et son défaut (déc. 4) | 1, 4 |
| L'ordre ne départage que des pairs (déc. 5) | 4 |
| Aucun genre ajouté, deux intentions dans un binaire (déc. 6) | 10 |
| Une Source déclare sur son canal (déc. 7) | 2, 7 |
| L'appareil va chercher l'image (déc. 8) | 3 |
| Deux champs pour deux rôles (déc. 9) | 1, 4 |
| `CoverRef` à deux formes (déc. 10) | 1 |
| `files` le répertoire, le cœur l'embarquée (déc. 11) | 6, 7 |
| Pas de placeholder (déc. 12) | 8 |
| Artiste **et** album, jamais l'ICY (déc. 13) | 10 |
| Cache mémoire borné (déc. 14) | 3 |
| Publier seulement les octets en main (déc. 15) | 4, 5 |
| Route, clé, plafonds, annulation | 3, 5 |
| Rendu dans la carte Player | 11 |
| Documentation des deux règles locales | 12 |

**Points où le plan va au-delà de la spec, et pourquoi :**

- **Task 4** tranche une question que la spec laissait implicite : le premier contributeur qui écrase fournit son bloc, les `fill_only` comblent. La spec dit « chaque étage voit ce que les précédents ont rempli » sans dire si deux contributeurs qui écrasent se composent. Le choix retenu est le plus conservateur — il laisse le texte des trois greffons livrés inchangé — et il est testé explicitement (`deux_contributeurs_qui_ecrasent_ne_sont_pas_melanges`).
- **Task 6** écrit la pochette embarquée dans un fichier temporaire au lieu de la garder en octets. La spec ne le disait pas ; cela garde **une seule** nature de pochette locale, donc rien en RAM, ce qui est l'esprit de sa décision 14.

**Ce qui reste incertain à la compilation, signalé aux implémenteurs dans les tâches concernées :** les noms exacts de l'API `lofty` 0.25 (Task 6), le nom réel de la fixture de lookup MusicBrainz (Task 10), et le nom du constructeur d'`AppState` de test (Task 3).
