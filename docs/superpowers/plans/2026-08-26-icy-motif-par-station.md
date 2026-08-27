# Découpage ICY par motif appris — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** le greffon `musicbrainz` découpe la chaîne ICY d'une radio en artiste + titre, en apprenant le motif de chaque station et en le vérifiant contre MusicBrainz, avec une page d'admin pour inspecter et corriger.

**Architecture :** un champ additif dans le protocole porte la chaîne brute du flux ; le greffon dérive des candidats de découpage de cette chaîne, les valide par une recherche d'enregistrement, retient le meilleur par URL de flux dans un fichier d'état, puis applique ce motif localement à chaque morceau — la requête de pochette servant de validation continue.

**Tech Stack :** Rust (tokio, serde, reqwest), Vue 3 + Vite pour la page d'admin, `ritornello-i18n` pour les catalogues.

**Spec :** `docs/superpowers/specs/2026-08-26-icy-motif-par-station-design.md` — à lire en entier avant la tâche 1. Elle porte les raisons ; ce plan ne porte que les gestes.

## Global Constraints

- **Commentaires de code en français.** Les fichiers `.md` de `docs/` sont en anglais. Le **protocole** (noms de champs) et les **journaux** sont en anglais, sans exception.
- **`cargo` n'existe que sous WSL**, et toujours avec `--offline` : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo …"`. Sans `--offline`, cargo peut pendre en silence.
- **Clippy par crate**, jamais `--workspace -D warnings` : celui-ci cesse d'ordonnancer au premier crate en échec, et `plugin-files` a trois erreurs de longue date dans `scan.rs`. Utiliser `cargo clippy -p <crate> --offline --all-targets -- -D warnings`.
- **Itérer avec `cargo test -p <crate>`, finir avec `--workspace`** : `-p` ne compile pas les littéraux de structures d'ailleurs, donc un champ ajouté à un type public ne révèle ses casses qu'en `--workspace`.
- **Commiter avant de muter.** Une preuve par mutation se restaure par `git checkout -- <fichier>`, qui restaure depuis `HEAD` : sur un fichier non commité, elle efface tout le travail.
- **Prouver par mutation les deux ou trois propriétés nommées** dans chaque tâche, pas chaque test, et sur le seul crate concerné.
- **Une fixture doit être une réponse que le service peut vraiment émettre.** Une preuve bâtie sur un `Default::default()` ou un JSON inventé ne prouve rien.
- Seuils, une seule définition chacun : `SEUIL_RELEASE = 85`, `SEUIL_RECORDING = 90`, `MAX_CANDIDATS = 4`, `ECHECS_AVANT_RESONDAGE = 3`, `INTERVALLE_MIN = 1100 ms`.
- Chemin d'état : variable `RITORNELLO_MUSICBRAINZ_STATE`, défaut `/var/lib/ritornello/plugin-musicbrainz.json`.
- **Aucun `unwrap()` / `expect()` sur une entrée externe** (JSON d'un tiers, fichier d'état, corps d'une requête d'admin). Sur une valeur construite dans la fonction même, c'est permis.

---

### Task 1 : le client apprend à dire non, et à ne pas mitrailler

Deux défauts du client actuel, indépendants de la fonctionnalité neuve, et qui la rendraient sans valeur : il croit le premier résultat de recherche, et il n'espace pas ses requêtes.

**Files:**
- Modify: `crates/ritornello-plugin-musicbrainz/src/musicbrainz.rs`

**Interfaces:**
- Consumes: rien.
- Produces: `SEUIL_RELEASE: u64`, `Etrangleur` (`new`, `attend`), `etrangleur() -> &'static Etrangleur`, et `premier_release_id` au comportement changé (même signature).

- [ ] **Step 1 : écrire les tests du seuil**

Dans le `mod tests` existant de `musicbrainz.rs` :

```rust
/// Réponse de recherche de release **telle que MusicBrainz l'émet** : le
/// champ `score` est toujours présent, et c'est lui qu'on ignorait.
fn reponse_release(score: u64) -> String {
    format!(
        r#"{{"created":"2026-08-26T12:00:00.000Z","count":1,"offset":0,
            "releases":[{{"id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","score":{score},
            "title":"Kind of Blue","status":"Official"}}]}}"#
    )
}

#[test]
fn une_release_assez_sure_est_retenue() {
    assert_eq!(
        premier_release_id(&reponse_release(SEUIL_RELEASE)).as_deref(),
        Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
        "le seuil pile doit passer"
    );
}

#[test]
fn une_release_trop_incertaine_est_refusee() {
    // Le defaut latent : aujourd'hui un album mal orthographie recoit une
    // pochette fausse avec aplomb, parce que la recherche rend toujours
    // quelque chose de plausible.
    assert_eq!(premier_release_id(&reponse_release(SEUIL_RELEASE - 1)), None);
}

#[test]
fn un_score_absent_est_refuse_et_non_suppose_bon() {
    // Un score manquant veut dire « je ne sais pas ». Le supposer bon
    // reviendrait au defaut d'avant, en silence ; le supposer mauvais coupe
    // la fonctionnalite, mais visiblement (voir le `warn`).
    let sans = r#"{"releases":[{"id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","title":"X"}]}"#;
    assert_eq!(premier_release_id(sans), None);
}

#[test]
fn une_reponse_sans_release_reste_none() {
    assert_eq!(premier_release_id(r#"{"releases":[]}"#), None);
    assert_eq!(premier_release_id("pas du json"), None);
}
```

- [ ] **Step 2 : lancer, vérifier l'échec**

`cargo test -p ritornello-plugin-musicbrainz --offline seuil` puis `... refusee`.
Attendu : `une_release_trop_incertaine_est_refusee` et `un_score_absent_est_refuse…` échouent (la fonction rend `Some`), les deux autres passent.

- [ ] **Step 3 : implémenter le seuil**

Remplacer `premier_release_id` :

```rust
/// Score minimal d'une recherche de release pour être crue.
///
/// La recherche MusicBrainz rend presque toujours **quelque chose** de
/// plausible : sans seuil, `premier_release_id` croyait le premier résultat
/// quel qu'il soit, et un album mal orthographié dans les étiquettes d'un
/// fichier recevait une pochette fausse avec aplomb. 85 plutôt que 90 pour la
/// release, parce que la requête contraint deux champs (artiste et album) dont
/// l'un vient d'étiquettes arbitraires : un peu plus de tolérance qu'un titre
/// d'enregistrement, que la station écrit d'une seule main.
pub const SEUIL_RELEASE: u64 = 85;

/// MBID du premier résultat, **s'il est assez sûr**. `None` = rien trouvé,
/// réponse illisible, ou meilleur résultat trop incertain.
pub fn premier_release_id(json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    let premiere = v.get("releases")?.as_array()?.first()?;
    // Score absent = refus, et un `warn` plutôt qu'un `debug` : c'est un champ
    // que l'API rend toujours, donc son absence est un changement de schéma.
    // Refuser garde la correction (pas de pochette fausse) et le niveau de
    // journal rend la panne diagnosticable, là où supposer « assez sûr »
    // restaurerait le défaut sans une ligne.
    let Some(score) = premiere.get("score").and_then(Value::as_u64) else {
        tracing::warn!("release search: no score field, refusing rather than guessing");
        return None;
    };
    if score < SEUIL_RELEASE {
        tracing::debug!("release search: best match scored {score}, under the {SEUIL_RELEASE} needed");
        return None;
    }
    premiere.get("id")?.as_str().map(str::to_string)
}
```

- [ ] **Step 4 : les tests passent**

`cargo test -p ritornello-plugin-musicbrainz --offline` : tout vert.

- [ ] **Step 5 : écrire le test de l'étrangleur**

```rust
#[tokio::test(start_paused = true)]
async fn letrangleur_espace_deux_requetes_consecutives() {
    // Horloge virtuelle : `sleep` avance le temps sans attendre, donc ce test
    // dure une microseconde tout en éprouvant un intervalle de 1,1 s.
    // L'étrangleur est **construit ici** et non pris d'un statique : deux
    // tests qui partageraient l'instance se pollueraient l'un l'autre.
    let e = Etrangleur::new();
    let depart = tokio::time::Instant::now();
    e.attend().await;
    assert_eq!(depart.elapsed(), std::time::Duration::ZERO, "la premiere ne doit pas attendre");
    e.attend().await;
    assert!(
        depart.elapsed() >= INTERVALLE_MIN,
        "la seconde doit etre espacee de {INTERVALLE_MIN:?}, mesure {:?}",
        depart.elapsed()
    );
}
```

- [ ] **Step 6 : implémenter l'étrangleur**

