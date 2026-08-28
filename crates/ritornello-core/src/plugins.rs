use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Un greffon sans mention est **active** : aucun `plugins.toml` en service ne
/// change de sens en winner cette clé, et « pas de clé = allumé » reste vrai
/// des deux côtés — `set_enabled` retire la clé au lieu d'écrire `true`.
fn enabled_by_default() -> bool {
    true
}

/// Une entrée de `plugins.toml` : quoi lancer, sous quel name. Rien d'autre.
///
/// Ni le kind ni la page d'admin n'y sont déclarés : ce sont des propriétés
/// du **binaire**, que celui-ci announcement lui-même sur le socket
/// d'enregistrement du cœur. L'opérateur n'a plus à les connaître, et leur
/// oubli ne peut plus produire de mode dégradé silencieux.
///
/// **L'order du fichier reste porteur** : c'est lui qui arbitre entre deux
/// plugins annonçant le kind `metadata` (voir `crate::register`).
#[derive(Debug, Clone, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    pub exec: String,
    /// Greffon lancé au démarrage et câblé, ou laissé éteint. Bascule depuis
    /// l'IHM d'admin (`PUT /api/plugins/:name/enabled`), persistée ici.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PluginManifest {
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<PluginConfig>,
}

impl PluginManifest {
    /// Un fichier **absent** donne un manifest clear : le cœur démarre sans
    /// plugin plutôt que d'échouer (cohérent avec le traitement déjà
    /// existant pour `stations.toml`). Toute autre erreur d'E/S est remontée,
    /// comme un TOML invalide : un `plugins.toml` présent mais illisible
    /// (droits) qui donnerait « aucune source disponible » enverrait le
    /// diagnostic dans la mauvaise direction.
    ///
    /// Un `name` dupliqué n'est ni rejeté ni dédoublonné ici, seulement
    /// signalé (voir `duplicate_names`) : c'était le contournement employé
    /// avant ce chantier pour faire serve deux genres à un même binaire, et
    /// un appareil en service peut encore le porter.
    pub fn load(path: &Path) -> Result<Self> {
        let manifest: Self = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        for name in duplicate_names(&manifest.plugins) {
            tracing::warn!(
                "plugin name '{name}' appears more than once in plugins.toml: a single \
                 announcement satisfies both entries, and the second connection is wired \
                 twice, left hanging in a backlog nobody accepts"
            );
        }
        Ok(manifest)
    }
}

/// Bascule la clé `enabled` du greffon `name` dans le fichier, en place.
///
/// Désactiver pose `enabled = false` ; réactiver **retire la clé** plutôt que
/// d'écrire `true`, pour qu'un fichier tout allumé n'en porte aucune et que
/// « pas de mention = allumé » reste vrai des deux côtés.
///
/// Un name non déclaré est une erreur et **ne réécrit rien** : c'est ce qui
/// permet à la couche HTTP de refuser avant d'agir.
pub fn set_enabled(path: &Path, name: &str, enabled: bool) -> Result<()> {
    let texte = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut =
        texte.parse().with_context(|| format!("parsing {}", path.display()))?;
    let blocs = doc
        .get_mut("plugin")
        .and_then(|item| item.as_array_of_tables_mut())
        .ok_or_else(|| anyhow::anyhow!("no [[plugin]] entry in {}", path.display()))?;
    let bloc = blocs
        .iter_mut()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
        .ok_or_else(|| anyhow::anyhow!("plugin '{name}' is not declared in {}", path.display()))?;
    if enabled {
        bloc.remove("enabled");
    } else {
        bloc["enabled"] = toml_edit::value(false);
    }
    write_atomic(path, &doc.to_string())
}

/// Écrit par fichier temporaire voisin puis `rename` — atomique sur un même
/// système de fichiers, et l'idiome déjà employé pour les fichiers de
/// configuration écrits par le greffon `files`.
///
/// Un `plugins.toml` tronqué par une coupure de courant — un appareil qu'on
/// débranche — ne laisserait plus rien se lancer au démarrage suivant.
fn write_atomic(path: &Path, contenu: &str) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, contenu).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        // L'erreur qui vaut la peine d'être remontée est celle du `rename`
        // (cible immuable, système de fichiers hostile au renommage) : un
        // nettoyage qui échouerait à son tour ne doit pas la masquer, donc on
        // ignore son résultat. Best-effort : ne pas laisser traîner le
        // fichier temporaire plutôt qu'échouer sur autre chose que ce qu'on
        // rapporte.
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("renaming onto {}", path.display()));
    }
    Ok(())
}

