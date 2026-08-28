//! Le store de patterns appris, un par station : quel découpage vérifier une
//! fois, et retenir. Le format ICY est une propriété de la **station**, pas du
//! track, donc l'unité de mémorisation est l'URL du stream, sondée une fois
//! puis rejouée sans réseau.
//!
//! Deux énumérations, pas une : [`Pattern`] dit **ce que c'est** — découper sur
//! tel séparateur dans tel order, ou ne pas découper — et [`Origin`] dit
//! **comment on l'a su** — standard confirmé, déviation apprise, ou manuel.
//! Les confondre mettrait « ne pas découper » parmi les origines, et rendrait
//! un « ne pas découper » posé à la main indistinguable d'un appris. La règle
//! selon laquelle le réapprentissage n'écrase **jamais** un pattern manuel a
//! précisément besoin de cette distinction : sans elle, le premier track
//! après une correction de l'utilisateur la déferait en silence.

use crate::icy::Candidate;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// **Quoi** : comment découper la chaîne annoncée par une station — ou ne pas
/// la découper du tout.
///
/// `DoNotSplit` fait partie du *quoi*, pas du *comment* : c'est une forme
/// de découpage à part entière (l'absence de découpage), au même title que
/// `Split`. La confondre avec une origin empêcherait de poser « ne pas
/// découper » à la main et de le distinguer d'un « ne pas découper » subi.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    Split {
        separator: String,
        artist_first: bool,
        /// Le title est le champ du **milieu** (`Artiste - Titre - Album`), le
        /// reste étant ignoré.
        ///
        /// `serde(default)` : un fichier d'état écrit avant ce champ se relit,
        /// et l'absence vaut « non », qui est la forme courante.
        ///
        /// **Quand il est vrai, `artist_first` n'a plus d'effet** : la
        /// forme à trois champs est toujours « artist, puis title, puis le
        /// reste », et c'est la seule que `icy::candidates` produise. La
        /// combinaison inverse est donc représentable sans être signifiante —
        /// elle n'est pas rendue impossible par le type, parce qu'un troisième
        /// variant d'énumération ferait payer au contrat JSON de la page une
        /// forme qu'elle n'offre pas.
        ///
        /// Ce champ existe parce que `icy::candidates` produit un candidat du
        /// milieu que le pattern devait pouvoir **rejouer**. Sans lui, ce candidat
        /// validait puis était réenregistré sous une forme qui recollait l'album
        /// au title : la validation échouait à chaque track, trois échecs
        /// déclenchaient un resondage, le même candidat regagnait — une boucle
        /// sans fin, trouvée par le test qui compare `apply` à `candidates`.
        #[serde(default)]
        title_in_middle: bool,
    },
    DoNotSplit,
}

impl Pattern {
    /// Le pattern que décrit ce candidat validé.
    ///
    /// L'inverse de [`crate::icy::candidates`] : celui-ci dérive les
    /// découpages plausibles depuis une chaîne, `from_candidate` retient
    /// lequel a validé, pour le rejouer sans réseau la prochaine fois.
    pub fn from_candidate(c: &Candidate) -> Pattern {
        Pattern::Split {
            separator: c.separator.to_string(),
            artist_first: c.artist_first,
            title_in_middle: c.title_in_middle,
        }
    }
}

/// **Comment on l'a su** : d'où vient le pattern retenu pour une station.
///
/// Jamais posée librement à côté d'un pattern quelconque : voir
/// [`Origin::from_pattern`], qui porte l'invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Le séparateur standard (`" - "`), artist en premier : la convention
    /// de fait des automates de diffusion, confirmée par une requête.
    StandardConfirmed,
    /// Tout le reste qu'un sondage a appris : un autre séparateur, l'order
    /// inverse, ou l'absence de découpage validée par élimination.
    LearnedDeviation,
    /// Posé depuis la page d'admin. Rien ne le réapprend jamais.
    Manual,
}

