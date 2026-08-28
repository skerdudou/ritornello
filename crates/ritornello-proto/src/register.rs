//! Line d'announcement d'un greffon, écrite sur le socket d'enregistrement du
//! cœur juste après que le greffon a lié ses propres sockets.
//!
//! L'order compte et il est structurel : les sockets sont liés par le
//! constructeur du SDK, l'announcement n'est écrite que par `Runtime::run`. Quand
//! le cœur read cette line, il sait donc à la fois quels genres existent et
//! que les sockets correspondants acceptent déjà une connexion.

use serde::{Deserialize, Serialize};

/// Ce qu'un greffon sait faire. Le kind est une propriété du **binaire**,
/// annoncée par lui, et non une line de configuration que l'opérateur
/// devait connaître (voir le même arbitrage rendition pour la page d'admin).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Source,
    Display,
    Input,
    /// Enrichit ce que plays la Source active sans que celle-ci le sache.
    ///
    /// **L'order compte** entre deux plugins `metadata` qui répondent pour le
    /// même track : le premier de `plugins.toml` gagne. Cet order vient
    /// désormais du manifest seul, l'announcement ne le porte pas — voir
    /// `ritornello-core::register`.
    Metadata,
}

/// Une announcement, une line de JSON, un greffon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    /// Repris tel quel de `--name`. Sert à corréler N annonces arrivant sur
    /// un socket unique ; l'autorité sur le name reste au manifest.
    pub name: String,
    pub kinds: Vec<PluginKind>,
    /// `false` par défaut : un greffon sans page d'admin peut omettre le champ.
    #[serde(default)]
    pub admin: bool,
    /// Cet afficheur veut-il recevoir les bytes des pochettes ?
    ///
    /// Même idiome que `admin` juste au-dessus, et pour la même raison :
    /// `false` par défaut, donc l'announcement la plus courante reste la plus courte
    /// à écrire, et un cœur d'avant ce champ relit sans rien y voir de nouveau.
    ///
    /// **Opt-in, et non un défaut** : une cover pèse jusqu'à
    /// `display::COVER_MAX_BYTES`, et un afficheur de vingt colonnes n'en a que
    /// faire. Le cœur ne push_cover les bytes qu'aux afficheurs qui ont demandé,
    /// plutôt que de les send_frame à tous en laissant chacun les jeter.
    ///
    /// Le drapeau est **dérivé** de ce que le greffon a enregistré, jamais
    /// demandé à l'appelant : voir `Runtime::display` dans le SDK, qui read
    /// `DisplayPlugin::wants_covers`. C'est l'invariant du protocol
    /// d'enregistrement — l'announcement ne peut pas mentir.
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
        let line = serde_json::to_string(&a).unwrap();
        assert_eq!(
            line,
            r#"{"name":"mpd","kinds":["input","display"],"admin":true,"covers":true}"#
        );
        assert_eq!(serde_json::from_str::<Announcement>(&line).unwrap(), a);
    }

    #[test]
    fn admin_absent_vaut_faux() {
        // Un greffon sans page peut omettre le champ : l'announcement la plus
        // courante doit rester la plus courte à écrire.
        let a: Announcement =
            serde_json::from_str(r#"{"name":"cd","kinds":["source"]}"#).unwrap();
        assert!(!a.admin);
        assert_eq!(a.kinds, vec![PluginKind::Source]);
    }

    #[test]
    fn covers_absent_vaut_faux() {
        // Le même idiome qu'`admin`, et la même conséquence : une announcement
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
        // pas absorbée en kind par défaut.
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