/// Noms de `plugin.name` apparaissant plus d'une fois dans `plugins`, chacun
/// une seule fois, dans l'order de leur première duplication.
///
/// Fonction pure pour être testable : `PluginManifest::load` s'en sert pour
/// nommer chaque doublon dans un `tracing::warn!`, sans rien rejeter ni
/// dédoublonner — un doublon de déclaration reste silencieusement câblé deux
/// fois aujourd'hui, la seconde connexion pendant dans un backlog que
/// personne n'accepte.
fn duplicate_names(plugins: &[PluginConfig]) -> Vec<String> {
    let mut vus = std::collections::HashSet::new();
    let mut doublons = Vec::new();
    for p in plugins {
        if !vus.insert(p.name.as_str()) && !doublons.iter().any(|d| d == &p.name) {
            doublons.push(p.name.clone());
        }
    }
    doublons
}

/// Rase et recrée `{runtime_dir}/sockets`, et rend son path.
///
/// Un répertoire neuf à chaque démarrage rend les fichiers rances
/// **impossibles** au lieu de reposer sur une pré-suppression au cas par cas :
/// un socket laissé par une exécution précédente est connectable, et le cœur
/// dialoguerait avec un zombie ou attendrait un `ECONNREFUSED` retenté. Une
/// seule instance du cœur par `runtime_dir` — garanti par `RuntimeDirectory=`
/// de systemd en service, par une variable distincte en développement.
pub fn prepare_sockets_dir(runtime_dir: &Path) -> Result<PathBuf> {
    let dir = runtime_dir.join("sockets");
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("clearing {}", dir.display()));
        }
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// Lance un greffon en lui disant où s'annoncer, sous quel name, et avec quel
/// préfixe de sockets.
///
/// Aucune pré-suppression de fichier ici : `prepare_sockets_dir` a rasé le
/// répertoire entier avant le premier lancement.
///
/// `locale` transmet la langue courante via `RITORNELLO_LOCALE`, appliquée
/// **au lancement** seulement (inchangé).
pub fn spawn(
    exec: &str,
    register: &Path,
    name: &str,
    prefix: &Path,
    locale: Option<&str>,
) -> Result<tokio::process::Child> {
    let mut cmd = tokio::process::Command::new(exec);
    cmd.arg("--register").arg(register);
    cmd.arg("--name").arg(name);
    cmd.arg("--socket-prefix").arg(prefix);
    if let Some(locale) = locale {
        cmd.env("RITORNELLO_LOCALE", locale);
    }
    // Le path est nommé dans l'erreur : « No such file or directory » seul
    // laisse deviner **lequel** des chemins de `plugins.toml` est en cause, et
    // la confusion la plus courante est justement là — un `exec` de déploiement
    // (`/usr/local/lib/...`) recopié dans une configuration de développement,
    // où les binaires sont sous `target/debug/`.
    cmd.kill_on_drop(true).spawn().with_context(|| format!("executable {exec}"))
}