```rust
/// Intervalle minimal entre deux requêtes vers MusicBrainz.
///
/// Le service demande une requête par seconde et par client, et ne l'applique
/// pas mollement. 1100 ms plutôt que 1000 pour ne pas jouer sur la borne : la
/// marge coûte cent millisecondes sur des tâches détachées que personne
/// n'attend.
pub const INTERVALLE_MIN: std::time::Duration = std::time::Duration::from_millis(1100);

/// Sérialise les requêtes et espace la suivante d'`INTERVALLE_MIN`.
///
/// Le verrou est **tenu pendant l'attente**, et c'est le mécanisme même : deux
/// tâches détachées parties en même temps se retrouvent en file au lieu de
/// mitrailler. Sans lui, le sondage de quatre candidats émettait quatre
/// requêtes dans la même milliseconde, ce que MusicBrainz refuse par des 503 —
/// donc un sondage qui échouait pour une raison qui n'a rien à voir avec le
/// découpage.
///
/// Une structure plutôt qu'un statique nu : c'est ce qui permet à un test
/// d'avoir sa propre instance. Le statique est la couche d'à côté.
pub struct Etrangleur(tokio::sync::Mutex<Option<tokio::time::Instant>>);

impl Etrangleur {
    pub fn new() -> Self {
        Self(tokio::sync::Mutex::new(None))
    }

    pub async fn attend(&self) {
        let mut garde = self.0.lock().await;
        if let Some(precedente) = *garde {
            let ecoule = precedente.elapsed();
            if ecoule < INTERVALLE_MIN {
                tokio::time::sleep(INTERVALLE_MIN - ecoule).await;
            }
        }
        *garde = Some(tokio::time::Instant::now());
    }
}

/// L'étrangleur du processus. Tous les chemins du greffon passent par lui —
/// disque, release, enregistrement — parce que le débit est compté par client
/// et non par fonctionnalité.
fn etrangleur() -> &'static Etrangleur {
    static E: std::sync::OnceLock<Etrangleur> = std::sync::OnceLock::new();
    E.get_or_init(Etrangleur::new)
}
```

Puis, **première ligne** du corps de `requete_texte` :

```rust
async fn requete_texte(url: &str) -> Result<Option<String>> {
    etrangleur().attend().await;
    // … suite inchangée …
```

- [ ] **Step 7 : preuve par mutation (deux propriétés)**

Commiter d'abord. Puis :
1. Retirer le `if score < SEUIL_RELEASE { return None }` → `une_release_trop_incertaine_est_refusee` doit tomber, et lui seul.
2. Retirer le `tokio::time::sleep` de `attend` → `letrangleur_espace_deux_requetes_consecutives` doit tomber.
Restaurer par `git checkout -- crates/ritornello-plugin-musicbrainz/src/musicbrainz.rs` après **chaque** mutation, et vérifier `grep -rn "// mutation" crates/` avant de conclure.

- [ ] **Step 8 : clippy et commit**

```bash
cargo clippy -p ritornello-plugin-musicbrainz --offline --all-targets -- -D warnings
git commit -am "fix(musicbrainz): le client sait dire non, et espace ses requetes"
```

---

### Task 2 : le protocole porte la chaîne annoncée par le flux

**Files:**
- Modify: `crates/ritornello-proto/src/metadata.rs` (struct `Known`)
- Modify: `crates/ritornello-core/src/metadata.rs` (fonction `known`)
- Test: dans les `mod tests` de ces deux fichiers

**Interfaces:**
- Produces: `ritornello_proto::Known::stream_title: Option<String>`.

- [ ] **Step 1 : le test du protocole**

Dans `crates/ritornello-proto/src/metadata.rs`, `mod tests` :

```rust
#[test]
fn stream_title_absent_ne_grossit_pas_la_trame() {
    // Même contrat que `covers` et `known` : un champ neuf ne doit rien
    // changer à la trame la plus courante, sinon chaque trame par seconde de
    // lecture paie l'ajout.
    let json = serde_json::to_string(&Known::default()).unwrap();
    assert!(!json.contains("stream_title"), "{json}");
}

#[test]
fn stream_title_voyage_quand_il_est_la() {
    let k = Known { stream_title: Some("Miles Davis - So What".into()), ..Default::default() };
    let json = serde_json::to_string(&k).unwrap();
    assert!(json.contains(r#""stream_title":"Miles Davis - So What""#), "{json}");
    assert_eq!(serde_json::from_str::<Known>(&json).unwrap(), k);
}

#[test]
fn une_trame_dun_binaire_anterieur_se_relit() {
    let k: Known = serde_json::from_str(r#"{"title":"X"}"#).unwrap();
    assert_eq!(k.stream_title, None);
}
```

- [ ] **Step 2 : le test du cœur — la propriété qui rend tout possible**

Dans `crates/ritornello-core/src/metadata.rs`, `mod tests`. Le test doit se
construire **comme la production** : poser un ICY, poser un enrichissement qui
écrase, puis poser un **nouvel** ICY sur la **même identité**.

```rust
#[test]
fn la_chaine_brute_survit_a_lenrichissement_qui_lecrase() {
    // La propriété dont dépend toute la fonctionnalité. L'identité d'une radio
    // est l'URL du flux : elle ne change pas entre deux morceaux, et `set_icy`
    // n'efface délibérément pas les enrichissements. Donc sans ce champ, un
    // greffon qui a une fois écrit un artiste ne reverrait plus jamais la
    // chaîne ICY, et ne pourrait plus rien découper — « ça marche une fois ».
    let mut m = Metadonnees::new(vec!["musicbrainz".to_string()]);
    let identite = serde_json::json!({ "kind": "stream", "url": "http://exemple/flux.mp3" });
    m.set_identity(Some(identite.clone()));
    assert!(m.set_icy("Miles Davis - So What".into()));

    // Le greffon corrige, en écrasant : le titre composé devient le sien.
    assert!(m.ajoute(
        "musicbrainz",
        ritornello_proto::Enrichment {
            identity: identite.clone(),
            artist: Some("Miles Davis".into()),
            title: Some("So What".into()),
            ..Default::default()
        }
    ));
    assert_eq!(m.known().title.as_deref(), Some("So What"));

    // Morceau suivant, même station : l'enrichissement précédent est toujours
    // là (identité inchangée), mais la chaîne brute doit être la neuve.
    assert!(m.set_icy("John Coltrane - Naima".into()));
    assert_eq!(
        m.known().stream_title.as_deref(),
        Some("John Coltrane - Naima"),
        "le brut doit suivre le flux, pas la composition"
    );
}

#[test]
fn sans_icy_le_champ_reste_vide() {
    let m = Metadonnees::new(vec![]);
    assert_eq!(m.known().stream_title, None);
}
```

**Note à l'implémenteur :** vérifier le nom réel de la méthode qui pose
l'identité (`set_identity` ci-dessus est un pari) en lisant le `mod tests`
existant du fichier, et reprendre l'idiome des tests voisins.

- [ ] **Step 3 : lancer, vérifier l'échec**

`cargo test -p ritornello-proto --offline stream_title` : ne compile pas (champ inconnu). C'est l'échec attendu.

- [ ] **Step 4 : ajouter le champ au protocole**

Dans `crates/ritornello-proto/src/metadata.rs`, à la fin de `struct Known` :

```rust
    /// Ce que le **flux lui-même** a annoncé, brut : ni découpé, ni composé,
    /// ni arbitré.
    ///
    /// Pas une redite de `title`. `title` est le résultat d'un arbitrage entre
    /// plusieurs contributeurs et peut donc venir d'un greffon ; ce champ est
    /// un fait d'un seul émetteur, la station.
    ///
    /// Il existe parce que seule la forme brute peut être **redécoupée**, et
    /// qu'un greffon a besoin de la revoir même après avoir lui-même écrasé le
    /// titre composé. L'identité d'une radio est l'URL de son flux, donc elle
    /// ne change pas d'un morceau à l'autre : le garde-fou de péremption de
    /// `Metadonnees::ajoute` ne périme rien, et `set_icy` n'efface pas les
    /// enrichissements. Sans ce champ, un greffon qui corrige une fois ne
    /// reverrait plus jamais ce que la station annonce.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_title: Option<String>,
```

- [ ] **Step 5 : le cœur le remplit**

Dans `crates/ritornello-core/src/metadata.rs`, méthode `known()` :

```rust
        ritornello_proto::Known {
            artist: m.artist,
            title: m.title,
            album: m.album,
            duration_s: m.duration_s,
            cover: self.cover_retenue().is_some(),
            // Verbatim, et depuis `self.icy` et non depuis `m` : `m` est le
            // texte **composé**, où l'ICY n'apparaît qu'en dernier recours.
            stream_title: self.icy.clone(),
        }
```

- [ ] **Step 6 : les tests passent, et le workspace compile**

`cargo test -p ritornello-proto --offline` puis `cargo test -p ritornello-core --offline`, puis **`cargo test --workspace --offline`** : `Known` est une structure publique, et `-p` ne voit pas les littéraux d'ailleurs. Corriger les littéraux cassés en ajoutant `stream_title: None` (ou une valeur non-défaut quand le test voisin explique qu'il en veut une — lire le commentaire de la fixture avant de choisir).

- [ ] **Step 7 : preuve par mutation (une propriété)**

Commiter, puis remplacer `stream_title: self.icy.clone()` par `stream_title: m.title.clone()` — la confusion la plus probable. `la_chaine_brute_survit_a_lenrichissement_qui_lecrase` doit tomber. Restaurer.

- [ ] **Step 8 : clippy et commit**

```bash
cargo clippy -p ritornello-proto --offline --all-targets -- -D warnings
cargo clippy -p ritornello-core --offline --all-targets -- -D warnings
git commit -am "feat(proto,core): la trame porte la chaine annoncee par le flux"
```

---

### Task 3 : nettoyer, puis dériver les candidats

Deux fonctions pures, dans un module neuf du greffon. C'est le seul endroit où
le découpage est décidé, et il ne touche ni au réseau ni à l'état.

