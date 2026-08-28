use anyhow::{Context, Result};
use ritornello_proto::{Overlay, PlayerState};
use std::io::Write;
use std::path::Path;

/// Ce qui s'écrit en place du name de source quand le cœur n'en a aucune.
const NO_SOURCE: &str = "—";

/// L'heure locale de l'appareil, en heures (0-23) et minutes.
///
/// `None` quand l'horloge du système n'est pas convertible — avant que le
/// réseau n'ait remis un Raspberry Pi à l'heure, par exemple, un Pi n'ayant
/// pas de pile. L'afficheur écrit alors la veille sans horloge, plutôt qu'une
/// heure fausse.
///
/// **`libc::localtime_r` et non un nouveau crate de dates.** L'appel est déjà
/// l'idiome de ce dépôt pour ce que la bibliothèque C sait faire seule (voir
/// `system.rs` et le greffon cd), et `libc` y est déjà. Ajouter `chrono`
/// coûterait une dépendance et sa transitive de fuseaux, pour deux entiers.
///
/// **Le fuseau est celui que la glibc charge au premier appel**, et cela
/// couvre ce qui compte : les règles de changement d'heure vivent *dans* le
/// fichier de fuseau, donc le passage à l'heure d'été est suivi sans rien
/// relire. Ce qui n'est pas suivi est un opérateur qui change le fuseau de la
/// machine pendant que le service tourne — rare, et un redémarrage du greffon
/// le règle. `tzset()` à chaque tour l'aurait couvert au prix d'un `stat` par
/// tour d'horloge, et la fonction n'est de toute façon pas exposée par le crate
/// `libc` sur toutes les cibles.
///
/// La variante réentrante, seule sûre dans un processus à plusieurs children.
pub fn local_time() -> Option<(u32, u32)> {
    let secondes =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let t = libc::time_t::try_from(secondes).ok()?;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // Sûreté : `localtime_r` remplit `tm` et n'en conserve aucun pointeur ; on
    // lui passe deux références valides pour la durée de l'appel. Elle rend
    // NULL — jamais un `tm` à moitié écrit — pour un temps qu'elle ne sait pas
    // convertir, ce que le test ci-dessous traite comme « pas d'heure ».
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    let (h, m) = (u32::try_from(tm.tm_hour).ok()?, u32::try_from(tm.tm_min).ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

/// `13:05`, ou `1:05 PM` — selon le réglage que la trame d'état porte.
///
/// Sur 12 h, minuit s'écrit `12:00 AM` et midi `12:00 PM` : c'est la convention
/// anglo-saxonne, et un `0:00 AM` n'existe nulle part. Les heures ne sont pas
/// complétées par un zéro dans cette forme (`1:05 PM`, pas `01:05 PM`), là où la
/// forme 24 h les complète — les deux usages diffèrent, et c'est ce que
/// l'utilisateur read ailleurs.
pub fn format_time(heures: u32, minutes: u32, twelve_hour: bool) -> String {
    if !twelve_hour {
        return format!("{heures:02}:{minutes:02}");
    }
    let (h, suffixe) = match heures {
        0 => (12, "AM"),
        1..=11 => (heures, "AM"),
        12 => (12, "PM"),
        _ => (heures - 12, "PM"),
    };
    format!("{h}:{minutes:02} {suffixe}")
}

/// Comme [`compose`], mais avec l'heure à écrire en veille.
///
/// Séparée pour que la mise en page reste **pure** : `compose` ne read aucune
/// horloge, donc chaque cas se teste sur une heure choisie. `None` = pas
/// d'heure à afficher (horloge système inutilisable, ou état hors veille).
pub fn compose_at(state: &PlayerState, maintenant: Option<(u32, u32)>) -> [String; 3] {
    if state.overlay.is_none() && state.standby {
        // **L'heure en veille**, demandée par le propriétaire : c'est le seul
        // moment où l'écran n'a rien d'autre à dire, et une horloge y est plus
        // utile qu'un tty noir. Le mot de veille reste en première line — il
        // dit *pourquoi* rien ne plays — et l'heure prend la seconde.
        let heure = maintenant
            .map(|(h, m)| format_time(h, m, state.clock.twelve_hour))
            .unwrap_or_default();
        return [state.status.clone().unwrap_or_default(), heure, String::new()];
    }
    compose(state)
}

/// Trois lines pour un écran texte d'environ vingt colonnes, composées depuis
/// l'état structuré.
///
/// C'est **ici** que vit la mise en page, et non dans le cœur : un autre
/// afficheur en écrira une autre à partir de la même trame, sans rien changer
/// au cœur.
///
/// Ne read **aucune** horloge : l'heure de la veille entre par `compose_at`,
/// qui délègue à cette fonction tout le reste.
pub fn compose(state: &PlayerState) -> [String; 3] {
    // Une incrustation prend toute la place : elle est passagère et c'est ce
    // qu'on veut read pendant qu'elle dure. Décision du propriétaire : le
    // texte arrive en un seul track depuis le cœur, et s'affiche en un seul
    // track — sur une line, là où l'incrustation volume tenait deux lines
    // avant ce chantier (« VOLUME » puis « 65 % »). Le propriétaire a vu la
    // différence et l'a acceptée : ce n'est pas une régression.
    if let Some(o) = &state.overlay {
        return [overlay_text(o).to_string(), String::new(), String::new()];
    }
    if state.standby {
        return [state.status.clone().unwrap_or_default(), String::new(), String::new()];
    }
    // « SOURCE  n/total », et « SOURCE  n » quand le total est inconnu.
    //
    // Choix du propriétaire, arbitré pendant ce chantier. Chaque source avait
    // avant son propre idiome, encodé dans son sources_catalog : la radio écrivait
    // « RADIO  P3 », le cd « CD 1/3 ». Un afficheur unique ne peut pas les
    // rejouer tous sans coder en dur des names de plugins, ce qu'on refuse — donc
    // un idiome commun, qui rend au cd le total qu'il avait perdu et apprend à
    // la radio combien de stations sont configurées.
    //
    // Un total à zéro (« rien à numéroter » : tiroir clear) ne s'écrit pas :
    // « 1/0 » serait absurde. Le cas est atteignable, `preset_count` valant
    // `Some(0)` de façon significative dans ce protocol.
    // `source` clear **est** l'absence de source : depuis l'enregistrement à
    // chaud, le cœur démarre même si aucun greffon `source` n'a répondu, en
    // attendant qu'un retardataire s'announcement. Sans ce repli, les trois lines de
    // l'écran étaient vides — indistinguable d'un afficheur mort, alors que
    // justement tout fonctionne.
    //
    // Un tiret, et non un mot : cet afficheur ne traduit rien. Tout ce qu'il
    // écrit lui arrive déjà traduit du cœur (le statut, le mot de veille), il n'a
    // ni sources_catalog ni langue courante — un `NO SOURCE` codé en dur ici mentirait
    // sur un appareil en français. Le tiret cadratin est déjà de ses caractères
    // (voir `title_line`).
    let name = if state.source.is_empty() {
        NO_SOURCE.to_string()
    } else {
        state.source.to_uppercase()
    };
    let line1 = match (state.preset, state.preset_count) {
        (Some(n), Some(total)) if total > 0 => format!("{name}  {n}/{total}"),
        (Some(n), _) => format!("{name}  {n}"),
        (None, _) => name,
    };
    // Le name de la présélection d'abord, puis l'album, puis le statut : du plus
    // spécifique au plus générique.
    let line2 = state
        .preset_name
        .clone()
        .or_else(|| state.track.album.clone())
        .or_else(|| state.status.clone())
        .unwrap_or_default();
    let line3 = title_line(state.track.artist.as_deref(), state.track.title.as_deref())
        .unwrap_or_default();
    [line1, line2, line3]
}

fn overlay_text(o: &Overlay) -> &str {
    match o {
        Overlay::Volume { text, .. } | Overlay::Tens { text, .. } | Overlay::Message { text, .. } => text,
    }
}

/// Line « artiste — titre », avec ses quatre replis. Déplacée du cœur : c'est
/// une décision de mise en page, donc elle appartient à l'afficheur.
///
/// Une information partielle vaut mieux que rien : l'artiste seul reste
/// affiché, parce qu'il dit déjà quelque chose de ce qu'on écoute.
pub fn title_line(artist: Option<&str>, title: Option<&str>) -> Option<String> {
    match (artist, title) {
        (Some(a), Some(t)) => Some(format!("{a} — {t}")),
        (None, Some(t)) => Some(t.to_string()),
        (Some(a), None) => Some(a.to_string()),
        (None, None) => None,
    }
}

/// Rendition texte pour console (ANSI : efface l'écran, curseur en haut à gauche).
/// \r\n car sur /dev/tty1 le mode canonique n'insère pas le retour chariot.
///
/// `#[cfg(test)]` : depuis que `ConsoleDisplay::show` mémorise son last
/// rendition (voir plus bas), la production appelle directement `render_lines`
/// sur les lines déjà composées, pour les comparer aux précédentes avant
/// d'écrire quoi que ce soit. Cette fonction reste la commodité des tests, qui
/// n'ont pas cette comparaison à faire et raisonnent sur un `PlayerState`
/// complet.
#[cfg(test)]
fn render_console(state: &PlayerState) -> String {
    render_lines(&compose(state))
}

/// Assemble le rendition ANSI à partir de lines déjà composées : partagé par
/// `render_console` (qui compose depuis un état complet, réservé aux tests) et
/// `ConsoleDisplay::show` (qui a besoin des lines à part pour les comparer
/// aux précédentes avant d'écrire quoi que ce soit).
fn render_lines(lines: &[String; 3]) -> String {
    format!(
        "\x1b[2J\x1b[H\r\n  {}\r\n\r\n  {}\r\n\r\n  {}\r\n",
        sanitize(&lines[0]),
        sanitize(&lines[1]),
        sanitize(&lines[2])
    )
}

/// Retire les caractères de contrôle d'une line avant écriture sur le tty.
///
/// Depuis que ce plugin compose lui-même l'affichage, **chacune** des trois
/// lines vient de données réseau : le name de présélection (une configuration
/// éditable à distance), le statut d'une source, un titre ICY. Un stream (ou une
/// configuration compromise) qui glisserait `\x1b[...` dans l'un de ces champs
/// pourrait manipuler la console. Les seules séquences de contrôle du rendition
/// sont celles que ce module écrit lui-même ; le contenu, lui, reste des
/// données.
fn sanitize(line: &str) -> String {
    line.chars().filter(|c| !c.is_control()).collect()
}

pub struct ConsoleDisplay {
    out: std::fs::File,
    /// Dernières lines effectivement écrites sur le tty. Le canal du cœur
    /// déduplique sur l'état *entier* (`PlayerState`) : une trame qui ne
    /// change que `preset_count`, `duration_s` ou `origin` — invisibles de
    /// `compose` — franchit donc cette garde et arrive jusqu'ici. Sans
    /// mémoire de son propre rendition, ce plugin réimprimerait les trois mêmes
    /// lines, précédées de l'effacement d'écran : un clignotement visible
    /// sur un tty pour une trame qu'il ne montre même pas.
    last_lines: Option<[String; 3]>,
}

impl ConsoleDisplay {
    pub fn open(path: &Path) -> Result<Self> {
        let out = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        Ok(Self { out, last_lines: None })
    }

    /// Réécrit l'écran depuis l'état courant, en lisant l'horloge du système.
    ///
    /// L'horloge est lue **ici** et non dans `compose_at`, qui reste pure : le
    /// seul appel impur du rendition vit au seul endroit qui touche déjà le tty.
    pub fn show(&mut self, state: &PlayerState) -> Result<()> {
        // Lue seulement quand elle sert : hors veille, `compose_at` ne la
        // regarde pas, et un `tzset` par trame d'état — une par seconde en
        // playback — serait un `stat` par seconde pour rien.
        let maintenant = state.standby.then(local_time).flatten();
        let lines = compose_at(state, maintenant);
        if self.last_lines.as_ref() == Some(&lines) {
            return Ok(());
        }
        self.out.write_all(render_lines(&lines).as_bytes())?;
        self.out.flush()?;
        self.last_lines = Some(lines);
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
            preset_count: Some(12),
            preset_name: Some("France Inter".into()),
            ..Default::default()
        }
    }

    #[test]
    fn compose_la_source_la_preselection_et_le_total_sur_la_premiere_ligne() {
        let l = compose(&etat_radio());
        assert_eq!(l[0], "RADIO  3/12");
        assert_eq!(l[1], "France Inter");
    }

    #[test]
    fn la_veille_affiche_lheure_sous_le_mot_de_veille() {
        // Demande du propriétaire : l'écran de veille n'a rien d'autre à dire,
        // une horloge y est plus utile qu'un tty noir. Le mot de veille reste
        // en tête — il dit *pourquoi* rien ne plays.
        let state = PlayerState { standby: true, status: Some("VEILLE".into()), ..Default::default() };
        assert_eq!(compose_at(&state, Some((13, 5))), ["VEILLE", "13:05", ""]);
    }

    #[test]
    fn la_veille_suit_le_reglage_douze_heures() {
        // Le réglage voyage dans la trame d'état : un afficheur ne va jamais
        // rien chercher de côté.
        let mut state =
            PlayerState { standby: true, status: Some("VEILLE".into()), ..Default::default() };
        state.clock.twelve_hour = true;
        assert_eq!(compose_at(&state, Some((13, 5)))[1], "1:05 PM");
    }

    #[test]
    fn une_horloge_inutilisable_laisse_la_veille_sans_heure() {
        // Un Pi n'a pas de pile : avant que le réseau ne l'ait remis à l'heure,
        // mieux vaut pas d'heure du tout qu'une heure fausse.
        let state = PlayerState { standby: true, status: Some("VEILLE".into()), ..Default::default() };
        assert_eq!(compose_at(&state, None), ["VEILLE", "", ""]);
    }

    #[test]
    fn une_incrustation_passe_devant_lhorloge_de_veille() {
        // L'incrustation est passagère et c'est ce qu'on veut read pendant
        // qu'elle dure — la règle qui vaut partout ailleurs dans `compose`.
        let state = PlayerState {
            standby: true,
            status: Some("VEILLE".into()),
            overlay: Some(Overlay::Message { text: "PAS DE DISQUE".into(), remaining_ms: 2000 }),
            ..Default::default()
        };
        assert_eq!(compose_at(&state, Some((13, 5))), ["PAS DE DISQUE", "", ""]);
    }

    #[test]
    fn les_deux_formats_dheure_couvrent_minuit_et_midi() {
        // Les deux bornes que la convention anglo-saxonne traite à part : un
        // `0:00 AM` n'existe nulle part, et midi est `12:00 PM`.
        assert_eq!(format_time(0, 0, false), "00:00");
        assert_eq!(format_time(0, 0, true), "12:00 AM");
        assert_eq!(format_time(12, 0, true), "12:00 PM");
        assert_eq!(format_time(23, 59, true), "11:59 PM");
        assert_eq!(format_time(9, 5, true), "9:05 AM");
        assert_eq!(format_time(9, 5, false), "09:05");
    }

    #[test]
    fn lheure_locale_est_lisible_et_dans_les_bornes() {
        // L'unique appel impur du module. On ne peut pas prédire l'heure, mais
        // on peut exiger qu'elle soit une heure : c'est ce qui attraperait une
        // `tm` mal lue (un champ pris pour un autre, un fuseau qui déborde).
        let (h, m) = local_time().expect("l'horloge du systeme de test doit etre convertible");
        assert!(h < 24, "heure hors bornes : {h}");
        assert!(m < 60, "minutes hors bornes : {m}");
    }

    #[test]
    fn la_premiere_ligne_omet_un_total_inconnu_ou_nul() {
        // Sans total déclaré, le numéro seul. Et surtout : un total à zéro
        // (« rien à numéroter », tiroir clear) ne s'écrit pas — « 1/0 » serait
        // absurde, et `Some(0)` est une valeur significative de ce protocol,
        // pas un accident.
        let mut e = etat_radio();
        e.preset_count = None;
        assert_eq!(compose(&e)[0], "RADIO  3");
        e.preset_count = Some(0);
        assert_eq!(compose(&e)[0], "RADIO  3");
    }

    #[test]
    fn un_coeur_sans_aucune_source_dit_labsence_au_lieu_de_ne_rien_ecrire() {
        // Le cœur démarre désormais sans source, en attendant qu'un greffon
        // s'announcement. `source` clear, et rien d'autre à écrire : l'écran entier
        // était clear, indistinguable d'un afficheur mort ou d'un tty perdu.
        let e = PlayerState::default();
        assert_eq!(compose(&e), ["—".to_string(), String::new(), String::new()]);
    }

    #[test]
    fn le_cd_retrouve_sa_piste_sur_son_total() {
        // Ce que le plugin cd composait lui-même avant ce chantier (« CD 1/3 »),
        // rendition par l'afficheur depuis les seules données de la trame.
        let e = PlayerState {
            source: "cd".into(),
            preset: Some(1),
            preset_count: Some(3),
            ..Default::default()
        };
        assert_eq!(compose(&e)[0], "CD  1/3");
    }

    #[test]
    fn les_quatre_replis_de_la_ligne_de_titre() {
        // Déplacés depuis le cœur avec la fonction qu'ils testent.
        assert_eq!(title_line(Some("Miles Davis"), Some("So What")).as_deref(), Some("Miles Davis — So What"));
        assert_eq!(title_line(None, Some("So What")).as_deref(), Some("So What"));
        // Décision du propriétaire : on affiche toute information disponible,
        // même partielle.
        assert_eq!(title_line(Some("Miles Davis"), None).as_deref(), Some("Miles Davis"));
        assert_eq!(title_line(None, None), None);
    }

    #[test]
    fn l_album_prime_sur_le_statut_quand_les_deux_existent() {
        // Ce que `line2_replaceable` négociait autrefois : le plugin décide,
        // sans avoir à demander la permission au cœur.
        let mut e = PlayerState { source: "cd".into(), preset: Some(1), preset_count: Some(3), ..Default::default() };
        e.status = Some("AUDIO CD".into());
        assert_eq!(compose(&e)[1], "AUDIO CD");
        e.track.album = Some("Kind of Blue".into());
        assert_eq!(compose(&e)[1], "Kind of Blue");
    }

    #[test]
    fn un_nom_de_preselection_n_est_jamais_supplante_par_un_album() {
        // L'autre moitié de la règle ci-dessus, et la plus facile à casser : le
        // cd laisse l'album gagner parce qu'il ne nomme pas ses pistes, mais une
        // station nommée doit rester affichée même quand un plugin `metadata`
        // finit par résoudre un album. Le cœur garantissait cela par
        // `line2_replaceable`, que la radio ne déclarait pas ; ici c'est l'order
        // de l'`or_else` qui le garantit, et rien ne signalerait son inversion.
        let mut e = etat_radio();
        e.track.album = Some("Kind of Blue".into());
        assert_eq!(compose(&e)[1], "France Inter");
    }

    #[test]
    fn le_rendu_efface_l_ecran_et_espace_les_trois_lignes() {
        // Format du rendition lui-même, indépendamment de ce que `compose` décide :
        // en-tête ANSI d'effacement, retours chariot explicites (sur /dev/tty1
        // le mode canonique ne les insère pas), et une line clear entre chaque.
        let mut e = etat_radio();
        e.track.artist = Some("Miles Davis".into());
        e.track.title = Some("So What".into());
        let s = render_console(&e);
        assert!(s.starts_with("\x1b[2J\x1b[H"));
        assert!(s.contains("RADIO  3/12\r\n"));
        assert!(s.contains("France Inter\r\n"));
        assert!(s.contains("Miles Davis — So What\r\n"));
        assert_eq!(s.matches("\r\n\r\n").count(), 2, "une line clear entre chacune des trois");
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
        // Depuis que le plugin compose, **chaque** track vient du réseau : un
        // name de station configuré à distance, un statut, un titre ICY. Un stream
        // qui glisserait `\x1b[2J` dans l'un d'eux pourrait manipuler la console.
        let e = PlayerState {
            source: "radio".into(),
            preset: Some(1),
            preset_name: Some("FI\x1b[2JP".into()),
            ..Default::default()
        };
        let s = render_console(&e);
        assert!(!s.contains("FI\x1b[2JP"));
        assert_eq!(s.matches('\x1b').count(), 2, "seuls les deux ESC de l'en-tête du rendition");
    }

    #[test]
    fn un_bel_est_aussi_retire_pas_seulement_lesc() {
        // Régression M4 (revue de branche) : l'ancien test épinglait aussi la
        // disparition du BEL (`\x07`), en plus du compte d'ESC. Un `sanitize`
        // réduit par erreur au seul filtrage d'ESC passerait le test
        // précédent sans être réellement sûr — `is_control` doit couvrir tous
        // les caractères de contrôle, pas seulement celui du rendition lui-même.
        let e = PlayerState {
            source: "radio".into(),
            preset_name: Some("FI\x07P".into()),
            ..Default::default()
        };
        let s = render_console(&e);
        assert!(!s.contains('\x07'), "le BEL doit disparaitre comme n'importe quel caractere de controle");
    }

    #[test]
    fn une_deuxieme_trame_aux_memes_lignes_ne_reecrit_pas_lecran() {
        // Régression M3 (revue de branche) : le canal du cœur déduplique sur
        // l'état ENTIER, pas sur les lines composées. Une trame qui ne change
        // que `duration_s` — invisible de `compose` — franchit donc la garde
        // du cœur et arrive jusqu'ici : sans mémoire de son propre rendition, le
        // plugin réimprimerait les trois mêmes lines, précédées de
        // l'effacement d'écran (clignotement visible sur un tty).
        //
        // Le fichier n'est pas ouvert en écriture tronquante : une seconde
        // écriture réelle se placerait après la première (le curseur a
        // avancé) et doublerait le contenu du fichier, ce que l'égalité
        // ci-dessous détecterait.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tty");
        std::fs::write(&path, b"").unwrap();
        let mut display = ConsoleDisplay::open(&path).unwrap();
        let mut e = etat_radio();
        display.show(&e).unwrap();
        let apres_premiere = std::fs::read(&path).unwrap();
        assert!(!apres_premiere.is_empty());

        e.track.duration_s = Some(210);
        display.show(&e).unwrap();
        let apres_seconde = std::fs::read(&path).unwrap();
        assert_eq!(
            apres_premiere, apres_seconde,
            "les trois lines composees sont identiques : la seconde ecriture n'aurait pas du avoir lieu"
        );
    }

    #[test]
    fn une_trame_aux_lignes_differentes_reecrit_bien_lecran() {
        // Garde-fou du test ci-dessus : la mémorisation ne doit pas empêcher
        // un vrai changement visible de s'afficher.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tty");
        std::fs::write(&path, b"").unwrap();
        let mut display = ConsoleDisplay::open(&path).unwrap();
        let mut e = etat_radio();
        display.show(&e).unwrap();
        let apres_premiere = std::fs::read(&path).unwrap();

        e.preset = Some(4);
        display.show(&e).unwrap();
        let apres_seconde = std::fs::read(&path).unwrap();
        assert!(apres_seconde.len() > apres_premiere.len(), "la seconde ecriture a bien eu lieu");
    }

    /// Décision de conception : cet afficheur **ne montre pas** la position.
    /// Trois lines d'une vingtaine de colonnes déjà pleines, et une horloge y
    /// coûterait un effacement d'écran par seconde — or le cœur en publie une
    /// trame par seconde pendant toute la playback. Le champ voyage quand même
    /// jusqu'ici : tout autre plugin d'affichage peut s'en serve.
    #[test]
    fn une_trame_qui_ne_change_que_la_position_compose_les_memes_lignes() {
        let mut e = etat_radio();
        let avant = compose(&e);
        e.position_s = Some(87);
        assert_eq!(compose(&e), avant);
        e.position_s = Some(88);
        assert_eq!(compose(&e), avant);
    }

    /// Et le corollaire sur l'incrustation : pendant un message éphémère, les
    /// trames par seconde composent la même line unique, donc la garde
    /// `last_lines` les absorbe — aucun clignotement pendant que le
    /// message est à l'écran.
    #[test]
    fn une_incrustation_survit_aux_trames_par_seconde() {
        let mut e = etat_radio();
        e.overlay = Some(Overlay::Message { text: "PRESELECTION VIDE".into(), remaining_ms: 5000 });
        e.position_s = Some(87);
        let avant = compose(&e);
        e.position_s = Some(88);
        e.overlay = Some(Overlay::Message { text: "PRESELECTION VIDE".into(), remaining_ms: 4000 });
        assert_eq!(compose(&e), avant);
    }
}