/// Temps laissé à un greffon entre `SIGTERM` et `SIGKILL`.
///
/// Deux secondes : aucun greffon n'a de nettoyage à faire aujourd'hui, et la
/// bascule vient d'une page web qui attend la réponse.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Termine un greffon : `SIGTERM`, puis `SIGKILL` s'il s'attarde au-delà de
/// `grace`.
///
/// `SIGTERM` d'abord, comme pour mpv (`system.rs`) : c'est le signal qu'un
/// greffon pourra un jour intercepter pour rendre une console ou éteindre un
/// écran. Aucun ne le fait, et le défaut de Rust le terminate aussitôt — mais
/// tuer d'entrée interdirait cette politesse pour toujours.
///
/// Rend le statut de sortie, jamais une attente sans fin : c'est tout l'objet
/// de la retombée sur `SIGKILL`, qu'aucun processus ne peut masquer.
pub async fn terminate(
    child: &mut tokio::process::Child,
    grace: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    if let Some(pid) = child.id() {
        // SAFETY : le `Child` est encore vivant ici, donc le processus n'a pas
        // été moissonné et son pid n'a pas pu être réattribué à un autre.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(statut) => statut,
        Err(_) => {
            tracing::warn!("plugin ignored SIGTERM, sending SIGKILL");
            child.kill().await?;
            child.wait().await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_un_manifeste_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            r#"
[[plugin]]
name = "radio"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"

[[plugin]]
name = "console"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-console"
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 2);
        assert_eq!(m.plugins[0].name, "radio");
        assert_eq!(m.plugins[1].name, "console");
    }

    #[test]
    fn un_manifeste_sans_kind_se_charge() {
        // Le kind est desormais announcement par le binaire : le fichier ne le
        // porte plus.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            r#"
[[plugin]]
name = "radio"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 1);
        assert_eq!(m.plugins[0].name, "radio");
    }

    #[test]
    fn le_repertoire_de_sockets_est_neuf_a_chaque_demarrage() {
        // Un fichier rance d'une execution precedente est connectable et
        // ferait dialoguer le coeur avec un zombie : le repertoire est donc
        // rase, pas nettoye au cas par cas.
        let dir = tempfile::tempdir().unwrap();
        let sockets = dir.path().join("sockets");
        std::fs::create_dir_all(&sockets).unwrap();
        let rance = sockets.join("radio-source.sock");
        std::fs::write(&rance, "").unwrap();

        let rendition = prepare_sockets_dir(dir.path()).unwrap();
        assert_eq!(rendition, sockets);
        assert!(rendition.is_dir(), "le repertoire doit exister apres l'appel");
        assert!(!rance.exists(), "le fichier rance doit avoir disparu");
    }

    #[test]
    fn un_manifeste_absent_est_vide_mais_illisible_est_une_erreur() {
        // Absent = installation sans plugin, cas normal. Illisible (ici : le
        // « répertoire » parent est en réalité un fichier) = un problème à
        // nommer — « aucune source disponible » enverrait le diagnostic dans
        // la mauvaise direction.
        let dir = tempfile::tempdir().unwrap();
        let absent = PluginManifest::load(&dir.path().join("plugins.toml")).unwrap();
        assert!(absent.plugins.is_empty());
        let bouchon = dir.path().join("pas-un-repertoire");
        std::fs::write(&bouchon, "").unwrap();
        assert!(PluginManifest::load(&bouchon.join("plugins.toml")).is_err());
    }

    #[test]
    fn une_erreur_de_lancement_nomme_toujours_lexecutable() {
        let dir = tempfile::tempdir().unwrap();
        let e = spawn(
            "/path/qui/nexiste/pas/ritornello-plugin-bidon",
            &dir.path().join("register.sock"),
            "bidon",
            &dir.path().join("bidon"),
            None,
        )
        .expect_err("un executable absent doit echouer");
        let message = format!("{e:#}");
        assert!(
            message.contains("/path/qui/nexiste/pas/ritornello-plugin-bidon"),
            "l'erreur doit nommer l'executable cherche: {message}"
        );
    }

    #[test]
    fn manifeste_absent_donne_liste_vide() {
        let dir = tempfile::tempdir().unwrap();
        let m = PluginManifest::load(&dir.path().join("absent.toml")).unwrap();
        assert!(m.plugins.is_empty());
    }

    #[test]
    fn detecte_un_nom_duplique_sans_le_rejeter_ni_le_dedoublonner() {
        // Le doublon de name était le contournement d'avant ce chantier pour
        // faire serve deux genres à un même binaire : un manifest qui le
        // porte doit charger tel quel (les deux entrées), pas échouer.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            r#"
[[plugin]]
name = "mpd"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-mpd"

[[plugin]]
name = "mpd"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-mpd"

[[plugin]]
name = "radio"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 3, "le doublon n'est pas dedoublonne au chargement");
        assert_eq!(duplicate_names(&m.plugins), vec!["mpd".to_string()]);
    }

    #[test]
    fn aucun_nom_duplique_ne_signale_rien() {
        let plugins = vec![
            PluginConfig { name: "radio".into(), exec: "radio".into(), enabled: true },
            PluginConfig { name: "files".into(), exec: "files".into(), enabled: true },
        ];
        assert!(duplicate_names(&plugins).is_empty());
    }

    #[test]
    fn enabled_absent_vaut_actif_et_false_est_lu() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            "[[plugin]]\nname = \"radio\"\nexec = \"/bin/true\"\n\n\
             [[plugin]]\nname = \"cd\"\nexec = \"/bin/true\"\nenabled = false\n",
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        // Un `plugins.toml` en service ne porte pas la clé : il doit continuer à
        // tout lancer.
        assert!(m.plugins[0].enabled, "sans mention, un greffon est active");
        assert!(!m.plugins[1].enabled);
    }

    /// Un manifest commenté comme celui du déploiement : c'est ce que la
    /// réécriture doit rendre intact.
    fn manifeste_commente() -> &'static str {
        "# Le tuner web.\n\
         [[plugin]]\n\
         name = \"radio\"\n\
         exec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-radio\"\n\
         \n\
         # Les métadonnées : l'order de ce fichier arbitre.\n\
         [[plugin]]\n\
         name = \"musicbrainz\"\n\
         exec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-musicbrainz\"\n"
    }

    #[test]
    fn desactiver_pose_la_cle_sans_toucher_aux_commentaires() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(&path, manifeste_commente()).unwrap();

        set_enabled(&path, "radio", false).unwrap();

        let apres = std::fs::read_to_string(&path).unwrap();
        assert!(apres.contains("# Le tuner web."), "commentaire de tête perdu");
        assert!(
            apres.contains("# Les métadonnées : l'order de ce fichier arbitre."),
            "commentaire du second bloc perdu"
        );
        let m = PluginManifest::load(&path).unwrap();
        assert!(!m.plugins[0].enabled);
        assert!(m.plugins[1].enabled, "le voisin n'a pas bougé");
        // L'order du fichier arbitre les `metadata` : le réécrire ne doit pas le
        // permuter.
        assert_eq!(m.plugins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["radio", "musicbrainz"]);
    }

    #[test]
    fn reactiver_retire_la_cle_au_lieu_decrire_true() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(&path, manifeste_commente()).unwrap();

        set_enabled(&path, "radio", false).unwrap();
        set_enabled(&path, "radio", true).unwrap();

        let apres = std::fs::read_to_string(&path).unwrap();
        // « Pas de mention = allumé » doit rester vrai des deux côtés : un
        // fichier tout allumé ne porte aucune clé.
        assert!(!apres.contains("enabled"), "la clé aurait dû disparaître : {apres}");
        assert!(PluginManifest::load(&path).unwrap().plugins[0].enabled);
    }

    #[test]
    fn un_nom_non_declare_est_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(&path, manifeste_commente()).unwrap();

        let avant = std::fs::read_to_string(&path).unwrap();
        assert!(set_enabled(&path, "inconnu", false).is_err());
        // Refus **sans effet de bord** : le fichier n'est pas réécrit.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), avant);
    }

    #[test]
    fn aucun_fichier_temporaire_ne_survit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(&path, manifeste_commente()).unwrap();

        set_enabled(&path, "radio", false).unwrap();

        let restes: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(restes, ["plugins.toml"], "un fichier temporaire est resté");
    }

    #[test]
    fn eteint_puis_rallume_le_greffon_retrouve_sa_place_dans_le_fichier() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(&path, manifeste_commente()).unwrap();

        set_enabled(&path, "musicbrainz", false).unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert!(m.plugins[0].enabled, "le voisin reste allumé");
        assert!(!m.plugins[1].enabled);

        set_enabled(&path, "musicbrainz", true).unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert!(m.plugins.iter().all(|p| p.enabled), "tout est rallumé");
        // L'order du fichier arbitre les `metadata` : un greffon rallumé doit
        // reprendre sa priorité d'origine, pas la queue de liste.
        assert_eq!(
            m.plugins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["radio", "musicbrainz"]
        );
        // Et le fichier est revenu à sa forme d'origine, commentaires compris.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), manifeste_commente());
    }

    #[tokio::test]
    async fn termine_arrete_un_processus_qui_dormait() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let statut = terminate(&mut child, SHUTDOWN_GRACE).await.unwrap();
        // Terminé par signal : pas de code de sortie nul.
        assert!(!statut.success(), "le processus aurait dû être terminé : {statut:?}");
    }

    #[tokio::test]
    async fn termine_insiste_quand_sigterm_est_ignore() {
        // Un greffon qui masque SIGTERM ne doit pas pouvoir retenir l'extinction.
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; sleep 30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        // Grâce courte : le test mesure la **retombée** sur SIGKILL, pas un délai.
        let statut = terminate(&mut child, std::time::Duration::from_millis(200)).await.unwrap();
        assert!(!statut.success(), "SIGKILL aurait dû avoir raison de lui : {statut:?}");
    }
}
