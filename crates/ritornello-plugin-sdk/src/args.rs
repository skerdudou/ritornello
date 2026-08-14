//! Ligne de commande d'un plugin, telle que le cœur la construit
//! (`plugins::spawn`) : `--socket <chemin>` toujours, `--admin-socket
//! <chemin>` si `admin = true`.
//!
//! Un seul exemplaire ici plutôt qu'une copie par binaire : la revue de
//! 2026-07-27 a compté six variantes de cette analyse dans les plugins, dont
//! une seule refusait proprement une option sans valeur — les cinq autres
//! paniquaient en « index out of bounds », anonyme, quand `--socket` était le
//! dernier argument.

use std::path::PathBuf;

/// Valeur de l'option `flag` dans `args` (forme `--flag <valeur>`).
///
/// Fonction pure pour être testable. `None` si l'option est absente ; panique
/// **en nommant l'option** si elle est présente sans valeur — ces lignes de
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

/// Chemin de la socket de genre (`--socket`), obligatoire pour tout plugin.
pub fn socket_path() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    arg_value(&args, "--socket").expect("--socket <path> required")
}

/// Chemin de la socket d'admin (`--admin-socket`), présent si le plugin est
/// déclaré `admin = true` dans `plugins.toml`.
pub fn admin_socket_path() -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    arg_value(&args, "--admin-socket")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn extrait_la_valeur_qui_suit_le_drapeau() {
        let a = args(&["plugin", "--socket", "/run/p.sock", "--admin-socket", "/run/a.sock"]);
        assert_eq!(arg_value(&a, "--socket"), Some(PathBuf::from("/run/p.sock")));
        assert_eq!(arg_value(&a, "--admin-socket"), Some(PathBuf::from("/run/a.sock")));
        assert_eq!(arg_value(&a, "--autre"), None);
    }

    #[test]
    fn drapeau_sans_valeur_panique_en_nommant_le_drapeau() {
        // C'était le défaut des cinq copies non robustes : « index out of
        // bounds » ne désigne rien.
        let a = args(&["plugin", "--socket"]);
        let e = std::panic::catch_unwind(|| arg_value(&a, "--socket")).unwrap_err();
        let msg = e.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(msg.contains("--socket"), "le message doit nommer l'option: {msg}");
    }
}