impl Origin {
    /// Dérive l'origin que peut porter ce pattern.
    ///
    /// L'invariant du store : `StandardConfirmed` ne s'apparie qu'au
    /// standard exact. Laisser les deux champs libres autoriserait un
    /// « standard confirmé » qui ne découpe pas, ou qui découpe dans l'order
    /// inverse — que rien ne rattraperait ensuite, puisque `learn` fait
    /// confiance à l'origin déjà posée pour savoir si elle peut réécrire.
    pub fn from_pattern(pattern: &Pattern) -> Origin {
        match pattern {
            // `title_in_middle: false` fait partie de la définition du
            // standard : `Artiste - Titre - Album` est une déviation, même si
            // son séparateur et son order sont ceux du standard.
            Pattern::Split { separator, artist_first: true, title_in_middle: false }
                if separator == " - " =>
            {
                Origin::StandardConfirmed
            }
            _ => Origin::LearnedDeviation,
        }
    }
}

/// Ce que le store retient pour une station.
///
/// Une entrée existe dès que la station a été sondée, même si le résultat est
/// conforme au standard : l'absence confondrait « jamais sondée » et
/// « vérifiée », deux états que l'appelant doit pouvoir distinguer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub url: String,
    pub pattern: Pattern,
    pub origin: Origin,
    /// ISO-8601 UTC, pas un type de date : ce dépôt n'a pas de crate de date,
    /// la valeur ne sert qu'à trier et à afficher, et la produire depuis
    /// `SystemTime` évite une dépendance.
    #[serde(default)]
    pub last_used: Option<String>,
    #[serde(default)]
    pub split_titles: u64,
}

/// Le store, indexé par URL de stream et persisté en JSON.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    stations: Vec<Entry>,
}

