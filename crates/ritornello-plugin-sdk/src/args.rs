//! Line de commande d'un plugin, telle que le cœur la construit
//! (`plugins::spawn`) : `--register <path>`, `--name <name>`, et
//! `--socket-prefix <préfixe>`, obligatoires.
//!
//! Un seul exemplaire ici plutôt qu'une copie par binaire : la revue de
//! 2026-07-27 a compté six variantes de cette analyse dans les plugins, dont
//! une seule refusait proprement une option sans valeur — les cinq autres
//! paniquaient en « index out of bounds », anonyme, quand `--socket` était le
//! dernier argument.

use std::path::PathBuf;
use ritornello_proto::PluginKind;

/// Valeur de l'option `flag` dans `args` (forme `--flag <valeur>`).
///
/// Fonction pure pour être testable. `None` si l'option est absente ; panique
/// **en nommant l'option** si elle est présente sans valeur — ces lines de
/// commande sont construites par le cœur, une valeur manquante est un bug de
/// montage à désigner clairement.
pub fn arg_value(args: &[String], flag: &str) -> Option<PathBuf> {
    args.iter().position(|a| a == flag).map(|i| {
        let valeur = args
            .get(i + 1)
            .unwrap_or_else(|| panic!("{flag} requires a value (no argument after {flag})"));
        PathBuf::from(valeur)
    })
}

/// Chemin du socket d'enregistrement du cœur (`--register`), obligatoire.
pub fn register_socket() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    arg_value(&args, "--register").expect("--register <path> required")
}

/// Nom sous lequel le cœur connaît ce greffon (`--name`), obligatoire.
///
/// Le greffon le **renvoie** dans son announcement sans jamais l'inventer : c'est
/// le manifest qui a autorité, sinon deux binaires pourraient réclamer le
/// même name et collisionner sur les chemins de sockets.
pub fn plugin_name() -> String {
    let args: Vec<String> = std::env::args().collect();
    arg_value(&args, "--name")
        .expect("--name <name> required")
        .to_string_lossy()
        .into_owned()
}

/// Préfixe des sockets que ce greffon doit lier (`--socket-prefix`).
///
/// Le cœur garde la maîtrise du répertoire et du préfixe ; le greffon n'a
/// autorité que sur les suffixes, qui sont exactement ce qu'il announcement.
pub fn socket_prefix() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    arg_value(&args, "--socket-prefix").expect("--socket-prefix <path> required")
}

/// `{prefixe}-{kind}.sock`.
pub fn socket_kind(prefix: &std::path::Path, kind: PluginKind) -> PathBuf {
    let kind = match kind {
        PluginKind::Source => "source",
        PluginKind::Display => "display",
        PluginKind::Input => "input",
        PluginKind::Metadata => "metadata",
    };
    PathBuf::from(format!("{}-{kind}.sock", prefix.display()))
}

/// `{prefixe}-admin.sock`.
pub fn admin_socket(prefix: &std::path::Path) -> PathBuf {
    PathBuf::from(format!("{}-admin.sock", prefix.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn extrait_la_valeur_qui_suit_le_drapeau() {
        let a = args(&["plugin", "--register", "/run/register.sock", "--name", "radio"]);
        assert_eq!(arg_value(&a, "--register"), Some(PathBuf::from("/run/register.sock")));
        assert_eq!(arg_value(&a, "--name"), Some(PathBuf::from("radio")));
        assert_eq!(arg_value(&a, "--autre"), None);
    }

    #[test]
    fn drapeau_sans_valeur_panique_en_nommant_le_drapeau() {
        // C'était le défaut des cinq copies non robustes : « index out of
        // bounds » ne désigne rien.
        let a = args(&["plugin", "--socket-prefix"]);
        let e = std::panic::catch_unwind(|| arg_value(&a, "--socket-prefix")).unwrap_err();
        let msg = e.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(msg.contains("--socket-prefix"), "le message doit nommer l'option: {msg}");
    }

    #[test]
    fn extrait_les_trois_options_du_nouveau_montage() {
        let a = args(&[
            "plugin",
            "--register", "/run/ritornello/sockets/register.sock",
            "--name", "radio",
            "--socket-prefix", "/run/ritornello/sockets/radio",
        ]);
        assert_eq!(
            arg_value(&a, "--register"),
            Some(PathBuf::from("/run/ritornello/sockets/register.sock"))
        );
        assert_eq!(arg_value(&a, "--name"), Some(PathBuf::from("radio")));
        assert_eq!(
            arg_value(&a, "--socket-prefix"),
            Some(PathBuf::from("/run/ritornello/sockets/radio"))
        );
    }

    #[test]
    fn suffixe_un_prefixe_par_genre_et_par_admin() {
        let p = PathBuf::from("/run/ritornello/sockets/radio");
        assert_eq!(
            super::socket_kind(&p, ritornello_proto::PluginKind::Source),
            PathBuf::from("/run/ritornello/sockets/radio-source.sock")
        );
        assert_eq!(
            super::admin_socket(&p),
            PathBuf::from("/run/ritornello/sockets/radio-admin.sock")
        );
    }
}
