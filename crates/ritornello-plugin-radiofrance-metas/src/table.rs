//! Correspondance entre l'URL d'un stream et l'identifiant de station.
//!
//! La table est **embarquée** dans le binaire (`stations.toml`), relevée de la
//! documentation publique de l'Open API de Radio France, où chaque station
//! porte à la fois son `liveStream` (donc son mount Icecast) et son `playerUrl`
//! (qui contains `id_station=<n>`). `scripts/fetch-stations.mjs` la régénère.
//!
//! Elle n'est **pas** relue au démarrage depuis le réseau : un appareil qui
//! démarre sans surveillance ne doit pas dépendre d'une page tierce pour
//! reconnaître ses stations, et l'échec d'une telle playback serait silencieux.
//! Une table embarquée échoue, elle, de façon reproductible et corrigible.
//!
//! Un fichier de configuration reste consulté **en premier** : il permet de
//! corriger une entrée devenue fausse ou d'en ajouter une, sans recompiler.

use serde::Deserialize;
use std::path::Path;

/// Table livrée avec le binaire.
const EMBEDDED: &str = include_str!("stations.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct Station {
    /// Libellé, pour les logs et la lisibilité du fichier. Jamais affiché.
    #[serde(default)]
    pub label: String,
    /// Mounts qui désignent cette station, cherchés comme **jetons** de l'URL
    /// configurée dans `stations.toml` (voir `contains_token`).
    ///
    /// Un mount et non l'URL entière : Radio France sert la même station sous
    /// au moins trois formes — `icecast.radiofrance.fr/<mount>-midfi.mp3`, le
    /// name historique `direct.fipradio.fr/live/<mount>-midfi.mp3` (qui redirige
    /// vers le premier, donc celui que les annuaires référencent), et le HLS
    /// `stream.radiofrance.fr/<mount>/<mount>.m3u8` — sans compter les qualités
    /// (`-lofi`, `-hifi.aac`). Le mount est la seule partie commune.
    pub mounts: Vec<String>,
    /// Identifiant attendu par le point d'entrée du direct.
    pub id: u32,
    /// Profil de rendition à demander pour cette station (dernier segment de
    /// l'URL du direct, voir `live::live_url`).
    ///
    /// Deux valeurs seulement, et le choix n'est pas cosmétique : c'est lui qui
    /// décide si le plugin dit quelque chose. `webrf_fip_player` sur Mouv'
    /// renvoie le slogan de la station et rien d'autre.
    ///
    /// La valeur par défaut sert les entrées écrites à la main dans le fichier
    /// de l'opérateur : c'est le profil des stations musicales, le plus
    /// probable pour une station qu'on prendrait la peine d'ajouter.
    #[serde(default = "default_profile")]
    pub rules: String,
}

