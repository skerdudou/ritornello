// Garantit l'existence de `ui/dist/{ui.js,ui.css}` embarques par
// `include_str!`. Le build npm n'est **jamais** invoque ici (voir
// `deploy/build.sh`) : la cross-compilation tourne dans une image sans Node.
include!("src/placeholder.rs");

// Ecrit `chemin` avec `contenu` s'il est absent ; s'il est deja la, le relit
// et re-emet l'avertissement tant qu'il s'agit du bouchon.
//
// C'est le point corrige : `cargo::warning` n'etait emis qu'a la **creation**
// du bouchon. Clone frais -> `cargo build` nu (bouchon cree, avertissement
// affiche une fois) -> `cross build --release --target armv7...` : les scripts
// de build sont rejoues par cible, mais `ui/dist/ui.js` existe desormais
// (c'est le bouchon), donc plus **aucun** avertissement -- et le binaire de
// release embarquait le bouchon en silence. L'avertissement nomme le fichier :
// deux actifs de bouchon donnent deux lignes distinctes et exploitables.
fn garantir(chemin: &std::path::Path, contenu: impl FnOnce() -> String) {
    let avertir = || {
        println!(
            "cargo::warning=IHM du plugin non construite : bouchon embarque ({})",
            chemin.display()
        );
    };
    match std::fs::read_to_string(chemin) {
        Ok(deja) => {
            if est_un_bouchon(&deja) {
                avertir();
            }
        }
        Err(_) => {
            avertir();
            std::fs::write(chemin, contenu()).unwrap();
        }
    }
}

fn main() {
    println!("cargo::rerun-if-changed=ui/dist");
    println!("cargo::rerun-if-changed=src/placeholder.rs");
    let dist = std::path::Path::new("ui/dist");
    std::fs::create_dir_all(dist).expect("creation de ui/dist");
    garantir(&dist.join("ui.js"), || {
        ui_placeholder_js("npm ci && npm run build --workspaces")
    });
    garantir(&dist.join("ui.css"), ui_placeholder_css);
}
