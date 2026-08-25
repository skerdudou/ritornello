//! Ligne d'annonce d'un greffon, écrite sur le socket d'enregistrement du
//! cœur juste après que le greffon a lié ses propres sockets.
//!
//! L'ordre compte et il est structurel : les sockets sont liés par le
//! constructeur du SDK, l'annonce n'est écrite que par `Runtime::run`. Quand
//! le cœur lit cette ligne, il sait donc à la fois quels genres existent et
//! que les sockets correspondants acceptent déjà une connexion.

use serde::{Deserialize, Serialize};

/// Ce qu'un greffon sait faire. Le genre est une propriété du **binaire**,
/// annoncée par lui, et non une ligne de configuration que l'opérateur
/// devait connaître (voir le même arbitrage rendu pour la page d'admin).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Source,
    Display,
    Input,
    /// Enrichit ce que joue la Source active sans que celle-ci le sache.
    ///
    /// **L'ordre compte** entre deux plugins `metadata` qui répondent pour le
    /// même morceau : le premier de `plugins.toml` gagne. Cet ordre vient
    /// désormais du manifeste seul, l'annonce ne le porte pas — voir
    /// `ritornello-core::register`.
    Metadata,
}

/// Une annonce, une ligne de JSON, un greffon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    /// Repris tel quel de `--name`. Sert à corréler N annonces arrivant sur
    /// un socket unique ; l'autorité sur le nom reste au manifeste.
    pub name: String,
    pub kinds: Vec<PluginKind>,
    /// `false` par défaut : un greffon sans page d'admin peut omettre le champ.
    #[serde(default)]
    pub admin: bool,
    /// Cet afficheur veut-il recevoir les octets des pochettes ?
    ///
    /// Même idiome que `admin` juste au-dessus, et pour la même raison :
    /// `false` par défaut, donc l'annonce la plus courante reste la plus courte
    /// à écrire, et un cœur d'avant ce champ relit sans rien y voir de nouveau.
    ///
    /// **Opt-in, et non un défaut** : une pochette pèse jusqu'à
    /// `display::COVER_MAX_BYTES`, et un afficheur de vingt colonnes n'en a que
    /// faire. Le cœur ne pousse les octets qu'aux afficheurs qui ont demandé,
    /// plutôt que de les envoyer à tous en laissant chacun les jeter.
    ///
    /// Le drapeau est **dérivé** de ce que le greffon a enregistré, jamais
    /// demandé à l'appelant : voir `Runtime::display` dans le SDK, qui lit
    /// `DisplayPlugin::wants_covers`. C'est l'invariant du protocole
    /// d'enregistrement — l'annonce ne peut pas mentir.
    #[serde(default)]
    pub covers: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_genres_se_serialisent_en_minuscules() {
        let a = Announcement {
            name: "mpd".into(),
            kinds: vec![PluginKind::Input, PluginKind::Display],
            admin: true,
            covers: true,
        };
        let ligne = serde_json::to_string(&a).unwrap();
        assert_eq!(
            ligne,
            r#"{"name":"mpd","kinds":["input","display"],"admin":true,"covers":true}"#
        );
        assert_eq!(serde_json::from_str::<Announcement>(&ligne).unwrap(), a);
    }

    #[test]
    fn admin_absent_vaut_faux() {
        // Un greffon sans page peut omettre le champ : l'annonce la plus
        // courante doit rester la plus courte à écrire.
        let a: Announcement =
            serde_json::from_str(r#"{"name":"cd","kinds":["source"]}"#).unwrap();
        assert!(!a.admin);
        assert_eq!(a.kinds, vec![PluginKind::Source]);
    }

    #[test]
    fn covers_absent_vaut_faux() {
        // Le même idiome qu'`admin`, et la même conséquence : une annonce
        // écrite avant ce champ — celle de la console, celle d'un greffon
        // externe — se relit sans erreur et **sans** demander de pochettes.
        // C'est ce qui protège l'afficheur de vingt colonnes.
        let a: Announcement =
            serde_json::from_str(r#"{"name":"console","kinds":["display"],"admin":false}"#).unwrap();
        assert!(!a.covers, "l'absence du champ ne doit jamais valoir un opt-in");
    }

    #[test]
    fn un_genre_inconnu_est_une_erreur_pas_un_silence() {
        // Une faute de frappe dans un binaire de greffon doit être rapportée,
        // pas absorbée en genre par défaut.
        assert!(serde_json::from_str::<Announcement>(r#"{"name":"x","kinds":["sourec"]}"#).is_err());
    }

    #[test]
    fn plusieurs_genres_survivent_a_l_aller_retour() {
        let a = Announcement {
            name: "double".into(),
            kinds: vec![PluginKind::Source, PluginKind::Metadata],
            admin: false,
            covers: false,
        };
        let retour: Announcement =
            serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(retour, a);
    }
}
