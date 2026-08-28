//! Correspondance entre l'URL d'un stream et l'identifiant de métadonnées.
//!
//! La table est **embarquée** dans le binaire (`webradios.toml`), relevée de la
//! source de vérité d'OUI FM : la variable JavaScript `apidata` de la page du
//! player, où chaque stream porte son identifiant de stream (`id`) et son
//! identifiant de métadonnées (`idMds`). `scripts/fetch-webradios.mjs` la
//! régénère.
//!
//! Elle n'est **pas** relue au démarrage depuis le site : la liste ne vit que
//! dans une page HTML, et une extraction par expression régulière sur une page
//! qu'un tiers refond quand il veut est trop fragile pour un appareil qui doit
//! démarrer sans surveillance — son échec serait silencieux, et l'appareil
//! perdrait les titres sans rien dire. Une table embarquée échoue, elle, de
//! façon reproductible et corrigible.
//!
//! Un fichier de configuration reste consulté **en premier** : il permet de
//! corriger une entrée devenue fausse ou d'en ajouter une, sans recompiler.

use serde::Deserialize;
use std::path::Path;

/// Table livrée avec le binaire.
const EMBEDDED: &str = include_str!("webradios.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct Webradio {
    /// Libellé, pour les logs et la lisibilité du fichier. Jamais affiché.
    #[serde(default)]
    pub label: String,
    /// Fragments d'URL qui désignent cette webradio, cherchés comme
    /// **sous-chaînes** de l'URL configurée dans `stations.toml`.
    ///
    /// Des sous-chaînes et non l'URL entière : l'URL de diffusion porte un jeton
    /// signé et un paramètre de format qui varient (`?format=hd`, `sd`, `hls`),
    /// mais elle contains toujours l'identifiant de stream.
    ///
    /// Plusieurs fragments parce qu'une même webradio se diffuse sous **deux
    /// formes d'URL** : celle de `streams.lesindesradios.fr` (celle que le site
    /// emploie aujourd'hui) et le mount Icecast historique
    /// (`ouifm3.ice.infomaniak.ch/ouifm3.mp3`). C'est la seconde qu'on rencontre
    /// en pratique — publiée de longue date, donc référencée par les annuaires et
    /// recopiée par les utilisateurs. N'en connaître qu'une revenait à ne
    /// reconnaître aucune station ajoutée normalement.
    pub urls: Vec<String>,
    /// Identifiant attendu par `?id=` du stream de métadonnées (`idMds` chez OUI
    /// FM). **Distinct de `stream`** : vérifié à la main, l'identifiant de stream
    /// donne une trame clear, sans artiste ni titre.
    pub metas: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Table {
    #[serde(default, rename = "webradio")]
    pub webradios: Vec<Webradio>,
}

impl Table {
    /// Table effective : les entrées de l'opérateur d'abord, puis celles
    /// embarquées.
    ///
    /// Cet order donne les deux usages d'un coup, sans deuxième réglage :
    /// **corriger** une entrée devenue fausse (la même `stream` déclarée dans le
    /// fichier gagne, la recherche s'arrêtant au premier accord) et **ajouter**
    /// un stream absent de la table livrée.
    ///
    /// File absent : cas normal, aucun avertissement — la table embarquée
    /// suffit. File illisible ou invalide : avertissement, et on continue
    /// avec la seule table embarquée plutôt que de priver l'appareil de tout.
    pub fn load(path: &Path) -> Self {
        let mut webradios = Vec::new();
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Self>(&text) {
                Ok(t) => {
                    tracing::info!("{} webradio(s) declared in {}", t.webradios.len(), path.display());
                    webradios.extend(t.webradios);
                }
                Err(e) => tracing::warn!("{} invalid ({e}): embedded table only", path.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("{} unreadable ({e}): embedded table only", path.display()),
        }
        webradios.extend(Self::embedded().webradios);
        Self { webradios }
    }

    /// Table embarquée seule. Une table livrée illisible serait un défaut de
    /// compilation du plugin, pas une erreur d'exploitation : d'où le `expect`,
    /// verrouillé par un test.
    pub fn embedded() -> Self {
        toml::from_str(EMBEDDED).expect("valid embedded webradio table")
    }

    /// Webradio correspondant à cette URL de stream, s'il y en a une. Premier
    /// accord, dans l'order de la table.
    pub fn metas_for(&self, url: &str) -> Option<&Webradio> {
        self.webradios
            .iter()
            .find(|w| w.urls.iter().any(|f| !f.is_empty() && url.contains(f.as_str())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// URL de stream réelle, telle qu'OUI FM la sert (jeton signé compris).
    const URL_CLASSIC_ROCK: &str = "https://streams.lesindesradios.fr/play/radios/oui-fm/3qhtSltZ27/any/300/11d46a.NND%2BFTMcarOrumMD%2FJU7lENzKQUNWno%2FSz7wPrtsPIw%3D?format=hd";

    #[test]
    fn aucun_fragment_nen_avale_un_autre() {
        // Invariant décisif : si un fragment d'une entrée était contenu dans un
        // fragment d'une autre, la première rencontrée capturerait les deux
        // stations et afficherait les titres de la mauvaise, sans aucun signe.
        // Un fragment trop court (`ouifm` au lieu de `ouifm3.`) suffirait à
        // provoquer exactement cela.
        let t = Table::embedded();
        for a in &t.webradios {
            for b in &t.webradios {
                if a.metas == b.metas {
                    continue;
                }
                for fa in &a.urls {
                    for fb in &b.urls {
                        assert!(
                            !fb.contains(fa.as_str()),
                            "« {fa} » ({}) est contenu dans « {fb} » ({})",
                            a.label,
                            b.label
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn les_mounts_historiques_sont_dans_la_table() {
        // Ce sont les URL publiées de longue date, donc celles qu'un annuaire
        // référence et qu'un utilisateur copie : sans elles, une station OUI FM
        // ajoutée normalement n'était reconnue par aucune entrée.
        let t = Table::embedded();
        for (url, attendu) in [
            ("https://ouifm.ice.infomaniak.ch/ouifm-high.mp3", "2174546520932614531"),
            ("https://ouifm3.ice.infomaniak.ch/ouifm3.mp3", "3134161803443976427"),
            ("https://ouifm2.ice.infomaniak.ch/ouifm2.mp3", "3134161803443976382"),
            ("https://ouifm5.ice.infomaniak.ch/ouifm5.mp3", "3134161803443976526"),
        ] {
            let w = t.metas_for(url).unwrap_or_else(|| panic!("{url} non reconnue"));
            assert_eq!(w.metas, attendu, "{url} -> {}", w.label);
        }
    }

    #[test]
    fn la_table_embarquee_est_valide_et_complete() {
        // `embedded()` panique sur une table cassée : ce test est ce qui fait
        // échouer la compilation logique du plugin plutôt que son démarrage.
        let t = Table::embedded();
        assert!(t.webradios.len() >= 20, "21 stream releves, {} trouves", t.webradios.len());
        for w in &t.webradios {
            assert!(!w.urls.is_empty(), "{}: aucun fragment d'URL", w.label);
            assert!(w.urls.iter().all(|u| !u.is_empty()), "{}: fragment clear", w.label);
            assert!(!w.metas.is_empty(), "{}: identifiant de metadata clear", w.label);
            // Les deux identifiants sont de natures différentes chez OUI FM (un
            // jeton alphanumérique court, un grand nombre décimal) : les
            // confondre donnerait une trame clear, sans artiste ni titre, et sans
            // aucun signe d'erreur. Vérifié à la main sur le stream réel.
            assert!(
                !w.urls.contains(&w.metas),
                "{}: identifiant de metadata employe comme fragment d'URL",
                w.label
            );
            assert!(
                w.metas.chars().all(|c| c.is_ascii_digit()),
                "{}: `metas` doit etre un identifiant mds numerique, trouve {:?}",
                w.label,
                w.metas
            );
        }
    }

    #[test]
    fn reconnait_une_url_de_flux_reelle() {
        let t = Table::embedded();
        let w = t.metas_for(URL_CLASSIC_ROCK).expect("Classic Rock reconnue");
        assert_eq!(w.metas, "3134161803443976427");
        assert_eq!(w.label, "Oüi FM Classic Rock");
    }

    #[test]
    fn reconnait_la_meme_station_quel_que_soit_le_format_ou_le_jeton() {
        // L'URL enregistrée par l'opérateur peut différer de celle relevée : le
        // jeton est signé et le format se choisit. Seul l'identifiant de stream est
        // stable, et c'est sur lui que porte la reconnaissance.
        let t = Table::embedded();
        for url in [
            "https://streams.lesindesradios.fr/play/radios/oui-fm/3qhtSltZ27/any/300/autre-jeton?format=sd",
            "https://streams.lesindesradios.fr/play/radios/oui-fm/3qhtSltZ27/any/300/x?format=hls",
            "http://exemple.test/3qhtSltZ27",
        ] {
            assert_eq!(t.metas_for(url).map(|w| w.metas.as_str()), Some("3134161803443976427"), "{url}");
        }
    }

    #[test]
    fn une_url_inconnue_ne_correspond_a_rien() {
        // Cas le plus courant : toute autre station configurée sur l'appareil.
        let t = Table::embedded();
        assert!(t.metas_for("http://icecast.radiofrance.fr/fip-midfi.mp3").is_none());
    }

    #[test]
    fn le_fichier_de_loperateur_est_consulte_avant_la_table_embarquee() {
        // Les deux usages du fichier : corriger une entrée devenue fausse, et en
        // ajouter une absente de la table livrée.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ouifm-metas.toml");
        std::fs::write(
            &p,
            "[[webradio]]\nlabel = \"correction\"\nurls = [\"3qhtSltZ27\"]\nmetas = \"999\"\n\n\
             [[webradio]]\nlabel = \"ajout\"\nurls = [\"nouveau-stream\"]\nmetas = \"123\"\n",
        )
        .unwrap();
        let t = Table::load(&p);
        assert_eq!(t.metas_for(URL_CLASSIC_ROCK).map(|w| w.metas.as_str()), Some("999"), "correction");
        assert_eq!(t.metas_for("http://x/nouveau-stream").map(|w| w.metas.as_str()), Some("123"), "ajout");
        // Le reste de la table embarquée continue de répondre.
        assert!(t.metas_for("http://x/fkYz8mdU3T").is_some(), "Rock Inde toujours connue");
    }

    #[test]
    fn fichier_absent_laisse_la_table_embarquee_intacte() {
        let dir = tempfile::tempdir().unwrap();
        let t = Table::load(&dir.path().join("absent.toml"));
        assert_eq!(t.webradios.len(), Table::embedded().webradios.len());
    }

    #[test]
    fn fichier_invalide_laisse_la_table_embarquee_intacte() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ouifm.toml");
        std::fs::write(&p, "ceci n'est pas du toml [[[").unwrap();
        assert_eq!(Table::load(&p).webradios.len(), Table::embedded().webradios.len());
    }

    #[test]
    fn un_fragment_vide_ne_correspond_pas_a_tout() {
        // Sans cette garde, `"".contains` etant toujours vrai, une entree mal
        // renseignee ferait interroger ce stream pour **toutes** les stations.
        let t: Table = toml::from_str("[[webradio]]\nurls = [\"\"]\nmetas = \"1\"\n").unwrap();
        assert!(t.metas_for("http://icecast.radiofrance.fr/fip-midfi.mp3").is_none());
        // Et une entree sans aucun fragment ne correspond a rien non plus.
        let clear: Table = toml::from_str("[[webradio]]\nurls = []\nmetas = \"1\"\n").unwrap();
        assert!(clear.metas_for("http://x/y").is_none());
    }

    #[test]
    fn le_fichier_dexemple_livre_est_valide() {
        // Il est destiné à être copié tel quel sur l'appareil : s'il ne se
        // chargeait pas, la panne serait silencieuse.
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/ouifm-metas.example.toml");
        let text = std::fs::read_to_string(&p).expect("exemple livre");
        toml::from_str::<Table>(&text).expect("exemple valide");
    }
}
