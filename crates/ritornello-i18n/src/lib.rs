//! Catalogue i18n partagé de ritornello.
//!
//! Deux couches indépendantes par composant :
//! - `own` : anglais embarqué du composant (`en.toml`), surchargé par le pack
//!   externe `<root>/<composant>/<lang>.toml`.
//! - `common` : anglais embarqué dans ce crate, surchargé par
//!   `<root>/common/<lang>.toml`.
//!
//! Résolution par clé : `own` → `common` → la clé elle-même (filet de secours).
//! Interpolation : le composant fait `catalog.get(key)` puis
//! `str::replace("{n}", &n.to_string())` (aucun moteur de template).

use std::collections::HashMap;
use std::path::Path;

/// Vocabulaire commun anglais embarqué dans le crate.
const COMMON_EN: &str = include_str!("locales/common_en.toml");

/// Parse pur d'un pack TOML plat (`clé = "valeur"`). Renvoie l'erreur de parse
/// pour l'appelant qui souhaite la logguer (chargement des couches de base).
pub fn try_parse(s: &str) -> Result<HashMap<String, String>, toml::de::Error> {
    toml::from_str(s)
}

/// Surcharge `base` avec le pack TOML lu sur disque en `path`. Fichier
/// **absent** : silencieux (cas normal — la plupart des composants n'ont pas de
/// pack pour la plupart des langues). Toute autre erreur — droits refusés,
/// UTF-8 invalide, TOML invalide — laisse `base` inchangée mais est **tracée** :
/// un pack présent que l'opérateur a voulu installer ne doit pas disparaître
/// sans une ligne de journal.
fn overlay_from_disk(base: &mut HashMap<String, String>, path: &Path) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!("pack i18n {} ignoré (lecture impossible): {e}", path.display());
            return;
        }
    };
    match toml::from_str::<HashMap<String, String>>(&text) {
        Ok(ext) => base.extend(ext),
        Err(e) => tracing::warn!("pack i18n {} ignoré (TOML invalide): {e}", path.display()),
    }
}

pub struct Catalog {
    own: HashMap<String, String>,
    common: HashMap<String, String>,
}