**Files:**
- Create: `crates/ritornello-plugin-musicbrainz/src/icy.rs`
- Modify: `crates/ritornello-plugin-musicbrainz/src/main.rs` (`mod icy;`)

**Interfaces:**
- Produces: `icy::nettoie(&str) -> String`, `icy::Candidat { artiste, titre, separateur, artiste_en_premier }`, `icy::candidats(&str) -> Vec<Candidat>`, `icy::SEPARATEURS`, `icy::MAX_CANDIDATS`, `icy::applique(&Motif, &str) -> Option<(String, String)>` (ajoutée en tâche 5, quand `Motif` existe — **ne pas** l'écrire ici).

- [ ] **Step 1 : les tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_nettoyage_retire_la_reclame_apres_une_barre() {
        assert_eq!(nettoie("Miles Davis - So What | Radio X"), "Miles Davis - So What");
        assert_eq!(nettoie("  Miles Davis - So What  "), "Miles Davis - So What");
    }

    #[test]
    fn le_nettoyage_retire_une_duree_entre_crochets_en_fin() {
        assert_eq!(nettoie("Miles Davis - So What [00:09:22]"), "Miles Davis - So What");
    }

    #[test]
    fn le_nettoyage_garde_une_parenthese_qui_fait_partie_du_titre() {
        // `(Radio Edit)`, `(Live)`, `(feat. X)` appartiennent au titre. Les
        // retirer casserait la validation au lieu de l'aider.
        let s = "Daft Punk - Around the World (Radio Edit)";
        assert_eq!(nettoie(s), s);
    }

    #[test]
    fn le_nettoyage_precede_le_decoupage_donc_la_reclame_ne_casse_pas_les_candidats() {
        // La régression que cette étape existe pour empêcher : sans nettoyage,
        // le titre du dernier candidat porte « | Radio X », aucun candidat ne
        // valide, et la station est classée « ne pas découper » — c'est-à-dire
        // définitivement, puisque rien ne resonde une station ainsi classée.
        let c = candidats(&nettoie("Miles Davis - So What | Radio X"));
        assert!(
            c.iter().any(|c| c.artiste == "Miles Davis" && c.titre == "So What"),
            "candidats obtenus : {c:?}"
        );
    }

    #[test]
    fn deux_candidats_pour_un_seul_separateur_les_deux_ordres() {
        let c = candidats("Miles Davis - So What");
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].artiste.as_str(), c[0].titre.as_str()), ("Miles Davis", "So What"));
        assert!(c[0].artiste_en_premier, "le standard passe en premier");
        assert_eq!((c[1].artiste.as_str(), c[1].titre.as_str()), ("So What", "Miles Davis"));
        assert!(!c[1].artiste_en_premier);
    }

    #[test]
    fn le_demi_cadratin_est_reconnu_comme_separateur() {
        let c = candidats("Miles Davis – So What");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].separateur, " – ");
    }

    #[test]
    fn trois_champs_donnent_aussi_le_candidat_du_milieu() {
        // La forme `Artiste - Titre - Album`, réelle. Sans ce candidat, le
        // titre porterait « So What - Kind of Blue » et ne validerait jamais.
        let c = candidats("Miles Davis - So What - Kind of Blue");
        assert!(
            c.iter().any(|c| c.artiste == "Miles Davis" && c.titre == "So What"),
            "candidats obtenus : {c:?}"
        );
    }

    #[test]
    fn le_plafond_de_candidats_est_tenu() {
        // Plusieurs types de séparateurs dans la même chaîne : le plafond doit
        // mordre, sinon un sondage part en dix requêtes.
        let c = candidats("A - B / C : D – E");
        assert!(c.len() <= MAX_CANDIDATS, "{} candidats", c.len());
    }

    #[test]
    fn sans_separateur_il_ny_a_aucun_candidat() {
        // Un slogan, un nom d'émission : rien à contraindre côté artiste, donc
        // rien de validable. L'appelant en conclut « ne pas découper ».
        assert!(candidats("Vous ecoutez Radio X").is_empty());
        assert!(candidats("").is_empty());
    }

    #[test]
    fn une_moitie_vide_ne_produit_pas_de_candidat() {
        // « - So What » ou « Miles Davis - » : une requête avec un champ vide
        // est une requête pour rien.
        assert!(candidats("- So What").is_empty());
        assert!(candidats("Miles Davis -").is_empty());
    }
}
```

- [ ] **Step 2 : lancer, vérifier l'échec**

`cargo test -p ritornello-plugin-musicbrainz --offline icy::` : ne compile pas. Attendu.

- [ ] **Step 3 : implémenter**

```rust
//! Le découpage d'une chaîne ICY : deux fonctions pures, aucun réseau, aucun
//! état. C'est le seul endroit où l'on décide **comment** couper ; la décision
//! de savoir *quel* découpage est le bon appartient à la validation.

/// Séparateurs reconnus, par ordre de priorité — c'est aussi l'ordre dans
/// lequel les candidats sont sondés.
///
/// `" - "` d'abord : c'est la convention de fait du champ `StreamTitle`, le
/// défaut de la plupart des automates de diffusion. Les espaces autour font
/// partie du motif, et ce n'est pas un détail : sans eux, `Jean-Michel Jarre`
/// se ferait couper en deux.
pub const SEPARATEURS: [&str; 5] = [" - ", " – ", " — ", " / ", " : "];

/// Plafond de candidats sondés pour une station.
///
/// Chaque candidat coûte une requête, espacée d'`INTERVALLE_MIN` : quatre font
/// un sondage de quatre secondes, une fois par station, que personne n'attend.
/// Sans plafond, une chaîne portant plusieurs types de séparateurs en
/// produirait dix.
pub const MAX_CANDIDATS: usize = 4;

/// Un découpage possible, et de quoi le rejouer plus tard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidat {
    pub artiste: String,
    pub titre: String,
    pub separateur: &'static str,
    pub artiste_en_premier: bool,
}

