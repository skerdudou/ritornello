//! Le magasin de motifs appris, un par station : quel découpage vérifier une
//! fois, et retenir. Le format ICY est une propriété de la **station**, pas du
//! morceau, donc l'unité de mémorisation est l'URL du flux, sondée une fois
//! puis rejouée sans réseau.
//!
//! Deux énumérations, pas une : [`Motif`] dit **ce que c'est** — découper sur
//! tel séparateur dans tel ordre, ou ne pas découper — et [`Origine`] dit
//! **comment on l'a su** — standard confirmé, déviation apprise, ou manuel.
//! Les confondre mettrait « ne pas découper » parmi les origines, et rendrait
//! un « ne pas découper » posé à la main indistinguable d'un appris. La règle
//! selon laquelle le réapprentissage n'écrase **jamais** un motif manuel a
//! précisément besoin de cette distinction : sans elle, le premier morceau
//! après une correction de l'utilisateur la déferait en silence.

use crate::icy::Candidat;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// **Quoi** : comment découper la chaîne annoncée par une station — ou ne pas
/// la découper du tout.
///
/// `NePasDecouper` fait partie du *quoi*, pas du *comment* : c'est une forme
/// de découpage à part entière (l'absence de découpage), au même titre que
/// `Separe`. La confondre avec une origine empêcherait de poser « ne pas
/// découper » à la main et de le distinguer d'un « ne pas découper » subi.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Motif {
    Separe { separateur: String, artiste_en_premier: bool },
    NePasDecouper,
}

impl Motif {
    /// Le motif que décrit ce candidat validé.
    ///
    /// L'inverse de [`crate::icy::candidats`] : celui-ci dérive les
    /// découpages plausibles depuis une chaîne, `depuis_candidat` retient
    /// lequel a validé, pour le rejouer sans réseau la prochaine fois.
    pub fn depuis_candidat(c: &Candidat) -> Motif {
        Motif::Separe { separateur: c.separateur.to_string(), artiste_en_premier: c.artiste_en_premier }
    }
}

/// **Comment on l'a su** : d'où vient le motif retenu pour une station.
///
/// Jamais posée librement à côté d'un motif quelconque : voir
/// [`Origine::depuis_motif`], qui porte l'invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origine {
    /// Le séparateur standard (`" - "`), artiste en premier : la convention
    /// de fait des automates de diffusion, confirmée par une requête.
    StandardConfirme,
    /// Tout le reste qu'un sondage a appris : un autre séparateur, l'ordre
    /// inverse, ou l'absence de découpage validée par élimination.
    DeviationApprise,
    /// Posé depuis la page d'admin. Rien ne le réapprend jamais.
    Manuel,
}

impl Origine {
    /// Dérive l'origine que peut porter ce motif.
    ///
    /// L'invariant du magasin : `StandardConfirme` ne s'apparie qu'au
    /// standard exact. Laisser les deux champs libres autoriserait un
    /// « standard confirmé » qui ne découpe pas, ou qui découpe dans l'ordre
    /// inverse — que rien ne rattraperait ensuite, puisque `apprend` fait
    /// confiance à l'origine déjà posée pour savoir si elle peut réécrire.
    pub fn depuis_motif(motif: &Motif) -> Origine {
        match motif {
            Motif::Separe { separateur, artiste_en_premier: true } if separateur == " - " => {
                Origine::StandardConfirme
            }
            _ => Origine::DeviationApprise,
        }
    }
}

/// Ce que le magasin retient pour une station.
///
/// Une entrée existe dès que la station a été sondée, même si le résultat est
/// conforme au standard : l'absence confondrait « jamais sondée » et
/// « vérifiée », deux états que l'appelant doit pouvoir distinguer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entree {
    pub url: String,
    pub motif: Motif,
    pub origine: Origine,
    /// ISO-8601 UTC, pas un type de date : ce dépôt n'a pas de crate de date,
    /// la valeur ne sert qu'à trier et à afficher, et la produire depuis
    /// `SystemTime` évite une dépendance.
    #[serde(default)]
    pub dernier_usage: Option<String>,
    #[serde(default)]
    pub titres_decoupes: u64,
}

