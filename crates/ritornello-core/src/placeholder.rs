// Page servie quand l'IHM n'a pas été construite.
//
// Ce fichier est inclus **textuellement** par `build.rs` (`include!`) autant
// qu'il est compilé comme module du crate : c'est ce qui permet de tester la
// fabrication du bouchon par `cargo test`, alors que Cargo n'exécute jamais
// les tests d'un script de build. Il ne doit donc dépendre d'**aucune**
// crate externe.
//
// Remarque : ce commentaire de module est volontairement un commentaire
// ordinaire (`//`) et non une doc interne (`//!`). Une doc interne provoque
// `E0753 : expected outer doc comment` une fois ce fichier inclus tel quel
// par `build.rs` via `include!` — la restriction du compilateur porte sur la
// position dans le flux de tokens du fichier hôte, pas sur le fichier source
// tel qu'on le lit ici.

/// Marqueur reconnaissable dans la page de bouchon.
pub const MARQUEUR: &str = "ritornello-ihm-non-construite";

/// HTML minimal, sans dépendance, qui explique quoi lancer. Mieux qu'une
/// erreur de macro `include_str!` sur un clone frais : `cargo build` et
/// `cargo test` restent verts sans Node installé, et le message est explicite.
pub fn placeholder_html(commande: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>ritornello</title></head><body id=\"{MARQUEUR}\">\
         <h1>ritornello</h1>\
         <p>Web interface not built. Run:</p><pre>{commande}</pre>\
         </body></html>"
    )
}

/// Vrai si `contenu` est la page de bouchon plutôt qu'un vrai livrable.
///
/// Fonction **pure** du contenu, donc testable ici comme le reste de ce
/// fichier, alors que Cargo n'exécute jamais les tests d'un script de build.
///
/// Elle existe pour `build.rs` : `cargo::warning` n'était émis qu'à la
/// **création** du bouchon. Séquence réaliste : clone frais → `cargo build` nu
/// (bouchon créé, avertissement affiché une fois) → `cross build --release
/// --target armv7…`. Les scripts de build sont rejoués par cible, mais
/// `index.html` existe désormais — c'est le bouchon — donc la fonction
/// retournait tôt et **aucun avertissement** n'était émis : le binaire de
/// release embarquait une page « Web interface not built » en silence.
#[allow(dead_code)] // consommee par build.rs (via `include!`) et par les tests
pub fn est_un_bouchon(contenu: &str) -> bool {
    contenu.contains(MARQUEUR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_bouchon_est_un_html_qui_invite_a_construire_lihm() {
        let html = placeholder_html("npm run build --workspaces");
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("npm run build --workspaces"));
        // Pas de faux positif : le bouchon doit se reconnaitre a coup sur.
        assert!(html.contains(MARQUEUR));
    }

    #[test]
    fn est_un_bouchon_reconnait_le_bouchon_et_pas_un_vrai_livrable() {
        assert!(est_un_bouchon(&placeholder_html("npm ci && npm run build --workspaces")));
        // Forme d'un `index.html` reellement produit par Vite (import map,
        // point de montage) : aucun marqueur, donc aucun avertissement.
        let vrai = "<!doctype html><html><head><script type=\"importmap\">\
                    {\"imports\":{\"vue\":\"/assets/vue.js\"}}</script></head>\
                    <body><div id=\"app\"></div></body></html>";
        assert!(!est_un_bouchon(vrai));
        assert!(!est_un_bouchon(""));
    }
}