/// Retire le bruit qu'une station accole à ce qu'elle annonce.
///
/// **Avant** tout découpage, et c'est l'ordre qui compte : une station qui
/// accole sa réclame ferait échouer *tous* les candidats, donc serait classée
/// « ne pas découper » — c'est-à-dire définitivement, puisque rien ne resonde
/// une station ainsi classée.
///
/// Délibérément **conservateur**. Trois formes seulement, celles qui ne peuvent
/// pas appartenir à un titre :
///
/// * tout ce qui suit une barre verticale — elle n'apparaît pas dans un titre ;
/// * un groupe entre crochets en fin de chaîne (durées, marqueurs de régie) ;
/// * les espaces de bord et les espaces répétés.
///
/// Ce qu'on ne fait **pas**, et pourquoi : retirer un suffixe du genre
/// `" - Radio X"` serait indistinguable d'un vrai séparateur, donc casserait
/// autant de stations qu'il en réparerait. Et les parenthèses restent : `(Radio
/// Edit)`, `(Live)`, `(feat. …)` font partie du titre, et les retirer
/// empêcherait la validation au lieu de l'aider. Une station que ce nettoyage
/// ne suffit pas à traiter finira en « ne pas découper », et la page d'admin
/// est le remède prévu pour elle.
pub fn nettoie(brut: &str) -> String {
    let sans_barre = brut.split('|').next().unwrap_or(brut);
    let mut s = sans_barre.trim();
    // Un seul groupe retiré, en fin de chaîne : boucler couperait un titre qui
    // finirait légitimement par des crochets.
    if let Some(ouvrant) = s.rfind('[') {
        if s.trim_end().ends_with(']') {
            s = s[..ouvrant].trim_end();
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Dérive les découpages plausibles de la chaîne **nettoyée**.
///
/// Les candidats se dérivent de la chaîne et non d'une liste fixe : on ne
/// construit que pour les séparateurs réellement présents. Une chaîne n'en
/// contient qu'un type en pratique, donc deux candidats — les deux ordres —, et
/// le plafond ne mord que sur les chaînes bavardes.
///
/// Pour un séparateur présent au moins deux fois — la forme
/// `Artiste - Titre - Album`, réelle — un troisième candidat prend le champ du
/// **milieu** comme titre. Sans lui, le titre porterait l'album collé et ne
/// validerait jamais.
///
/// Une moitié vide ne produit pas de candidat : une requête avec un champ vide
/// est une requête pour rien.
pub fn candidats(nettoye: &str) -> Vec<Candidat> {
    let mut out: Vec<Candidat> = Vec::new();
    for separateur in SEPARATEURS {
        let parts: Vec<&str> = nettoye.split(separateur).map(str::trim).collect();
        if parts.len() < 2 {
            continue;
        }
        let tete = parts[0];
        let reste = parts[1..].join(separateur);
        let mut pousse = |artiste: &str, titre: &str, artiste_en_premier: bool| {
            if artiste.is_empty() || titre.is_empty() || out.len() >= MAX_CANDIDATS {
                return;
            }
            out.push(Candidat {
                artiste: artiste.to_string(),
                titre: titre.to_string(),
                separateur,
                artiste_en_premier,
            });
        };
        pousse(tete, &reste, true);
        pousse(&reste, tete, false);
        if parts.len() >= 3 {
            pousse(tete, parts[1], true);
        }
    }
    out
}
```

Ajouter `mod icy;` dans `main.rs`, à côté de `mod musicbrainz;`.

- [ ] **Step 4 : les tests passent**

`cargo test -p ritornello-plugin-musicbrainz --offline`.

**Attention :** le test `le_plafond_de_candidats_est_tenu` peut demander un
ajustement de l'ordre des `pousse` si le plafond coupe avant le candidat du
milieu. Si un test ne passe pas, **ne pas relâcher l'assertion** : corriger le
code, ou justifier par écrit dans le rapport pourquoi l'assertion était fausse.

- [ ] **Step 5 : preuve par mutation (deux propriétés)**

Commiter, puis :
1. Retirer l'appel à `nettoie` dans `le_nettoyage_precede_le_decoupage…` (le remplacer par la chaîne brute) → le test doit tomber. C'est la preuve que l'ordre est porteur.
2. Retirer la garde `artiste.is_empty() || titre.is_empty()` → `une_moitie_vide_ne_produit_pas_de_candidat` doit tomber.

- [ ] **Step 6 : clippy et commit**

```bash
cargo clippy -p ritornello-plugin-musicbrainz --offline --all-targets -- -D warnings
git commit -am "feat(musicbrainz): nettoyage et candidats de decoupage d'une chaine ICY"
```

---

### Task 4 : la recherche d'enregistrement, et la validation

**Files:**
- Modify: `crates/ritornello-plugin-musicbrainz/src/musicbrainz.rs`

**Interfaces:**
- Consumes: `echappe_lucene`, `pourcent_encode`, `requete_texte`, `etrangleur` (tâche 1).
- Produces: `SEUIL_RECORDING`, `Enregistrement { score, titre, release_id }`, `requete_recording(&str, &str) -> String`, `premier_enregistrement(&str) -> Option<Enregistrement>`, `normalise(&str) -> String`, `cherche_enregistrement(&str, &str) -> Result<Option<Enregistrement>>`.

- [ ] **Step 1 : les tests**

```rust
/// Réponse de recherche d'enregistrement **telle que MusicBrainz l'émet** :
/// `score`, `title`, et les releases dont sortira la pochette.
fn reponse_recording(score: u64, titre: &str, avec_release: bool) -> String {
    let releases = if avec_release {
        r#","releases":[{"id":"11111111-2222-3333-4444-555555555555","title":"Kind of Blue"}]"#
    } else {
        ""
    };
    format!(
        r#"{{"created":"2026-08-26T12:00:00.000Z","count":1,"offset":0,
            "recordings":[{{"id":"99999999-8888-7777-6666-555555555555","score":{score},
            "title":"{titre}","length":545000{releases}}}]}}"#
    )
}

#[test]
fn la_requete_dun_enregistrement_echappe_les_deux_langages() {
    // Lucene à l'intérieur des guillemets, puis l'URL par-dessus : la même
    // exigence que `requete_release`, pour la même raison — ces valeurs
    // viennent d'une station, donc d'une entrée qu'on ne choisit pas.
    let url = requete_recording(r#"AC"DC"#, "Back in Black & Co");
    assert!(url.starts_with("https://musicbrainz.org/ws/2/recording/?query="), "{url}");
    assert!(!url.contains('&') || url.matches('&').count() == 1, "un seul & de parametre : {url}");
    assert!(url.contains("%5C%22"), "le guillemet doit etre echappe deux fois : {url}");
}

#[test]
fn un_enregistrement_est_lu_avec_son_score_et_sa_release() {
    let e = premier_enregistrement(&reponse_recording(100, "So What", true)).unwrap();
    assert_eq!(e.score, 100);
    assert_eq!(e.titre, "So What");
    assert_eq!(e.release_id.as_deref(), Some("11111111-2222-3333-4444-555555555555"));
}

#[test]
fn un_enregistrement_sans_release_reste_exploitable() {
    // Le découpage est acquis même sans image : le couple artiste/titre vaut
    // par lui-même, et le cœur traite déjà une pochette absente en silence.
    let e = premier_enregistrement(&reponse_recording(100, "So What", false)).unwrap();
    assert_eq!(e.release_id, None);
    assert_eq!(e.titre, "So What");
}

#[test]
fn une_reponse_illisible_ou_vide_rend_none() {
    assert!(premier_enregistrement(r#"{"recordings":[]}"#).is_none());
    assert!(premier_enregistrement("pas du json").is_none());
    // Score absent : refus, comme pour la release.
    assert!(premier_enregistrement(r#"{"recordings":[{"id":"x","title":"y"}]}"#).is_none());
}

#[test]
fn la_normalisation_rend_comparables_deux_ecritures_du_meme_titre() {
    assert_eq!(normalise("So What"), normalise("so  what"));
    assert_eq!(normalise("Où es-tu ?"), normalise("ou es tu"));
    assert_eq!(normalise("Café/Crème"), normalise("cafe creme"));
}

#[test]
fn la_normalisation_ne_confond_pas_deux_titres_differents() {
    // Le contrôle : une normalisation trop agressive accepterait n'importe
    // quoi, et la validation ne validerait plus rien.
    assert_ne!(normalise("So What"), normalise("So What Else"));
    assert_ne!(normalise("Naima"), normalise("Nauma"));
}
```

- [ ] **Step 2 : lancer, vérifier l'échec** — ne compile pas.

- [ ] **Step 3 : implémenter**

```rust
/// Score minimal d'une recherche d'enregistrement pour être crue.
///
/// Plus haut que `SEUIL_RELEASE` : ici les deux champs contraints viennent de
/// la **même** chaîne écrite d'une seule main par la station, donc un vrai
/// couple obtient un score franc. Et la validation sert à *choisir* entre deux
/// découpages : plus le seuil est haut, moins l'ordre inverse a de chances de
/// se glisser au-dessus.
pub const SEUIL_RECORDING: u64 = 90;

/// Ce qu'un enregistrement rendu par la recherche apprend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enregistrement {
    pub score: u64,
    /// Le titre **tel que MusicBrainz l'écrit**. C'est lui qu'on compare au
    /// candidat après normalisation, et cette comparaison porte la validation :
    /// le score seul est trop généreux.
    pub titre: String,
    /// Première release, s'il en a une. La pochette en vient.
    ///
    /// Pas de choix « intelligent » entre original, compilation et remaster :
    /// MusicBrainz ne les classe pas par pertinence, et ce serait une
    /// heuristique de plus pour un carré de 500 pixels.
    pub release_id: Option<String>,
}

/// Requête de recherche d'un enregistrement par artiste et titre.
///
/// Les deux valeurs viennent d'une **station**, donc d'une entrée qu'on ne
/// choisit pas : échappées pour les deux langages superposés qu'elles
/// traversent, Lucene puis l'URL. Voir la doc de `requete_release`, qui écrit
/// ce qu'une version antérieure y avait manqué.
pub fn requete_recording(artist: &str, title: &str) -> String {
    let echappe = |s: &str| pourcent_encode(&echappe_lucene(s));
    format!(
        "https://musicbrainz.org/ws/2/recording/?query=artist:%22{}%22%20AND%20recording:%22{}%22&fmt=json&limit=1",
        echappe(artist),
        echappe(title)
    )
}

/// Premier enregistrement de la réponse. `None` = rien, illisible, ou sans
/// score — voir `premier_release_id` pour le raisonnement sur le score absent.
pub fn premier_enregistrement(json: &str) -> Option<Enregistrement> {
    let v: Value = serde_json::from_str(json).ok()?;
    let premier = v.get("recordings")?.as_array()?.first()?;
    let Some(score) = premier.get("score").and_then(Value::as_u64) else {
        tracing::warn!("recording search: no score field, refusing rather than guessing");
        return None;
    };
    Some(Enregistrement {
        score,
        titre: premier.get("title")?.as_str()?.to_string(),
        release_id: premier
            .get("releases")
            .and_then(Value::as_array)
            .and_then(|r| r.first())
            .and_then(|r| r.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Forme comparable d'un titre : minuscules, diacritiques retirés, et tout ce
/// qui n'est ni lettre ni chiffre ramené à un espace unique.
///
/// **Pas** une normalisation Unicode complète, et c'est assumé : une crate de
/// décomposition pour une soixantaine de caractères latins ne se justifie pas
/// dans ce dépôt, et un titre en écriture non latine n'a pas de diacritique à
/// retirer — il traverse cette fonction inchangé, ce qui est exactement le
/// comportement voulu.
pub fn normalise(s: &str) -> String {
    let mut mots: Vec<String> = Vec::new();
    let mut courant = String::new();
    for c in s.chars() {
        let c = sans_diacritique(c).to_lowercase().next().unwrap_or(c);
        if c.is_alphanumeric() {
            courant.push(c);
        } else if !courant.is_empty() {
            mots.push(std::mem::take(&mut courant));
        }
    }
    if !courant.is_empty() {
        mots.push(courant);
    }
    mots.join(" ")
}

/// Le caractère latin de base d'un caractère accentué, sinon lui-même.
///
/// Table plutôt qu'algorithme : elle couvre le français, l'espagnol,
/// l'allemand et le portugais, ce qui est le parc réel d'un appareil de salon
/// européen. Ce qui n'y figure pas passe inchangé.
fn sans_diacritique(c: char) -> char {
    match c {
        'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'î' | 'ï' | 'í' | 'ì' => 'i',
        'ô' | 'ö' | 'ó' | 'õ' | 'ò' => 'o',
        'ù' | 'û' | 'ü' | 'ú' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        'ÿ' | 'ý' => 'y',
        'À' | 'Â' | 'Ä' | 'Á' | 'Ã' | 'Å' => 'A',
        'É' | 'È' | 'Ê' | 'Ë' => 'E',
        'Î' | 'Ï' | 'Í' | 'Ì' => 'I',
        'Ô' | 'Ö' | 'Ó' | 'Õ' | 'Ò' => 'O',
        'Ù' | 'Û' | 'Ü' | 'Ú' => 'U',
        'Ç' => 'C',
        'Ñ' => 'N',
        autre => autre,
    }
}

/// Cherche un enregistrement, et rend ce qu'on en sait. `Ok(None)` = rien
/// trouvé ou hors ligne, comme partout dans ce module.
pub async fn cherche_enregistrement(artist: &str, title: &str) -> Result<Option<Enregistrement>> {
    let url = requete_recording(artist, title);
    let Some(body) = requete_texte(&url).await? else { return Ok(None) };
    Ok(premier_enregistrement(&body))
}
```

- [ ] **Step 4 : les tests passent.**

- [ ] **Step 5 : preuve par mutation (deux propriétés)**

Commiter, puis :
1. Faire rendre `normalise` la chaîne d'entrée telle quelle → `la_normalisation_rend_comparables…` tombe.
2. Faire rendre `normalise` une chaîne vide → `la_normalisation_ne_confond_pas…` tombe. Les deux mutations opposées prouvent que la paire de tests borne la fonction des deux côtés.

- [ ] **Step 6 : clippy et commit**

```bash
git commit -am "feat(musicbrainz): recherche d'enregistrement, et la normalisation qui valide"
```

---

### Task 5 : le magasin de motifs

**Files:**
- Create: `crates/ritornello-plugin-musicbrainz/src/motifs.rs`
- Modify: `crates/ritornello-plugin-musicbrainz/src/icy.rs` (ajouter `applique`)
- Modify: `crates/ritornello-plugin-musicbrainz/src/main.rs` (`mod motifs;`)
- Modify: `crates/ritornello-plugin-musicbrainz/Cargo.toml` (dev-dep `tempfile = "3"`)

**Interfaces:**
- Consumes: `icy::Candidat`.
- Produces: `motifs::{Motif, Origine, Entree, Magasin}`. `Magasin::{charge(&Path), enregistre(&Path), entree(&str), apprend(&str, Motif), pose_manuel(&str, Motif), succes(&str), supprime(&str), vide, entrees}`. `Entree { url, motif, origine, dernier_usage, titres_decoupes }`. `Motif::depuis_candidat(&Candidat) -> Motif`, `Origine::depuis_motif(&Motif) -> Origine`. `icy::applique(&Motif, &str) -> Option<(String, String)>`.

- [ ] **Step 1 : les tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn separe(sep: &str, premier: bool) -> Motif {
        Motif::Separe { separateur: sep.to_string(), artiste_en_premier: premier }
    }

    #[test]
    fn lorigine_se_derive_du_motif_et_ne_peut_pas_le_contredire() {
        // L'invariant : `StandardConfirme` ne s'apparie qu'avec le standard.
        // Laisser les deux champs libres autoriserait un « standard confirmé »
        // qui ne découpe pas, que rien ne rattraperait ensuite.
        assert_eq!(Origine::depuis_motif(&separe(" - ", true)), Origine::StandardConfirme);
        assert_eq!(Origine::depuis_motif(&separe(" - ", false)), Origine::DeviationApprise);
        assert_eq!(Origine::depuis_motif(&separe(" / ", true)), Origine::DeviationApprise);
        assert_eq!(Origine::depuis_motif(&Motif::NePasDecouper), Origine::DeviationApprise);
    }

    #[test]
    fn un_motif_pose_a_la_main_est_manuel_meme_sil_est_standard() {
        let mut m = Magasin::default();
        m.pose_manuel("http://f", separe(" - ", true));
        assert_eq!(m.entree("http://f").unwrap().origine, Origine::Manuel);
    }

    #[test]
    fn apprendre_nefface_jamais_un_motif_manuel() {
        // La règle sur laquelle repose la confiance dans la page : sans elle,
        // le premier morceau après une correction la déferait en silence.
        let mut m = Magasin::default();
        m.pose_manuel("http://f", separe(" / ", false));
        m.apprend("http://f", separe(" - ", true));
        let e = m.entree("http://f").unwrap();
        assert_eq!(e.origine, Origine::Manuel);
        assert_eq!(e.motif, separe(" / ", false), "le motif manuel doit survivre");
    }

    #[test]
    fn une_entree_existe_des_que_la_station_est_sondee_meme_conforme() {
        // L'invariant de stockage : « conforme » est une entrée, pas une
        // absence. L'absence confondrait « jamais sondée » et « vérifiée ».
        let mut m = Magasin::default();
        m.apprend("http://f", separe(" - ", true));
        let e = m.entree("http://f").expect("une station conforme doit avoir son entree");
        assert_eq!(e.origine, Origine::StandardConfirme);
    }

    #[test]
    fn les_succes_se_comptent_et_datent_lentree() {
        let mut m = Magasin::default();
        m.apprend("http://f", separe(" - ", true));
        assert_eq!(m.entree("http://f").unwrap().titres_decoupes, 0);
        m.succes("http://f");
        m.succes("http://f");
        assert_eq!(m.entree("http://f").unwrap().titres_decoupes, 2);
        assert!(m.entree("http://f").unwrap().dernier_usage.is_some());
    }

    #[test]
    fn un_fichier_illisible_donne_un_magasin_vide_et_non_une_erreur() {
        // Un état rejetable : on réapprend. Faire échouer le démarrage du
        // greffon pour un fichier de cache serait disproportionné.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("etat.json");
        std::fs::write(&p, "{ ceci n'est pas du json").unwrap();
        assert!(Magasin::charge(&p).entrees().is_empty());
        assert!(Magasin::charge(&dir.path().join("absent.json")).entrees().is_empty());
    }

    #[test]
    fn un_aller_retour_sur_disque_conserve_tout() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sous").join("etat.json");
        let mut m = Magasin::default();
        m.pose_manuel("http://a", separe(" – ", false));
        m.apprend("http://b", Motif::NePasDecouper);
        m.succes("http://a");
        m.enregistre(&p).unwrap();

        let relu = Magasin::charge(&p);
        assert_eq!(relu.entree("http://a"), m.entree("http://a"));
        assert_eq!(relu.entree("http://b").unwrap().motif, Motif::NePasDecouper);
    }

    #[test]
    fn supprimer_une_entree_la_rend_a_nouveau_sondable() {
        // Le geste de reprise pour une station classée « ne pas découper » :
        // rien ne la resonde automatiquement, la suppression est le remède.
        let mut m = Magasin::default();
        m.apprend("http://f", Motif::NePasDecouper);
        m.supprime("http://f");
        assert!(m.entree("http://f").is_none());
    }
}
```

Et dans `icy.rs` :

```rust
    #[test]
    fn appliquer_un_motif_redonne_le_couple() {
        let m = Motif::Separe { separateur: " - ".into(), artiste_en_premier: false };
        assert_eq!(
            applique(&m, "So What - Miles Davis"),
            Some(("Miles Davis".to_string(), "So What".to_string())),
            "ordre inverse : l'artiste est en second"
        );
    }

    #[test]
    fn appliquer_un_motif_absent_de_la_chaine_rend_none() {
        // Le morceau où la station change de forme : pas un couple bancal,
        // rien du tout. C'est ce `None` qui compte comme échec de validation.
        let m = Motif::Separe { separateur: " - ".into(), artiste_en_premier: true };
        assert_eq!(applique(&m, "Vous ecoutez Radio X"), None);
    }

    #[test]
    fn ne_pas_decouper_ne_produit_jamais_de_couple() {
        assert_eq!(applique(&Motif::NePasDecouper, "Miles Davis - So What"), None);
    }
```

- [ ] **Step 2 : lancer, vérifier l'échec.**

- [ ] **Step 3 : implémenter `motifs.rs`**

Contraintes de forme, à respecter exactement :

- `Motif` : `enum { Separe { separateur: String, artiste_en_premier: bool }, NePasDecouper }`, `#[serde(rename_all = "snake_case")]`, dérive `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`.
- `Origine` : `enum { StandardConfirme, DeviationApprise, Manuel }`, `#[serde(rename_all = "snake_case")]`, mêmes dérives.
- `Entree` : `{ url: String, motif: Motif, origine: Origine, dernier_usage: Option<String>, titres_decoupes: u64 }`. `dernier_usage` est une chaîne ISO-8601 UTC — **pas** un type de date : ce dépôt n'a pas de crate de date, la valeur ne sert qu'à trier et à afficher, et la produire depuis `SystemTime` en une ligne évite une dépendance. `#[serde(default)]` sur `dernier_usage` et `titres_decoupes`.
- `Magasin` : `#[derive(Debug, Default)] pub struct Magasin { stations: Vec<Entree> }`, sérialisé comme `{ "stations": [...] }`.
- `Origine::depuis_motif` porte l'invariant. `Motif::depuis_candidat(c)` rend `Separe { c.separateur.to_string(), c.artiste_en_premier }`.
- `apprend` : si l'entrée existe et que son origine est `Manuel`, **ne rien faire** (et le dire en `debug`). Sinon poser/remplacer motif + origine dérivée, en **conservant** `titres_decoupes` et `dernier_usage` si l'entrée existait.
- `charge` / `enregistre` : reprendre **verbatim** le motif de `crates/ritornello-plugin-radio/src/state.rs` — `read_to_string().ok().and_then(from_str).unwrap_or_default()` pour la lecture, et pour l'écriture `create_dir_all(parent)`, nom temporaire portant le pid **et** un compteur unique, `write` puis `rename`. Le commentaire de ce fichier explique pourquoi le `.tmp` doit être unique ; le reprendre.
- `enregistre` rend `anyhow::Result<()>` ; les appelants journalisent l'échec sans mourir.

Puis dans `icy.rs` :

```rust
/// Rejoue un motif appris sur une chaîne nettoyée.
///
/// **Aucun réseau** : c'est là tout l'intérêt du souvenir. Une fois le motif
/// d'une station connu, séparer artiste et titre est une opération locale, et
/// seule la pochette demande encore une requête.
///
/// `None` quand le motif ne s'applique pas : la chaîne ne porte pas ce
/// séparateur, une moitié est vide, ou le motif est `NePasDecouper`. Ce `None`
/// **est** l'échec de validation dont parle la règle des trois échecs
/// consécutifs — pas une erreur, un morceau qui ne rentre pas dans la forme.
pub fn applique(motif: &crate::motifs::Motif, nettoye: &str) -> Option<(String, String)> {
    let crate::motifs::Motif::Separe { separateur, artiste_en_premier } = motif else {
        return None;
    };
    let (tete, reste) = nettoye.split_once(separateur.as_str())?;
    let (tete, reste) = (tete.trim(), reste.trim());
    if tete.is_empty() || reste.is_empty() {
        return None;
    }
    Some(if *artiste_en_premier {
        (tete.to_string(), reste.to_string())
    } else {
        (reste.to_string(), tete.to_string())
    })
}
```

- [ ] **Step 4 : les tests passent** (`cargo test -p ritornello-plugin-musicbrainz --offline`).

- [ ] **Step 5 : preuve par mutation (deux propriétés)**

Commiter, puis :
1. Retirer la garde `Manuel` de `apprend` → `apprendre_nefface_jamais_un_motif_manuel` tombe.
2. Faire rendre `Origine::depuis_motif` toujours `StandardConfirme` → `lorigine_se_derive_du_motif…` tombe.

- [ ] **Step 6 : clippy et commit**

```bash
git commit -am "feat(musicbrainz): le magasin de motifs par station, et son invariant d'origine"
```

---

### Task 6 : le sondage et le régime établi

Le câblage. C'est la tâche la plus délicate : elle touche la boucle du greffon,
dont la doc existante explique pourquoi chaque garde est là. **Lire `main.rs` en
entier avant de commencer**, en particulier la doc de `next_enrichment` sur
l'annulabilité du `select!`.

**Files:**
- Modify: `crates/ritornello-plugin-musicbrainz/src/main.rs`

**Interfaces:**
- Consumes: tout ce que les tâches 1 à 5 produisent.
- Produces: `MusicBrainzPlugin::new(magasin: Arc<RwLock<Magasin>>, chemin_etat: PathBuf)` — signature changée, l'admin partageant le magasin. `ECHECS_AVANT_RESONDAGE`.

- [ ] **Step 1 : l'état ajouté au greffon**

À `struct MusicBrainzPlugin`, ajouter — en gardant le style de commentaire des champs voisins :

```rust
    // --- Chemin ICY (radio) ---
    /// Le magasin, **partagé avec la page d'admin** : les deux moitiés du
    /// processus le lisent et l'écrivent, comme les deux moitiés du greffon
    /// radio partagent son fichier d'état.
    magasin: std::sync::Arc<tokio::sync::RwLock<motifs::Magasin>>,
    chemin_etat: std::path::PathBuf,
    /// Dernière chaîne brute traitée. Icecast répète le même en-tête tout au
    /// long d'un morceau : sans cette garde, chaque répétition relancerait une
    /// requête.
    icy_vu: Option<String>,
    /// Échecs de validation **consécutifs**, par URL de flux. En mémoire et
    /// non persisté : c'est une suite d'événements en cours, pas un fait acquis
    /// sur la station, et un redémarrage est une remise à zéro légitime.
    echecs: std::collections::HashMap<String, u32>,
    /// URL dont un traitement est en vol, pour ne pas le lancer deux fois.
    icy_en_vol: Option<String>,
    icy_tx: mpsc::Sender<IssueIcy>,
    icy_rx: mpsc::Receiver<IssueIcy>,
```

Et le message de retour :

```rust
/// Ce qu'une tâche de traitement ICY rapporte, en **un seul** message.
///
/// Un message et non deux (« voici le motif », « voici le couple ») : la
/// boucle doit pouvoir mettre à jour le magasin, le compteur d'échecs et
/// l'enrichissement dans le même tour, sans état intermédiaire où le motif
/// serait retenu mais le compteur pas encore remis à zéro.
#[derive(Debug)]
struct IssueIcy {
    url: String,
    /// La chaîne traitée. Sert de garde de péremption : une issue qui ne
    /// décrit pas la chaîne courante est jetée, comme les deux autres chemins
    /// jettent une réponse qui ne décrit plus ce qui joue.
    brut: String,
    /// Le motif à retenir quand un sondage a eu lieu. `None` = pas de
    /// sondage (régime établi), donc rien à apprendre.
    motif: Option<motifs::Motif>,
    /// Le couple validé et sa pochette. `None` = validation échouée.
    valide: Option<(String, String, Option<String>)>,
}
```

- [ ] **Step 2 : la fonction de traitement, détachée**

Écrire une fonction libre (pas une méthode : elle ne doit pas capturer `&mut self`) :

```rust
/// Traite une chaîne ICY : applique le motif connu, ou sonde la station.
///
/// Détachée dans une tâche, comme les deux autres chemins : une station peut
/// coûter quatre requêtes espacées d'une seconde, et la boucle du greffon ne
/// doit pas attendre.
async fn traite_icy(
    url: String,
    brut: String,
    connu: Option<motifs::Motif>,
    resonde: bool,
) -> IssueIcy { … }
```

Règles, dans cet ordre :

1. `nettoye = icy::nettoie(&brut)`.
2. `connu == Some(NePasDecouper)` et `!resonde` ⟹ rendre `IssueIcy { motif: None, valide: None }` **sans aucune requête**. C'est la station parlée, et son coût doit être nul.
3. `connu == Some(Separe{..})` et `!resonde` ⟹ `icy::applique`. `None` ⟹ `valide: None`. `Some((a, t))` ⟹ une recherche d'enregistrement ; validée ⟹ `valide: Some((a, t, release_id))`, sinon `valide: None`. `motif: None` dans les deux cas.
4. Sinon (station inconnue, ou `resonde`) ⟹ **sondage** : `icy::candidats(&nettoye)`, chacun éprouvé par `cherche_enregistrement`, un candidat accepté si `score >= SEUIL_RECORDING` **et** `normalise(enregistrement.titre) == normalise(candidat.titre)`. Retenir **le meilleur score** parmi les acceptés — pas le premier. Aucun candidat, ou aucun accepté ⟹ `motif: Some(NePasDecouper)`, `valide: None`. Sinon `motif: Some(Motif::depuis_candidat(gagnant))` et `valide` renseigné.
5. Journaliser, en anglais : le nombre de candidats éprouvés, celui retenu et son score, et **ce que le plafond a écarté** s'il a mordu — un plafond silencieux se lit comme « on a tout essayé ».
6. Si `brut` porte un `U+FFFD` ou une séquence caractéristique de latin-1 relu en UTF-8, le journaliser **distinctement** (`warn`, texte parlant de l'encodage) : un titre en mojibake ne validera jamais, et ressemble à un mauvais découpage alors que le découpage était bon.

- [ ] **Step 3 : le déclencheur dans `now_playing`**

Dans la branche `None` du `match disque` (identité qui n'est pas un disque),
**après** le traitement générique existant qu'il ne faut pas toucher :

- Extraire l'URL : identité dont `kind == "stream"`, champ `url` non vide. Écrire une fonction pure `url_de_flux(&Value) -> Option<String>` à côté de `disque_de`, et la tester comme elle (forme inattendue écartée sans bruit).
- Si `np.known.stream_title` diffère de `self.icy_vu` : mémoriser, lire le motif connu sous verrou de lecture, et détacher `traite_icy` avec `resonde = self.echecs.get(&url) >= ECHECS_AVANT_RESONDAGE`.
- `self.icy_en_vol` empêche le doublon.

- [ ] **Step 4 : la branche du `select!`**

Ajouter un troisième bras `r = self.icy_rx.recv()`, sur le modèle **exact** des deux existants (garde de péremption comprise, `std::future::pending()` sur `None`) :

1. Jeter l'issue si `brut` n'est plus `self.icy_vu` — une station a pu changer de morceau pendant le vol.
2. `motif: Some(m)` ⟹ `magasin.write().await.apprend(&url, m)` puis `enregistre(&chemin_etat)` (échec journalisé, pas fatal).
3. `valide: Some((a, t, rid))` ⟹ `magasin.write().await.succes(&url)`, `enregistre`, `self.echecs.remove(&url)`, et poser `self.pret` avec un `Enrichment` : `identity` = l'identité en écho, `artist`, `title`, `cover` depuis `rid`, **`fill_only: false`**.
4. `valide: None` ⟹ `*self.echecs.entry(url).or_default() += 1`, et **ne rien poser**.

Le commentaire doit dire pourquoi `fill_only: false` alors que le chemin
générique voisin est en `fill_only: true` : ici on **remplace** la chaîne ICY
brute, qui est précisément ce qu'on corrige, et on ne le fait que sur ce que
MusicBrainz a confirmé.

- [ ] **Step 5 : `main` et le chemin d'état**

```rust
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let chemin_etat = std::path::PathBuf::from(
        std::env::var("RITORNELLO_MUSICBRAINZ_STATE")
            .unwrap_or_else(|_| "/var/lib/ritornello/plugin-musicbrainz.json".to_string()),
    );
    let magasin = std::sync::Arc::new(tokio::sync::RwLock::new(motifs::Magasin::charge(&chemin_etat)));
    Runtime::from_args()?
        .metadata(MusicBrainzPlugin::new(magasin.clone(), chemin_etat.clone()))?
        .run()
        .await
}
```

(La ligne `.admin(...)` arrive en tâche 8.)

- [ ] **Step 6 : les tests**

Les tests de ce fichier pilotent le greffon par `now_playing` puis
`next_enrichment`, comme les tests existants — **reprendre leur banc**, ne pas
en inventer un second. Le réseau n'est pas joignable en test : les cas qui
demandent une réponse MusicBrainz se prouvent sur `traite_icy` en injectant les
réponses, donc **extraire la validation en fonction pure** :

```rust
/// Choisit le meilleur candidat accepté parmi des réponses déjà obtenues.
///
/// Séparée du réseau exprès : c'est la décision, et c'est elle qui doit être
/// éprouvée. Les paires sont `(candidat, réponse)`, dans l'ordre d'essai.
fn meilleur_accepte(
    essais: &[(icy::Candidat, Option<musicbrainz::Enregistrement>)],
) -> Option<&icy::Candidat> { … }
```

Tests exigés :

```rust
#[test]
fn le_meilleur_score_gagne_et_non_le_premier_accepte() {
    // Le gagnant est **second** dans l'ordre d'essai : sans cela, le test
    // passerait aussi avec « prendre le premier accepté ».
    …
}

#[test]
fn un_titre_qui_ne_correspond_pas_est_ecarte_malgre_un_bon_score() {
    // La garde qui porte tout : le score seul est trop généreux, la recherche
    // rendant presque toujours quelque chose de plausible.
    …
}

#[test]
fn aucun_candidat_accepte_donne_ne_pas_decouper() { … }

#[tokio::test]
async fn une_station_classee_ne_pas_decouper_ne_declenche_aucune_requete() {
    // `traite_icy` avec `connu = NePasDecouper` et `resonde = false` doit
    // rendre son issue **sans** toucher au réseau. Prouvé par le fait que le
    // test passe alors qu'aucun réseau n'est joignable : une requête tentée
    // ferait échouer le test par le délai.
    …
}

#[tokio::test]
async fn un_echec_isole_ne_resonde_pas_et_trois_daffilee_resondent() {
    // Les deux moitiés. Sans la première, « resonder toujours » passerait ;
    // sans la seconde, « ne resonder jamais » passerait.
    …
}

#[tokio::test]
async fn un_succes_remet_le_compteur_a_zero() {
    // Deux échecs, un succès, deux échecs : pas de resondage. La seule
    // assertion qui distingue un compteur consécutif d'un cumulatif — et le
    // cumulatif est le défaut naturel.
    …
}

#[test]
fn une_identite_qui_nest_pas_un_flux_nest_pas_traitee() {
    assert!(url_de_flux(&serde_json::json!({"kind":"disc","toc":"1 2 3"})).is_none());
    assert!(url_de_flux(&serde_json::json!({"kind":"stream"})).is_none());
    assert!(url_de_flux(&serde_json::json!({"kind":"stream","url":""})).is_none());
    assert_eq!(
        url_de_flux(&serde_json::json!({"kind":"stream","url":"http://f"})).as_deref(),
        Some("http://f")
    );
}
```

- [ ] **Step 7 : preuve par mutation (trois propriétés)**

Commiter, puis, une à la fois :
1. `meilleur_accepte` rend le premier accepté → `le_meilleur_score_gagne…` tombe.
2. Retirer la comparaison de titre normalisé → `un_titre_qui_ne_correspond_pas…` tombe.
3. Remplacer le compteur consécutif par un cumul (ne pas remettre à zéro au succès) → `un_succes_remet_le_compteur_a_zero` tombe.

- [ ] **Step 8 : clippy, workspace, commit**

```bash
cargo clippy -p ritornello-plugin-musicbrainz --offline --all-targets -- -D warnings
cargo test --workspace --offline
git commit -am "feat(musicbrainz): sondage d'une station, puis decoupage local a chaque morceau"
```

---

### Task 7 : les catalogues de traduction

Le greffon n'a **ni** `src/locales/` **ni** pack français. Les deux sont à créer
avant la page, qui les consomme.

**Files:**
- Create: `crates/ritornello-plugin-musicbrainz/src/locales/en.toml`
- Create: `deploy/locales/musicbrainz/fr.toml`
- Modify: `crates/ritornello-plugin-musicbrainz/src/main.rs` (constante `MUSICBRAINZ_EN`)

**Interfaces:**
- Produces: `MUSICBRAINZ_EN: &str`, et les clés listées ci-dessous.

- [ ] **Step 1 : les clés**

`en.toml`, exactement ces clés (le pack `fr.toml` porte les mêmes, traduites) :

```toml
# Page d'admin du greffon musicbrainz : les motifs de decoupage appris par
# station. Anglais embarque dans le binaire ; le pack fr vit dans deploy/.
title = "ICY split patterns"
intro = "One entry per station this device has probed. A station's ICY format is a property of the station, not of the track, so the pattern is learned once and then applied locally."
col_station = "Stream"
col_pattern = "Pattern"
col_origin = "Origin"
col_last_used = "Last used"
col_split_count = "Titles split"
col_actions = ""
origin_standard = "standard, confirmed"
origin_learned = "learned deviation"
origin_manual = "manual"
pattern_no_split = "do not split"
pattern_artist_first = "artist first"
pattern_title_first = "title first"
filter_exceptions_only = "Exceptions only"
empty = "No station probed yet."
empty_filtered = "No exception: every probed station follows the standard format."
edit = "Edit"
delete = "Delete"
clear_all = "Clear all"
save = "Save"
cancel = "Cancel"
field_separator = "Separator"
field_order = "Order"
field_no_split = "Do not split this station"
separator_empty = "the separator cannot be empty"
separator_no_space = "the separator must contain a space on each side, otherwise a hyphenated name gets cut in two"
unknown_station = "no entry for that stream"
save_failed = "could not write the pattern file"
```

`separator_no_space` porte une règle de validation réelle : sans espaces
autour, `Jean-Michel Jarre` se ferait couper. La page **et** le dorsal la
vérifient, et le dorsal est l'autorité.

- [ ] **Step 2 : la constante et le test de parité**

Dans `main.rs` :

```rust
/// Catalogue anglais embarqué. Le pack français vit dans
/// `deploy/locales/musicbrainz/fr.toml` — voir le test de parité.
pub(crate) const MUSICBRAINZ_EN: &str = include_str!("locales/en.toml");
```

Le test de parité va en tâche 8, dans `admin.rs`, sur le modèle **exact** de
`crates/ritornello-plugin-mpd/src/admin.rs` (fonction
`parite_des_cles_entre_len_embarque_et_le_pack_fr`) : lire le pack via
`CARGO_MANIFEST_DIR`, comparer les jeux de clés triés.

- [ ] **Step 3 : commit**

```bash
git commit -am "feat(musicbrainz): catalogues en/fr du greffon"
```

---

### Task 8 : le dorsal d'admin

**Files:**
- Create: `crates/ritornello-plugin-musicbrainz/src/admin.rs`
- Create: `crates/ritornello-plugin-musicbrainz/build.rs`
- Modify: `crates/ritornello-plugin-musicbrainz/Cargo.toml` (dépendance `ritornello-i18n`, `build = "build.rs"`)
- Modify: `crates/ritornello-plugin-musicbrainz/src/main.rs` (`mod admin;`, `.admin(...)`, chargement du catalogue)

**Interfaces:**
- Consumes: `motifs::Magasin` (partagé), `MUSICBRAINZ_EN`.
- Produces: `admin::MusicBrainzAdmin`.

- [ ] **Step 1 : recopier la machinerie du greffon MPD**

`build.rs` : reprendre `crates/ritornello-plugin-mpd/build.rs` **à l'identique**,
en ne changeant que les chemins. Il ne lance jamais `npm` (la
cross-compilation se fait sans Node) : il garantit l'existence de
`ui/dist/ui.js` et `ui/dist/ui.css`, écrit un bouchon si absent, et émet un
`cargo::warning` **tant que** le bouchon est là — pas seulement à sa création.

`admin.rs` implémente `ritornello_plugin_sdk::AdminPlugin` :

- `asset(path)` : `match path { "ui.js" => include_str!("../ui/dist/ui.js"), "ui.css" => …, _ => None }` avec le mime, `None` ailleurs (le cœur en fait un 404).
- `catalog()` : le catalogue à plat, en JSON.
- `get_data()` : `{ "stations": [ … ] }`, les entrées **triées par `dernier_usage` décroissant**, sans date en premier.
- `set_data(v)` : désérialiser dans une structure dédiée à champs **obligatoires**, distincte de `Entree` :

```rust
/// Ce que la page envoie. Structure dédiée et champs obligatoires, comme
/// `EcritureConfig` du greffon MPD : `Entree` a des `serde(default)` pour
/// relire un fichier d'une version antérieure, et les réutiliser ici ferait
/// qu'un champ oublié par la page passerait pour un choix.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Ecriture {
    /// Poser un motif à la main sur une station. Toujours `Origine::Manuel`.
    Pose { url: String, motif: MotifEcrit },
    Supprime { url: String },
    Vide,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MotifEcrit {
    Separe { separateur: String, artiste_en_premier: bool },
    NePasDecouper,
}
```

- Validation dans `set_data`, **avant** toute écriture : séparateur non vide,
  et commençant *et* finissant par une espace. Le refus rend une **phrase
  traduite**, jamais une clé brute.
- `Supprime` sur une URL absente : refus avec `unknown_station`, pas un succès
  silencieux — la page afficherait « fait » sur un geste sans effet.
- Après toute écriture : `enregistre(&chemin_etat)`, et l'échec devient
  `save_failed` traduit.

- [ ] **Step 2 : les tests**

```rust
#[tokio::test]
async fn poser_un_motif_le_rend_manuel_et_le_persiste() { … }

#[tokio::test]
async fn un_separateur_sans_espaces_est_refuse_par_une_phrase_pas_une_cle() {
    // Le contrat du SDK, et la règle réelle : sans espaces autour,
    // `Jean-Michel Jarre` se ferait couper en deux.
    let err = /* set_data avec separateur "-" */;
    assert!(err.contains("space"), "doit etre la phrase du catalogue : {err}");
    assert!(!err.contains("separator_no_space"), "jamais la cle brute : {err}");
}

#[tokio::test]
async fn supprimer_une_station_inconnue_est_un_refus_et_non_un_succes_muet() { … }

#[tokio::test]
async fn get_data_trie_par_dernier_usage_decroissant() { … }

#[tokio::test]
async fn une_ecriture_malformee_est_rejetee() {
    // Champ manquant, action inconnue : refus, pas un défaut appliqué.
    …
}

#[test]
fn les_actifs_inconnus_ne_sont_pas_servis() {
    // `None` = 404 côté cœur. Servir autre chose ouvrirait une route de
    // lecture arbitraire.
    …
}

#[test]
fn parite_des_cles_entre_len_embarque_et_le_pack_fr() { /* modèle : plugin-mpd */ }
```

- [ ] **Step 3 : câbler dans `main`**

Charger le catalogue comme le fait `plugin-mpd/src/main.rs` (`RITORNELLO_LOCALES`,
défaut `/etc/ritornello/locales`, locale passée au lancement), puis :

```rust
    Runtime::from_args()?
        .metadata(MusicBrainzPlugin::new(magasin.clone(), chemin_etat.clone()))?
        .admin(admin::MusicBrainzAdmin::new(magasin, chemin_etat, catalog))?
        .run()
        .await
```

Écrire, dans la doc de `MusicBrainzAdmin`, qu'un greffon `metadata` **ne
reçoit pas** `SetLocale` — cette trame n'existe que pour `SourcePlugin` — donc
le catalogue est figé à la langue du lancement et un changement de langue ne se
voit qu'après redémarrage du greffon. Même limite que la page du greffon MPD.

- [ ] **Step 4 : tests, clippy, commit**

```bash
cargo test -p ritornello-plugin-musicbrainz --offline
cargo clippy -p ritornello-plugin-musicbrainz --offline --all-targets -- -D warnings
git commit -am "feat(musicbrainz): dorsal d'admin, et la validation du separateur"
```

---

### Task 9 : la page

**Files:**
- Create: `crates/ritornello-plugin-musicbrainz/ui/{package.json,tsconfig.json,vite.config.ts,vitest.config.ts}`
- Create: `crates/ritornello-plugin-musicbrainz/ui/src/{index.ts,MusicBrainzAdmin.vue,ui.css,MusicBrainzAdmin.test.ts,i18nKeysUsed.test.ts}`

**Interfaces:**
- Consumes: le contrat `get_data` / `set_data` de la tâche 8.
- Produces: `ui/dist/ui.js`, `ui/dist/ui.css`.

- [ ] **Step 1 : recopier le paquet du greffon MPD**

Reprendre `crates/ritornello-plugin-mpd/ui/` : les quatre fichiers de
configuration à l'identique, en ne changeant que le `name` du `package.json`
(`ritornello-plugin-musicbrainz-ui`). `vue` et `@ritornello/ui` restent
**externes** — ils viennent de l'import map du shell hôte —, la sortie reste une
bibliothèque ES unique `ui.js` avec un `ui.css` unique
(`cssCodeSplit: false`).

- [ ] **Step 2 : le composant**

Exigences, toutes vérifiables :

- Tableau : `data-station-ligne` par entrée, colonnes flux / motif / origine /
  dernier usage / titres découpés / actions.
- **Filtre « exceptions seulement » actif par défaut** (`data-filtre-exceptions`),
  qui masque les entrées d'origine `standard_confirme`.
- Deux messages de vide **distincts** : `empty` quand rien n'a été sondé,
  `empty_filtered` quand le filtre masque tout. Sans cette distinction, un
  écran vide serait ambigu — tout va bien, ou rien n'a jamais marché ?
- Édition (`data-editer`) : un champ séparateur (`data-separateur`), un choix
  d'ordre (`data-ordre`), une case « ne pas découper » (`data-ne-pas-decouper`)
  qui grise les deux précédents. **Jamais** de champ d'expression rationnelle.
- `data-supprimer` par ligne, `data-vider` global.
- La largeur : l'URL d'un flux est longue. La colonne doit tronquer avec un
  `title` complet, et le tableau défiler dans son propre conteneur
  (`overflow-x: auto`) sans faire défiler la page.

- [ ] **Step 3 : les tests**

```ts
it('masque les stations conformes par defaut', …)
it('les montre quand on decoche le filtre', …)
it('distingue « rien de sonde » de « aucune exception »', …)
it('« ne pas decouper » grise le separateur et l ordre', …)
it('poste une action pose avec un motif du jeu ferme', …)
it('poste une action supprime, puis rafraichit', …)
it('affiche l erreur du dorsal telle quelle', …)
```

Plus `i18nKeysUsed.test.ts` sur le modèle du greffon MPD : toute clé employée
par le composant doit exister dans `en.toml`.

- [ ] **Step 4 : construire, tester, commit**

```bash
cd crates/ritornello-plugin-musicbrainz/ui && npm run build && npm run test
```

Si `npm` n'est pas disponible dans l'environnement de l'implémenteur, le
`build.rs` a déjà écrit un bouchon : le noter dans le rapport comme **non
vérifié**, ne pas prétendre le contraire.

```bash
git commit -am "feat(musicbrainz): la page des motifs, filtree sur les exceptions"
```

---

### Task 10 : la documentation et l'exemple de déploiement

**Files:**
- Modify: `docs/plugins.md`
- Modify: `deploy/plugins.example.toml`
- Create: `deploy/musicbrainz.example.toml` **seulement si** un réglage doit vivre dans `/etc` — sinon **ne pas le créer** : l'état vit dans `/var/lib` et n'est pas une configuration.

- [ ] **Step 1 : l'exigence d'ordre, écrite**

Dans `deploy/plugins.example.toml`, au-dessus du bloc `musicbrainz`, un
commentaire disant que sa position **après** les greffons de station est une
exigence et non un hasard : `bloc_de_texte` parcourt les greffons dans l'ordre
déclaré et rend le premier bloc non-`fill_only` **en entier**, donc un
`musicbrainz` déclaré avant `ouifm-metas` gagnerait l'arbitrage sur les
stations que celui-ci connaît mieux.

- [ ] **Step 2 : la section de `docs/plugins.md`**

En anglais, comme le reste du fichier. Doit couvrir : le champ `stream_title` et
pourquoi il existe, le motif appris par URL de flux, les quatre origines, le
fichier d'état et sa variable, les deux seuils, la règle des trois échecs
consécutifs, le fait qu'une station « ne pas découper » n'est jamais resondée
automatiquement et que la suppression depuis la page est le remède, et
l'exigence d'ordre de déclaration.

- [ ] **Step 3 : commit**

```bash
git commit -am "docs(plugins): le decoupage ICY, ses seuils, et l'ordre de declaration qui compte"
```

---

## Vérification finale

- [ ] `cargo test --workspace --offline` : zéro échec.
- [ ] `cargo clippy -p <crate> --offline --all-targets -- -D warnings` sur `ritornello-proto`, `ritornello-core`, `ritornello-plugin-musicbrainz` — **par crate**, jamais `--workspace`.
- [ ] `grep -rn "// mutation" crates/` : vide.
- [ ] Suite web inchangée : `cd web/app && npx --no-install vitest run`.
- [ ] Le greffon démarre sans fichier d'état et n'écrit rien avant d'avoir appris.