/// Le magasin, indexé par URL de flux et persisté en JSON.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Magasin {
    stations: Vec<Entree>,
}

impl Magasin {
    /// Charge le magasin depuis le disque.
    ///
    /// Un fichier absent ou illisible rend un magasin vide plutôt qu'une
    /// erreur : un état rejetable pour un simple cache se réapprend, il ne
    /// doit pas empêcher le greffon de démarrer.
    pub fn charge(path: &Path) -> Magasin {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// Écrit le magasin sur le disque, atomiquement.
    ///
    /// Nom temporaire propre à ce processus **et** à cet appel : un `.tmp`
    /// partagé permettrait à deux écritures simultanées de se voler le
    /// fichier sous le pied (`rename` en ENOENT). Même motif que
    /// `ritornello-plugin-radio/src/state.rs`.
    pub fn enregistre(&self, path: &Path) -> Result<()> {
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
    pub fn entree(&self, url: &str) -> Option<&Entree> {
        self.stations.iter().find(|e| e.url == url)
    }

    /// Toutes les entrées, pour la page d'admin.
    pub fn entrees(&self) -> &[Entree] {
        &self.stations
    }

    /// Aucune station sondée.
    pub fn vide(&self) -> bool {
        self.stations.is_empty()
    }

    /// Pose le motif appris d'un sondage.
    ///
    /// Si l'entrée existante est `Manuel`, ne fait **rien** : c'est la règle
    /// sur laquelle repose la confiance dans la page d'admin. Sans elle, le
    /// premier morceau après une correction de l'utilisateur la déferait en
    /// silence.
    pub fn apprend(&mut self, url: &str, motif: Motif) {
        if let Some(e) = self.stations.iter_mut().find(|e| e.url == url) {
            if e.origine == Origine::Manuel {
                tracing::debug!("motif manuel conserve pour {url}, apprentissage ignore");
                return;
            }
            e.origine = Origine::depuis_motif(&motif);
            e.motif = motif;
            return;
        }
        self.stations.push(Entree {
            url: url.to_string(),
            origine: Origine::depuis_motif(&motif),
            motif,
            dernier_usage: None,
            titres_decoupes: 0,
        });
    }

    /// Pose un motif à la main, depuis la page d'admin : toujours `Manuel`,
    /// même quand le motif posé est le standard.
    pub fn pose_manuel(&mut self, url: &str, motif: Motif) {
        if let Some(e) = self.stations.iter_mut().find(|e| e.url == url) {
            e.motif = motif;
            e.origine = Origine::Manuel;
            return;
        }
        self.stations.push(Entree {
            url: url.to_string(),
            motif,
            origine: Origine::Manuel,
            dernier_usage: None,
            titres_decoupes: 0,
        });
    }

    /// Compte un titre découpé avec succès, et date l'entrée.
    pub fn succes(&mut self, url: &str) {
        let Some(e) = self.stations.iter_mut().find(|e| e.url == url) else {
            tracing::debug!("succes signale pour {url}, sans entree correspondante");
            return;
        };
        e.titres_decoupes += 1;
        e.dernier_usage = Some(maintenant_iso8601());
    }

    /// Retire l'entrée d'une station.
    ///
    /// Le geste de reprise pour une station classée « ne pas découper » :
    /// rien ne la resonde automatiquement, la suppression est le remède.
    pub fn supprime(&mut self, url: &str) {
        self.stations.retain(|e| e.url != url);
    }
}

/// Horodatage courant, ISO-8601 UTC.
///
/// Pas de crate de date dans ce dépôt : cette valeur ne sert qu'à trier et à
/// afficher, jamais à un calcul calendaire applicatif. La conversion jours →
/// année/mois/jour est l'algorithme de Howard Hinnant (`civil_from_days`), le
/// classique qui évite la dépendance.
fn maintenant_iso8601() -> String {
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
        // le premier morceau après une correction de l'utilisateur la déferait
        // en silence.
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