impl Store {
    /// Charge le store depuis le disc.
    ///
    /// Un fichier absent ou illisible rend un store clear plutôt qu'une
    /// erreur : un état rejetable pour un simple cache se réapprend, il ne
    /// doit pas empêcher le greffon de démarrer.
    pub fn load(path: &Path) -> Store {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// Écrit le store sur le disc, atomiquement.
    ///
    /// Nom temporaire propre à ce processus **et** à cet appel : un `.tmp`
    /// partagé permettrait à deux écritures simultanées de se voler le
    /// fichier sous le pied (`rename` en ENOENT). Même pattern que
    /// `ritornello-plugin-radio/src/state.rs`.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = path.with_extension(format!("json.tmp.{}.{unique}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// L'entrée d'une station, si elle a déjà été sondée.
    pub fn entry(&self, url: &str) -> Option<&Entry> {
        self.stations.iter().find(|e| e.url == url)
    }

    /// Toutes les entrées, pour la page d'admin.
    pub fn entries(&self) -> &[Entry] {
        &self.stations
    }

    /// Aucune station sondée.
    ///
    /// `is_empty` et non `clear` : c'est la convention de ce dépôt pour un
    /// prédicat (`Known::is_empty`, `Track::is_empty`), et surtout « clear »
    /// seul est ambigu en français — adjectif ou verbe. Mon brief l'avait écrit
    /// ainsi et l'implémenteur a compris le prédicat là où j'entendais
    /// l'action ; les deux existent maintenant, sous deux names qui ne peuvent
    /// plus se confondre.
    pub fn is_empty(&self) -> bool {
        self.stations.is_empty()
    }

    /// Oublie **toutes** les stations : c'est le « tout vider » de la page
    /// d'admin.
    ///
    /// Un geste à part de `remove`, et pas seulement une boucle dessus : il
    /// répond à « je ne fais plus confiance à ce que l'appareil a appris »,
    /// alors que `remove` répond à « reprobe celle-ci ». La page les présente
    /// distinctement pour cette reason, et l'appelant reste chargé
    /// d'`save` — comme pour les autres mutations, pour qu'une écriture
    /// disc ne se cache pas derrière un name qui n'en parle pas.
    pub fn clear_all(&mut self) {
        let combien = self.stations.len();
        self.stations.clear();
        tracing::info!("forgot the split patterns of {combien} stations");
    }

    /// Set le pattern appris d'un sondage.
    ///
    /// Si l'entrée existante est `Manual`, ne fait **rien** : c'est la règle
    /// sur laquelle repose la confiance dans la page d'admin. Sans elle, le
    /// premier track après une correction de l'utilisateur la déferait en
    /// silence.
    pub fn learn(&mut self, url: &str, pattern: Pattern) {
        if let Some(e) = self.stations.iter_mut().find(|e| e.url == url) {
            if e.origin == Origin::Manual {
                tracing::debug!("pattern manuel conserve pour {url}, apprentissage ignore");
                return;
            }
            e.origin = Origin::from_pattern(&pattern);
            e.pattern = pattern;
            return;
        }
        self.stations.push(Entry {
            url: url.to_string(),
            origin: Origin::from_pattern(&pattern),
            pattern,
            last_used: None,
            split_titles: 0,
        });
    }

    /// Set un pattern à la main, depuis la page d'admin : toujours `Manual`,
    /// même quand le pattern posé est le standard.
    pub fn set_manual(&mut self, url: &str, pattern: Pattern) {
        if let Some(e) = self.stations.iter_mut().find(|e| e.url == url) {
            e.pattern = pattern;
            e.origin = Origin::Manual;
            return;
        }
        self.stations.push(Entry {
            url: url.to_string(),
            pattern,
            origin: Origin::Manual,
            last_used: None,
            split_titles: 0,
        });
    }

    /// Compte un title découpé avec succès, et date l'entrée.
    pub fn record_success(&mut self, url: &str) {
        let Some(e) = self.stations.iter_mut().find(|e| e.url == url) else {
            tracing::debug!("record_success signale pour {url}, sans entry correspondante");
            return;
        };
        e.split_titles += 1;
        e.last_used = Some(now_iso8601());
    }

    /// Retire l'entrée d'une station.
    ///
    /// Le geste de reprise pour une station classée « ne pas découper » :
    /// rien ne la reprobe automatiquement, la suppression est le remède.
    pub fn remove(&mut self, url: &str) {
        self.stations.retain(|e| e.url != url);
    }
}

/// Horodatage courant, ISO-8601 UTC.
///
/// Pas de crate de date dans ce dépôt : cette valeur ne sert qu'à trier et à
/// afficher, jamais à un calcul calendaire applicatif. La conversion jours →
/// année/mois/jour est l'algorithme de Howard Hinnant (`civil_from_days`), le
/// classique qui évite la dépendance.
fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (jours, reste) = (secs / 86_400, secs % 86_400);
    let z = jours as i64 + 719_468;
    let ere = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - ere * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let jour = doy - (153 * mp + 2) / 5 + 1;
    let mois = if mp < 10 { mp + 3 } else { mp - 9 };
    let annee = yoe as i64 + ere * 400 + if mois <= 2 { 1 } else { 0 };
    format!(
        "{annee:04}-{mois:02}-{jour:02}T{:02}:{:02}:{:02}Z",
        reste / 3600,
        (reste % 3600) / 60,
        reste % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn separe(sep: &str, premier: bool) -> Pattern {
        Pattern::Split {
            separator: sep.to_string(),
            artist_first: premier,
            title_in_middle: false,
        }
    }

    /// La forme `Artiste - Titre - Album`, dont le pattern doit se distinguer du
    /// standard : voir `Origin::from_pattern`.
    fn separe_milieu(sep: &str) -> Pattern {
        Pattern::Split {
            separator: sep.to_string(),
            artist_first: true,
            title_in_middle: true,
        }
    }

    #[test]
    fn lorigine_se_derive_du_motif_et_ne_peut_pas_le_contredire() {
        // L'invariant : `StandardConfirmed` ne s'apparie qu'avec le standard.
        // Laisser les deux champs libres autoriserait un « standard confirmé »
        // qui ne découpe pas, que rien ne rattraperait ensuite.
        assert_eq!(Origin::from_pattern(&separe(" - ", true)), Origin::StandardConfirmed);
        assert_eq!(Origin::from_pattern(&separe(" - ", false)), Origin::LearnedDeviation);
        assert_eq!(Origin::from_pattern(&separe(" / ", true)), Origin::LearnedDeviation);
        assert_eq!(Origin::from_pattern(&Pattern::DoNotSplit), Origin::LearnedDeviation);
        assert_eq!(
            Origin::from_pattern(&separe_milieu(" - ")),
            Origin::LearnedDeviation,
            "« Artiste - Titre - Album » n'est pas le standard, meme avec son separator et son order"
        );
    }

    #[test]
    fn un_motif_pose_a_la_main_est_manuel_meme_sil_est_standard() {
        let mut m = Store::default();
        m.set_manual("http://f", separe(" - ", true));
        assert_eq!(m.entry("http://f").unwrap().origin, Origin::Manual);
    }

    #[test]
    fn apprendre_nefface_jamais_un_motif_manuel() {
        // La règle sur laquelle repose la confiance dans la page : sans elle,
        // le premier track après une correction de l'utilisateur la déferait
        // en silence.
        let mut m = Store::default();
        m.set_manual("http://f", separe(" / ", false));
        m.learn("http://f", separe(" - ", true));
        let e = m.entry("http://f").unwrap();
        assert_eq!(e.origin, Origin::Manual);
        assert_eq!(e.pattern, separe(" / ", false), "le pattern manuel doit survivre");
    }

    #[test]
    fn une_entree_existe_des_que_la_station_est_sondee_meme_conforme() {
        // L'invariant de stockage : « conforme » est une entrée, pas une
        // absence. L'absence confondrait « jamais sondée » et « vérifiée ».
        let mut m = Store::default();
        m.learn("http://f", separe(" - ", true));
        let e = m.entry("http://f").expect("une station conforme doit avoir son entry");
        assert_eq!(e.origin, Origin::StandardConfirmed);
    }

    #[test]
    fn les_succes_se_comptent_et_datent_lentree() {
        let mut m = Store::default();
        m.learn("http://f", separe(" - ", true));
        assert_eq!(m.entry("http://f").unwrap().split_titles, 0);
        m.record_success("http://f");
        m.record_success("http://f");
        assert_eq!(m.entry("http://f").unwrap().split_titles, 2);
        assert!(m.entry("http://f").unwrap().last_used.is_some());
    }

    #[test]
    fn un_fichier_illisible_donne_un_magasin_vide_et_non_une_erreur() {
        // Un état rejetable : on réapprend. Faire échouer le démarrage du
        // greffon pour un fichier de cache serait disproportionné.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.json");
        std::fs::write(&p, "{ ceci n'est pas du json").unwrap();
        assert!(Store::load(&p).entries().is_empty());
        assert!(Store::load(&dir.path().join("absent.json")).entries().is_empty());
    }

    #[test]
    fn un_aller_retour_sur_disque_conserve_tout() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sous").join("state.json");
        let mut m = Store::default();
        m.set_manual("http://a", separe(" – ", false));
        m.learn("http://b", Pattern::DoNotSplit);
        m.record_success("http://a");
        m.save(&p).unwrap();

        let relu = Store::load(&p);
        assert_eq!(relu.entry("http://a"), m.entry("http://a"));
        assert_eq!(relu.entry("http://b").unwrap().pattern, Pattern::DoNotSplit);
    }

    #[test]
    fn tout_vider_emporte_meme_les_motifs_manuels() {
        // Le point non évident, et il fallait le trancher : la protection d'un
        // pattern `Manual` vise le **réapprentissage automatique**, jamais un
        // geste explicite de l'utilisateur. Il a cliqué « tout vider » ; lui
        // laisser silencieusement ses corrections passées serait lui répondre à
        // côté, et il ne pourrait plus s'en débarrasser du tout.
        let mut m = Store::default();
        m.set_manual("http://a", separe(" / ", false));
        m.learn("http://b", separe(" - ", true));
        assert!(!m.is_empty());

        m.clear_all();
        assert!(m.is_empty(), "plus aucune station");
        assert!(m.entry("http://a").is_none(), "le manuel part aussi");
        assert!(m.entry("http://b").is_none());
    }

    #[test]
    fn supprimer_une_entree_la_rend_a_nouveau_sondable() {
        // Le geste de reprise pour une station classée « ne pas découper » :
        // rien ne la reprobe automatiquement, la suppression est le remède.
        let mut m = Store::default();
        m.learn("http://f", Pattern::DoNotSplit);
        m.remove("http://f");
        assert!(m.entry("http://f").is_none());
    }
}
