use anyhow::{Context, Result};
use ritornello_proto::View;
use std::io::Write;
use std::path::Path;

/// Rendu texte pour console (ANSI : efface l'écran, curseur en haut à gauche).
/// \r\n car sur /dev/tty1 le mode canonique n'insère pas le retour chariot.
pub fn render_console(view: &View) -> String {
    format!(
        "\x1b[2J\x1b[H\r\n  {}\r\n\r\n  {}\r\n\r\n  {}\r\n",
        assainit(&view.line1),
        assainit(&view.line2),
        assainit(&view.line3)
    )
}

/// Retire les caractères de contrôle d'une ligne avant écriture sur le tty.
///
/// `line2`/`line3` finissent par porter des titres ICY venus du réseau : un
/// flux (ou un miroir compromis) qui glisse `\x1b[...` dans son titre pourrait
/// manipuler la console. Les seules séquences de contrôle du rendu sont celles
/// que ce module écrit lui-même ; le contenu, lui, reste des données.
fn assainit(ligne: &str) -> String {
    ligne.chars().filter(|c| !c.is_control()).collect()
}

pub struct ConsoleDisplay {
    out: std::fs::File,
}

impl ConsoleDisplay {
    pub fn open(path: &Path) -> Result<Self> {
        let out = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .with_context(|| format!("ouverture de {}", path.display()))?;
        Ok(Self { out })
    }

    pub fn show(&mut self, view: &View) -> Result<()> {
        self.out.write_all(render_console(view).as_bytes())?;
        self.out.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendu_efface_et_affiche_trois_lignes() {
        let v = View {
            line1: "RADIO  P3".into(),
            line2: "France Inter".into(),
            line3: "Le 7/9".into(),
        };
        let s = render_console(&v);
        assert!(s.starts_with("\x1b[2J\x1b[H"));
        assert!(s.contains("RADIO  P3\r\n"));
        assert!(s.contains("France Inter\r\n"));
        assert!(s.contains("Le 7/9\r\n"));
    }

    #[test]
    fn un_titre_venu_du_reseau_ne_peut_pas_injecter_de_sequence_de_controle() {
        // `line3` porte des titres ICY : données réseau non maîtrisées. Seules
        // les séquences écrites par `render_console` lui-même doivent survivre.
        let v = View {
            line1: "RADIO  P3".into(),
            line2: "FIP".into(),
            line3: "titre\x1b[2Jmalicieux\x07".into(),
        };
        let s = render_console(&v);
        // Sans l'ESC, « [2J » n'est plus qu'un texte imprimable inoffensif ;
        // le BEL, lui, disparaît entièrement.
        assert!(s.contains("titre[2Jmalicieux"));
        assert!(!s.contains('\x07'));
        // Les deux seuls ESC sont ceux de l'en-tête du rendu (\x1b[2J\x1b[H).
        assert_eq!(s.matches('\x1b').count(), 2);
    }
}
