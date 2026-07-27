// Garantit que le répertoire embarqué par `rust-embed` existe et contient au
// moins un `index.html`. Le build npm n'est **jamais** invoqué ici : la
// cross-compilation par `cross` tourne dans une image Docker sans Node, et le
// livrable y est déjà présent sur le disque (voir `deploy/build.sh`).
include!("src/placeholder.rs");

const DIST: &str = "../../web/app/dist";
const AVERTISSEMENT: &str = "IHM web non construite : bouchon embarque a la place";

fn main() {
    // On surveille ici le répertoire dans lequel ce script écrit parfois
    // lui-même (le bouchon, plus bas). La documentation de Cargo déconseille
    // en général de surveiller un chemin qu'on modifie soi-même, mais c'est
    // volontaire et sûr ici : Cargo capture l'empreinte du répertoire APRÈS
    // l'exécution complète de ce script, donc l'écriture qui suit ne se
    // reboucle pas sur elle-même immédiatement — au pire, la prochaine
    // invocation de `cargo build` constatera que le contenu a changé (le
    // bouchon qu'on vient d'écrire) et recompilera une fois de plus, sans
    // jamais reboucler indéfiniment (une fois le bouchon présent, `index.html`
    // existe et la fonction retourne avant d'écrire quoi que ce soit).
    // Surveiller le seul `dist/index.html` plutôt que tout le répertoire
    // serait un pas en arrière fonctionnel : c'est cette surveillance plus
    // large qui permet de détecter l'arrivée d'un vrai build npm ultérieur
    // (nouveaux fichiers sous `assets/`) et de recompiler en conséquence.
    println!("cargo::rerun-if-changed={DIST}");
    println!("cargo::rerun-if-changed=src/placeholder.rs");
    let dist = std::path::Path::new(DIST);
    let index = dist.join("index.html");
    if index.exists() {
        // Le fichier est là, mais est-ce un livrable ou le bouchon écrit par
        // une invocation précédente de ce script ? L'avertissement n'était émis
        // qu'à la **création** du bouchon : un `cargo build` nu (bouchon créé,
        // avertissement affiché une fois) suivi d'un `cross build --release
        // --target armv7…` — les scripts de build sont rejoués par cible, mais
        // `index.html` existe désormais — ne disait plus rien, et le binaire de
        // release embarquait « Web interface not built » en silence. On relit
        // donc le fichier pour ré-émettre l'avertissement à **chaque** build
        // tant que le vrai livrable n'est pas là.
        if std::fs::read_to_string(&index).is_ok_and(|c| est_un_bouchon(&c)) {
            println!("cargo::warning={AVERTISSEMENT}");
        }
        return;
    }
    println!("cargo::warning={AVERTISSEMENT}");
    std::fs::create_dir_all(dist).expect("creation de web/app/dist");
    std::fs::write(&index, placeholder_html("npm ci && npm run build --workspaces"))
        .expect("ecriture du bouchon");
}