/// Profil retenu quand une entrée n'en déclare pas.
fn default_profile() -> String {
    "webrf_fip_player".to_string()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Table {
    #[serde(default, rename = "station")]
    pub stations: Vec<Station>,
}

/// Vrai si `mount` apparaît dans `url` comme **jeton entier**, c'est-à-dire
/// bordé de part et d'autre par un caractère non alphanumérique (ou par le
/// bord de la chaîne).
///
/// La recherche par sous-chaîne simple ne convient pas ici : `fip` est un
/// préfixe de `fipgroove`, `francemusique` de `francemusiquebaroque`, et la
/// première entrée rencontrée capturerait toutes les autres en affichant les
/// titres de la mauvaise station, sans aucun signe. La règle de bord règle le
/// cas une fois pour toutes et laisse une entrée par station, au lieu d'obliger
/// à choisir des fragments assez longs pour ne pas s'avaler entre eux.
///
/// Elle traite bien les trois formes d'URL : `/fip-midfi.mp3` (bordé par `/` et
/// `-`), `/fip/fip.m3u8` (par `/` et `/`, puis `/` et `.`), `fip_midfi.m3u8`
/// (par `/` et `_`) — et refuse `fipradio.fr` comme `fipgroove-midfi.mp3`.
pub fn contains_token(url: &str, mount: &str) -> bool {
    if mount.is_empty() {
        return false;
    }
    let bytes = url.as_bytes();
    url.match_indices(mount).any(|(debut, _)| {
        let fin = debut + mount.len();
        let avant_libre = debut == 0 || !bytes[debut - 1].is_ascii_alphanumeric();
        let apres_libre = fin >= bytes.len() || !bytes[fin].is_ascii_alphanumeric();
        avant_libre && apres_libre
    })
}

impl Table {
    /// Table effective : les entrées de l'opérateur d'abord, puis celles
    /// embarquées.
    ///
    /// Cet order donne les deux usages d'un coup, sans deuxième réglage :
    /// **corriger** une entrée devenue fausse (le même mount déclaré dans le
    /// fichier gagne, la recherche s'arrêtant au premier accord) et **ajouter**
    /// une station absente de la table livrée.
    ///
    /// File absent : cas normal, aucun avertissement — la table embarquée
    /// suffit. File illisible ou invalide : avertissement, et on continue
    /// avec la seule table embarquée plutôt que de priver l'appareil de tout.
    pub fn load(path: &Path) -> Self {
        let mut stations = Vec::new();
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Self>(&text) {
                Ok(t) => {
                    tracing::info!("{} station(s) declared in {}", t.stations.len(), path.display());
                    stations.extend(t.stations);
                }
                Err(e) => tracing::warn!("{} is invalid ({e}): bundled table only", path.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("{} is unreadable ({e}): bundled table only", path.display()),
        }
        stations.extend(Self::embedded().stations);
        Self { stations }
    }

    /// Table embarquée seule. Une table livrée illisible serait un défaut de
    /// compilation du plugin, pas une erreur d'exploitation : d'où le `expect`,
    /// verrouillé par un test.
    pub fn embedded() -> Self {
        toml::from_str(EMBEDDED).expect("table de stations embedded valide")
    }

    /// Station correspondant à cette URL de stream, s'il y en a une. Premier
    /// accord, dans l'order de la table.
    pub fn station_for(&self, url: &str) -> Option<&Station> {
        self.stations.iter().find(|s| s.mounts.iter().any(|m| contains_token(url, m)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_jeton_respecte_ses_bords() {
        assert!(contains_token("https://icecast.radiofrance.fr/fip-midfi.mp3", "fip"));
        assert!(contains_token("https://stream.radiofrance.fr/fip/fip.m3u8", "fip"));
        assert!(contains_token("https://stream.radiofrance.fr/fip/fip_midfi.m3u8", "fip"));
        assert!(contains_token("https://direct.fipradio.fr/live/fip-midfi.mp3", "fip"));
        // Le cœur du problème : un préfixe ne doit pas capturer les autres.
        assert!(!contains_token("https://icecast.radiofrance.fr/fipgroove-midfi.mp3", "fip"));
        assert!(!contains_token("https://direct.fipradio.fr/live/fipgroove-midfi.mp3", "fip"));
        assert!(!contains_token("https://icecast.radiofrance.fr/francemusiquebaroque-midfi.mp3", "francemusique"));
        // Un mount clear ne correspond à rien (sans quoi il correspondrait à tout).
        assert!(!contains_token("https://icecast.radiofrance.fr/fip-midfi.mp3", ""));
    }

    #[test]
    fn un_caractere_non_ascii_fait_bord() {
        // Une URL peut porter n'importe quoi ; la règle ne doit pas paniquer
        // sur un octet de continuation UTF-8 au bord du jeton.
        assert!(contains_token("https://exemple.test/é/fip/é", "fip"));
    }

    #[test]
    fn aucun_mount_nen_avale_un_autre() {
        // Invariant décisif : si le mount d'une entrée était reconnu comme
        // jeton dans une URL construite sur le mount d'une autre, la première
        // rencontrée capturerait les deux stations et afficherait les titres de
        // la mauvaise, sans aucun signe.
        let t = Table::embedded();
        for a in &t.stations {
            for b in &t.stations {
                if a.id == b.id {
                    continue;
                }
                for ma in &a.mounts {
                    for mb in &b.mounts {
                        let url = format!("https://icecast.radiofrance.fr/{mb}-midfi.mp3");
                        assert!(
                            !contains_token(&url, ma),
                            "« {ma} » ({}) capture l'URL de « {mb} » ({})",
                            a.label,
                            b.label
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn chaque_station_porte_un_profil_connu() {
        // Un profil inconnu ne fait pas échouer la requête : le serveur répond,
        // mais sans rien de plus que le slogan de la station. La panne serait
        // donc silencieuse, d'où ce verrou.
        let t = Table::embedded();
        for s in &t.stations {
            assert!(
                matches!(s.rules.as_str(), "webrf_fip_player" | "webrf_mouv_player"),
                "{}: profil inattendu {:?}",
                s.label,
                s.rules
            );
        }
        // Les stations musicales dont la réponse sépare titre et artiste.
        for (id, attendu) in [
            (7, "webrf_fip_player"),
            (66, "webrf_fip_player"),
            (411, "webrf_fip_player"),
            // Mouv' et les locales : mesuré, seul ce profil sort le track.
            (6, "webrf_mouv_player"),
            (12, "webrf_mouv_player"),
            (1, "webrf_mouv_player"),
            (4, "webrf_mouv_player"),
        ] {
            let s = t.stations.iter().find(|s| s.id == id).unwrap();
            assert_eq!(s.rules, attendu, "station {id} ({})", s.label);
        }
    }

    #[test]
    fn une_entree_sans_profil_prend_celui_par_defaut() {
        // Le fichier de l'opérateur doit rester écrivable à la main, sans
        // connaître ce champ.
        let t: Table = toml::from_str("[[station]]\nmounts = [\"x\"]\nid = 1\n").unwrap();
        assert_eq!(t.stations[0].rules, "webrf_fip_player");
    }

    #[test]
    fn la_table_embarquee_est_valide_et_complete() {
        // `embedded()` panique sur une table cassée : ce test est ce qui fait
        // échouer la compilation logique du plugin plutôt que son démarrage.
        let t = Table::embedded();
        assert_eq!(t.stations.len(), 74, "6 marques + 12 webradios FIP + 11 France Musique + 45 locales");
        let mut ids = std::collections::HashSet::new();
        let mut mounts = std::collections::HashSet::new();
        for s in &t.stations {
            assert!(!s.label.is_empty(), "station {} sans libelle", s.id);
            assert!(!s.mounts.is_empty(), "{}: aucun mount", s.label);
            assert!(s.mounts.iter().all(|m| !m.is_empty()), "{}: mount clear", s.label);
            assert!(s.id > 0, "{}: identifiant nul", s.label);
            assert!(ids.insert(s.id), "{}: identifiant {} en double", s.label, s.id);
            for m in &s.mounts {
                assert!(mounts.insert(m.clone()), "{}: mount {m} en double", s.label);
                // Les mounts relevés sont alphanumériques ; un `-` ou un `.`
                // signalerait qu'une URL entière a été recopiée par erreur, et
                // la règle de bord ne s'appliquerait plus comme prévu.
                assert!(
                    m.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                    "{}: mount {m} inattendu",
                    s.label
                );
            }
        }
    }

    #[test]
    fn reconnait_les_trois_formes_durl_dune_meme_station() {
        // L'URL enregistrée par l'opérateur peut venir d'un annuaire, du site
        // ou d'un player : seul le mount est commun aux trois.
        let t = Table::embedded();
        for url in [
            "https://icecast.radiofrance.fr/fipgroove-midfi.mp3",
            "https://icecast.radiofrance.fr/fipgroove-hifi.aac",
            "https://direct.fipradio.fr/live/fipgroove-midfi.mp3",
            "https://stream.radiofrance.fr/fipgroove/fipgroove.m3u8",
        ] {
            assert_eq!(t.station_for(url).map(|s| s.id), Some(66), "{url}");
        }
    }

    #[test]
    fn les_grandes_stations_et_les_locales_sont_reconnues() {
        let t = Table::embedded();
        for (url, attendu) in [
            ("https://icecast.radiofrance.fr/franceinter-midfi.mp3", 1),
            ("https://icecast.radiofrance.fr/franceinfo-midfi.mp3", 2),
            ("https://icecast.radiofrance.fr/francemusique-midfi.mp3", 4),
            ("https://icecast.radiofrance.fr/franceculture-lofi.mp3", 5),
            ("https://icecast.radiofrance.fr/mouv-midfi.mp3", 6),
            ("https://icecast.radiofrance.fr/fip-midfi.mp3", 7),
            // Locale dont le mount ne ressemble pas à son name d'antenne.
            ("https://icecast.radiofrance.fr/fbfrequenzamora-midfi.mp3", 11),
            ("https://icecast.radiofrance.fr/fb1071-midfi.mp3", 68),
            // Webradio France Musique dont le mount est aussi trompeur.
            ("https://icecast.radiofrance.fr/francemusiquelabo-midfi.mp3", 407),
        ] {
            let s = t.station_for(url).unwrap_or_else(|| panic!("{url} non reconnue"));
            assert_eq!(s.id, attendu, "{url} -> {}", s.label);
        }
    }

    #[test]
    fn une_url_inconnue_ne_correspond_a_rien() {
        // Cas le plus courant : toute autre station configurée sur l'appareil.
        let t = Table::embedded();
        assert!(t.station_for("https://ouifm3.ice.infomaniak.ch/ouifm3.mp3").is_none());
        assert!(t.station_for("https://somafm.com/groovesalad256.pls").is_none());
        // Le site de Radio France n'est pas un stream, mais il porte les mêmes
        // mots : cette URL-là n'a rien à faire dans `stations.toml`, et si elle
        // y était, reconnaître la station serait encore le moindre mal.
        assert!(t.station_for("https://www.radiofrance.fr/").is_none());
    }

    #[test]
    fn le_fichier_de_loperateur_est_consulte_avant_la_table_embarquee() {
        // Les deux usages du fichier : corriger une entrée devenue fausse, et
        // en ajouter une absente de la table livrée.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("radiofrance-metas.toml");
        std::fs::write(
            &p,
            "[[station]]\nlabel = \"correction\"\nmounts = [\"fip\"]\nid = 999\n\n\
             [[station]]\nlabel = \"ajout\"\nmounts = [\"nouveauflux\"]\nid = 123\n",
        )
        .unwrap();
        let t = Table::load(&p);
        let url = "https://icecast.radiofrance.fr/fip-midfi.mp3";
        assert_eq!(t.station_for(url).map(|s| s.id), Some(999), "correction");
        assert_eq!(t.station_for("https://x/nouveauflux.mp3").map(|s| s.id), Some(123), "ajout");
        // Le reste de la table embarquée continue de répondre.
        assert_eq!(
            t.station_for("https://icecast.radiofrance.fr/fipreggae-midfi.mp3").map(|s| s.id),
            Some(71),
            "Reggae toujours connue"
        );
    }

    #[test]
    fn fichier_absent_laisse_la_table_embarquee_intacte() {
        let dir = tempfile::tempdir().unwrap();
        let t = Table::load(&dir.path().join("absent.toml"));
        assert_eq!(t.stations.len(), Table::embedded().stations.len());
    }

    #[test]
    fn fichier_invalide_laisse_la_table_embarquee_intacte() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rf.toml");
        std::fs::write(&p, "ceci n'est pas du toml [[[").unwrap();
        assert_eq!(Table::load(&p).stations.len(), Table::embedded().stations.len());
    }

    #[test]
    fn un_mount_vide_ne_correspond_pas_a_tout() {
        // Sans la garde de `contains_token`, une entrée mal renseignée ferait
        // interroger cette station pour **toutes** les URL.
        let t: Table = toml::from_str("[[station]]\nmounts = [\"\"]\nid = 1\n").unwrap();
        assert!(t.station_for("https://icecast.radiofrance.fr/fip-midfi.mp3").is_none());
        let clear: Table = toml::from_str("[[station]]\nmounts = []\nid = 1\n").unwrap();
        assert!(clear.station_for("https://x/y").is_none());
    }

    #[test]
    fn le_fichier_dexemple_livre_est_valide() {
        // Il est destiné à être copié tel quel sur l'appareil : s'il ne se
        // chargeait pas, la panne serait silencieuse.
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/radiofrance-metas.example.toml");
        let text = std::fs::read_to_string(&p).expect("exemple livre");
        toml::from_str::<Table>(&text).expect("exemple valide");
    }
}
