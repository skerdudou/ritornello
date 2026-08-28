// Module ESM servi tant que l'IHM du plugin n'a pas ete construite.
//
// Inclus **textuellement** par `build.rs` (`include!`) autant que compile
// comme module du crate : c'est ce qui permet de tester la fabrication du
// bouchon par `cargo test`, alors que Cargo n'execute jamais les tests d'un
// script de build. Aucune dependance externe autorisee ici.
//
// Remarque : ce commentaire de module est volontairement un commentaire
// ordinaire (`//`) et non une doc interne (`//!`). Une doc interne provoque
// `E0753 : expected outer doc comment` une fois ce fichier inclus tel quel
// par `build.rs` via `include!` — la restriction du compilateur porte sur la
// position dans le stream de tokens du fichier hote, pas sur le fichier source
// tel qu'on le read ici (voir `ritornello-core/src/placeholder.rs`).

/// Marqueur reconnaissable dans les deux active de bouchon. Equivalent du
/// `MARKER` du coeur (`ritornello-core/src/placeholder.rs`), qui permet a
/// `build.rs` de distinguer un active de bouchon deja present d'un vrai
/// livrable.
pub const MARKER: &str = "ritornello-ihm-plugin-non-construite";

/// Contrat volontairement invalide : le shell affiche alors son message
/// « plugin à reconstruire », qui décrit exactement la situation.
pub fn ui_placeholder_js(commande: &str) -> String {
    format!(
        "// {MARKER}\n// IHM non construite. Lancer : {commande}\nexport const contract = -1;\n"
    )
}

/// Feuille de style de bouchon, porteuse du même marqueur : un `ui.css` de
/// bouchon laissé derrière un `ui.js` reconstruit donnerait une IHM sans
/// aucun style, autre dégradation silencieuse.
pub fn ui_placeholder_css() -> String {
    format!("/* {MARKER} : IHM non construite */\n")
}

/// Vrai si `contenu` est un active de bouchon plutôt qu'un vrai livrable.
///
/// Fonction **pure** du contenu, donc testable ici alors que Cargo n'exécute
/// jamais les tests d'un script de build. Elle existe parce que
/// `cargo::warning` n'était émis qu'à la **création** du bouchon : un
/// `cargo build` nu (bouchon créé, avertissement affiché une fois) suivi d'un
/// `cross build --release --target armv7…` — les scripts de build sont rejoués
/// par cible, mais `ui/dist/ui.js` existe désormais — ne disait plus rien, et
/// le binaire de release embarquait le bouchon en silence.
pub fn is_placeholder(contenu: &str) -> bool {
    contenu.contains(MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_bouchon_exporte_un_contrat_invalide_et_invite_a_construire_lihm() {
        let js = ui_placeholder_js("npm ci && npm run build --workspaces");
        assert!(js.contains("npm ci && npm run build --workspaces"));
        assert!(js.contains("export const contract = -1"));
        // Le marqueur est porte par un commentaire JS : le module reste
        // valide et chargeable, il announcement seulement un contrat invalide.
        assert!(js.starts_with("// "));
    }

    #[test]
    fn est_un_bouchon_reconnait_les_deux_actifs_de_bouchon() {
        assert!(is_placeholder(&ui_placeholder_js("npm ci")));
        assert!(is_placeholder(&ui_placeholder_css()));
    }

    #[test]
    fn est_un_bouchon_ne_se_declenche_pas_sur_un_vrai_livrable() {
        // Forme d'un `ui.js` reellement produit par Vite : imports externes
        // resolus par l'import map du shell, contrat valide, export par
        // defaut. Aucun marqueur, donc aucun avertissement.
        let vrai_js = "import{defineComponent as e}from\"vue\";\
                       import{api as t}from\"@ritornello/ui\";\
                       const o=e({});export const contract=1;export default o;\n";
        assert!(!is_placeholder(vrai_js));
        let vrai_css = ".space-y-6>:not([hidden])~:not([hidden]){margin-top:1.5rem}\n";
        assert!(!is_placeholder(vrai_css));
        assert!(!is_placeholder(""));
    }
}
