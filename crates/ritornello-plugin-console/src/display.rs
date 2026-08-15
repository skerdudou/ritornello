use anyhow::{Context, Result};
use ritornello_proto::{Overlay, PlayerState};
use std::io::Write;
use std::path::Path;

/// Trois lignes pour un écran texte d'environ vingt colonnes, composées depuis
/// l'état structuré.
///
/// C'est **ici** que vit la mise en page, et non dans le cœur : un autre
/// afficheur en écrira une autre à partir de la même trame, sans rien changer
/// au cœur.
pub fn compose(etat: &PlayerState) -> [String; 3] {
    // Une incrustation prend toute la place : elle est passagère et c'est ce
    // qu'on veut lire pendant qu'elle dure. Décision du propriétaire : le
    // texte arrive en un seul morceau depuis le cœur, et s'affiche en un seul
    // morceau — sur une ligne, là où l'incrustation volume tenait deux lignes
    // avant ce chantier (« VOLUME » puis « 65 % »). Le propriétaire a vu la
    // différence et l'a acceptée : ce n'est pas une régression.
    if let Some(o) = &etat.overlay {
        return [texte_incrustation(o).to_string(), String::new(), String::new()];
    }
    if etat.standby {
        return [etat.status.clone().unwrap_or_default(), String::new(), String::new()];
    }
    let line1 = match etat.preset {
        Some(n) => format!("{}  P{n}", etat.source.to_uppercase()),
        None => etat.source.to_uppercase(),
    };
    // Le nom de la présélection d'abord, puis l'album, puis le statut : du plus
    // spécifique au plus générique.
    let line2 = etat
        .preset_name
        .clone()
        .or_else(|| etat.morceau.album.clone())
        .or_else(|| etat.status.clone())
        .unwrap_or_default();
    let line3 = ligne_titre(etat.morceau.artist.as_deref(), etat.morceau.title.as_deref())
        .unwrap_or_default();
    [line1, line2, line3]
}

fn texte_incrustation(o: &Overlay) -> &str {
    match o {
        Overlay::Volume { text, .. } | Overlay::Tens { text, .. } | Overlay::Message { text, .. } => text,
    }
}

/// Ligne « artiste — titre », avec ses quatre replis. Déplacée du cœur : c'est
/// une décision de mise en page, donc elle appartient à l'afficheur.
///
/// Une information partielle vaut mieux que rien : l'artiste seul reste
/// affiché, parce qu'il dit déjà quelque chose de ce qu'on écoute.
pub fn ligne_titre(artist: Option<&str>, title: Option<&str>) -> Option<String> {
    match (artist, title) {
        (Some(a), Some(t)) => Some(format!("{a} — {t}")),
        (None, Some(t)) => Some(t.to_string()),
        (Some(a), None) => Some(a.to_string()),
        (None, None) => None,
    }
}

/// Rendu texte pour console (ANSI : efface l'écran, curseur en haut à gauche).
/// \r\n car sur /dev/tty1 le mode canonique n'insère pas le retour chariot.
pub fn render_console(etat: &PlayerState) -> String {
    let [line1, line2, line3] = compose(etat);
    format!(
        "\x1b[2J\x1b[H\r\n  {}\r\n\r\n  {}\r\n\r\n  {}\r\n",
        assainit(&line1),
        assainit(&line2),
        assainit(&line3)
    )
}

/// Retire les caractères de contrôle d'une ligne avant écriture sur le tty.
///
/// Depuis que ce plugin compose lui-même l'affichage, **chacune** des trois
/// lignes vient de données réseau : le nom de présélection (une configuration
/// éditable à distance), le statut d'une source, un titre ICY. Un flux (ou une
/// configuration compromise) qui glisserait `\x1b[...` dans l'un de ces champs
/// pourrait manipuler la console. Les seules séquences de contrôle du rendu
/// sont celles que ce module écrit lui-même ; le contenu, lui, reste des
/// données.
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
            .with_context(|| format!("opening {}", path.display()))?;
        Ok(Self { out })
    }

    pub fn show(&mut self, etat: &PlayerState) -> Result<()> {
        self.out.write_all(render_console(etat).as_bytes())?;
        self.out.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn etat_radio() -> PlayerState {
        PlayerState {
            source: "radio".into(),
            volume: 60,
            preset: Some(3),
            preset_name: Some("France Inter".into()),
            ..Default::default()
        }
    }

    #[test]
    fn compose_la_source_et_la_preselection_sur_la_premiere_ligne() {
        let l = compose(&etat_radio());
        assert_eq!(l[0], "RADIO  P3");
        assert_eq!(l[1], "France Inter");
    }

    #[test]
    fn les_quatre_replis_de_la_ligne_de_titre() {
        // Déplacés depuis le cœur avec la fonction qu'ils testent.
        assert_eq!(ligne_titre(Some("Miles Davis"), Some("So What")).as_deref(), Some("Miles Davis — So What"));
        assert_eq!(ligne_titre(None, Some("So What")).as_deref(), Some("So What"));
        // Décision du propriétaire : on affiche toute information disponible,
        // même partielle.
        assert_eq!(ligne_titre(Some("Miles Davis"), None).as_deref(), Some("Miles Davis"));
        assert_eq!(ligne_titre(None, None), None);
    }

    #[test]
    fn l_album_prime_sur_le_statut_quand_les_deux_existent() {
        // Ce que `line2_replaceable` négociait autrefois : le plugin décide,
        // sans avoir à demander la permission au cœur.
        let mut e = PlayerState { source: "cd".into(), preset: Some(1), preset_count: Some(3), ..Default::default() };
        e.status = Some("AUDIO CD".into());
        assert_eq!(compose(&e)[1], "AUDIO CD");
        e.morceau.album = Some("Kind of Blue".into());
        assert_eq!(compose(&e)[1], "Kind of Blue");
    }

    #[test]
    fn une_incrustation_prend_toute_la_place() {
        let mut e = etat_radio();
        e.overlay = Some(Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4000 });
        assert_eq!(compose(&e)[0], "VOLUME 65 %");
        assert_eq!(compose(&e)[1], "");
    }

    #[test]
    fn la_veille_affiche_son_mot_seul() {
        let e = PlayerState { standby: true, status: Some("VEILLE".into()), ..Default::default() };
        assert_eq!(compose(&e)[0], "VEILLE");
    }

    #[test]
    fn tout_le_contenu_est_assaini_pas_seulement_la_troisieme_ligne() {
        // Depuis que le plugin compose, **chaque** morceau vient du réseau : un
        // nom de station configuré à distance, un statut, un titre ICY. Un flux
        // qui glisserait `\x1b[2J` dans l'un d'eux pourrait manipuler la console.
        let e = PlayerState {
            source: "radio".into(),
            preset: Some(1),
            preset_name: Some("FI\x1b[2JP".into()),
            ..Default::default()
        };
        let s = render_console(&e);
        assert!(!s.contains("FI\x1b[2JP"));
        assert_eq!(s.matches('\x1b').count(), 2, "seuls les deux ESC de l'en-tête du rendu");
    }
}