impl Catalog {
    /// Construit le catalogue d'un composant pour une langue donnée.
    /// Part de l'anglais embarqué (`own_en` pour `own`, `COMMON_EN` pour
    /// `common`), puis superpose les packs externes présents et valides.
    /// Jamais de panique : un pack absent ou invalide laisse l'anglais.
    pub fn load(component: &str, locale: &str, root: &Path, own_en: &str) -> Catalog {
        let mut own = match try_parse(own_en) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("pack embarque {component} invalide: {e}");
                HashMap::new()
            }
        };
        let mut common = match try_parse(COMMON_EN) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("pack embarque common invalide: {e}");
                HashMap::new()
            }
        };
        overlay_from_disk(&mut common, &root.join("common").join(format!("{locale}.toml")));
        overlay_from_disk(&mut own, &root.join(component).join(format!("{locale}.toml")));
        Catalog { own, common }
    }

    /// Résout une clé : `own` → `common` → la clé elle-même.
    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.own
            .get(key)
            .or_else(|| self.common.get(key))
            .map(String::as_str)
            .unwrap_or(key)
    }

    /// Carte plate de **toutes** les clés connues, `own` surchargeant
    /// `common` — même ordre de priorité que `get`, mais exposé d'un bloc.
    ///
    /// Sert à livrer le catalogue au navigateur (`GET /api/i18n`) : la SPA
    /// résout ses clés côté client, ce qui remplace la substitution `{{clé}}`
    /// d'autrefois. Les valeurs restent des **données** de bout en bout :
    /// aucun caractère n'est dangereux, contrairement à la substitution brute
    /// dans du source JS.
    pub fn entries(&self) -> HashMap<&str, &str> {
        let mut out: HashMap<&str, &str> =
            self.common.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        for (k, v) in &self.own {
            out.insert(k.as_str(), v.as_str());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Écrit `<root>/<sous_dossier>/<fichier>` et retourne le TempDir racine.
    fn ecrire(dir: &std::path::Path, sous_dossier: &str, fichier: &str, contenu: &str) {
        let d = dir.join(sous_dossier);
        std::fs::create_dir_all(&d).unwrap();
        let mut f = std::fs::File::create(d.join(fichier)).unwrap();
        f.write_all(contenu.as_bytes()).unwrap();
    }

    #[test]
    fn own_prime_sur_common() {
        let dir = tempfile::tempdir().unwrap();
        // own_en définit "error", common l'a aussi : own doit gagner.
        let cat = Catalog::load("core", "en", dir.path(), "error = \"own-error\"\n");
        assert_eq!(cat.get("error"), "own-error");
    }

    #[test]
    fn externe_surcharge_l_embarque_own() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), "core", "fr.toml", "standby = \"VEILLE\"\n");
        let cat = Catalog::load("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        assert_eq!(cat.get("standby"), "VEILLE");
    }

    #[test]
    fn externe_surcharge_l_embarque_common() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), "common", "fr.toml", "error = \"Erreur\"\n");
        let cat = Catalog::load("core", "fr", dir.path(), "");
        assert_eq!(cat.get("error"), "Erreur");
    }

    #[test]
    fn cle_manquante_repli_anglais_puis_cle_elle_meme() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::load("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        // pas de pack fr : on garde l'anglais embarqué
        assert_eq!(cat.get("standby"), "STANDBY");
        // clé inconnue : on renvoie la clé elle-même
        assert_eq!(cat.get("inconnue"), "inconnue");
    }

    #[test]
    fn toml_invalide_est_ignore_sans_paniquer() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), "core", "fr.toml", "ceci = n'est pas valide");
        let cat = Catalog::load("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        assert_eq!(cat.get("standby"), "STANDBY"); // repli anglais, pas de panique
    }

    #[test]
    fn try_parse_du_common_en_embarque_est_non_vide() {
        assert!(!try_parse(COMMON_EN).unwrap().is_empty());
    }

    #[test]
    fn try_parse_renvoie_err_sur_toml_invalide() {
        assert!(try_parse("ceci n'est pas du toml =").is_err());
    }

    #[test]
    fn entries_fusionne_own_par_dessus_common() {
        let dir = tempfile::tempdir().unwrap();
        // `error` existe dans le common embarque : `own` doit primer, comme
        // dans `get`.
        let cat = Catalog::load("core", "en", dir.path(), "error = \"own-error\"\nautre = \"x\"\n");
        let e = cat.entries();
        assert_eq!(e.get("error").copied(), Some("own-error"));
        assert_eq!(e.get("autre").copied(), Some("x"));
        // Les cles du common non redefinies sont presentes : la carte est
        // complete, c'est elle qui alimente `t()` cote navigateur.
        assert!(e.len() > 1);
        assert!(e.keys().any(|k| *k == "play"), "le vocabulaire commun doit etre inclus");
    }

    /// Pack `common` français livré dans le dépôt. Même invariant de parité que
    /// pour chaque composant (voir `core.rs::parite_des_cles_entre_len_embarque_et_le_pack_fr`),
    /// qui manquait à la couche commune : rien ne signalait qu'une clé ajoutée
    /// dans `common_en.toml` n'avait pas de traduction française.
    fn pack_common_fr() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/locales/common/fr.toml");
        std::fs::read_to_string(p).expect("pack common fr livre")
    }

    #[test]
    fn parite_des_cles_entre_le_common_embarque_et_le_pack_fr() {
        let en = try_parse(COMMON_EN).unwrap();
        let fr = try_parse(&pack_common_fr()).unwrap();
        let mut cles_en: Vec<&String> = en.keys().collect();
        let mut cles_fr: Vec<&String> = fr.keys().collect();
        cles_en.sort();
        cles_fr.sort();
        assert_eq!(cles_en, cles_fr, "jeux de cles common en/fr divergents");
    }

    #[test]
    fn les_cles_de_chargement_dihm_de_plugin_vivent_dans_la_couche_commune() {
        // Ces trois clés sont affichées par le shell de la SPA
        // (`web/app/src/views/PluginView.ts`). Elles doivent vivre dans
        // `common` — héritée par TOUS les catalogues — et non dans celui du
        // cœur : le shell les résout d'abord dans le catalogue **du plugin**,
        // qui est vide précisément quand le plugin est injoignable, le cas même
        // qui produit `plugin_unavailable`.
        let dir = tempfile::tempdir().unwrap();
        // Catalogue d'un plugin dont le `own` ne définit rien : les trois clés
        // doivent quand même se résoudre, et jamais renvoyer la clé elle-même.
        let cat = Catalog::load("radio", "en", dir.path(), "");
        for cle in ["loading", "plugin_unavailable", "plugin_contract_mismatch"] {
            assert_ne!(cat.get(cle), cle, "cle {cle} absente du vocabulaire commun");
            // `entries()` est ce qui part vers le navigateur : la clé doit y
            // être, sinon le `t()` de la SPA retombe sur la clé brute.
            assert!(cat.entries().contains_key(cle), "cle {cle} absente de entries()");
        }
    }

    #[test]
    fn entries_reflete_les_surcharges_externes() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), "core", "fr.toml", "standby = \"VEILLE\"\n");
        let cat = Catalog::load("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        assert_eq!(cat.entries().get("standby").copied(), Some("VEILLE"));
    }
}
