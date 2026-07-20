use anyhow::{Context, Result};
use radio_pi_proto::View;
use std::io::Write;
use std::path::Path;

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
}

/// Rendu texte pour console (ANSI : efface l'écran, curseur en haut à gauche).
/// \r\n car sur /dev/tty1 le mode canonique n'insère pas le retour chariot.
pub fn render_console(view: &View) -> String {
    format!(
        "\x1b[2J\x1b[H\r\n  {}\r\n\r\n  {}\r\n\r\n  {}\r\n",
        view.line1, view.line2, view.line3
    )
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
