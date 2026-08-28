//! La forme du protocol MPD : découper une line de commande, mettre en forme
//! les réponses et les refus. Aucune E/S ici — c'est ce qui rend tout le remainder
//! testable sans socket.
//!
//! `ack` et `line` sont appelés par `commands.rs`, `split` par la session
//! — seule à read des lines. Plus aucun `#[allow(dead_code)]` ici : les trois
//! ont leur appelant.

use std::fmt::Display;

/// Les seuls codes d'erreur que ce serveur emploie. Les valeurs sont celles de
/// `ack.h` de MPD et ne peuvent pas changer : les clients les lisent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    /// Argument absent, non numérique, ou hors bounds.
    Arg = 2,
    /// Commande inconnue **ou** volontairement non gérée. MPD ne distingue pas
    /// les deux, et c'est tant mieux : `commands` dit déjà ce qui existe.
    Unknown = 5,
    /// Ce qui est nommé n'existe pas : une liste enregistrée, ou l'image d'une
    /// URI.
    ///
    /// Quatre producteurs dans `commands.rs`, et le name qu'ils refusent est
    /// toujours bien formé — c'est ce qui distingue ce code d'un `Arg` : `load`
    /// et `listplaylistinfo` pour un name de source absent du sources_catalog,
    /// `albumart` et `readpicture` pour une URI dont ce qui plays n'a pas
    /// d'image à cet instant.
    NoExist = 50,
}

/// `ACK [<code>@<index>] {<commande>} <message>`. `index` est le rang de la
/// commande dans une liste de commands, 0 hors liste.
pub fn ack(code: Ack, index: usize, commande: &str, message: &str) -> String {
    format!("ACK [{}@{index}] {{{commande}}} {message}", code as u16)
}

/// Une line `clé: valeur` de réponse.
pub fn line(key: &str, valeur: impl Display) -> String {
    format!("{key}: {valeur}")
}

/// Découpe une line de commande. Les arguments sont séparés par des espaces ;
/// un argument entre guillemets doubles peut en contenir, et `\"` comme `\\` y
/// sont des littéraux.
///
/// Un guillemet non fermé est `Ack::Arg` et non une tolérance : accepter la
/// line ferait exécuter une commande dont l'argument est tronqué, ce qui est
/// pire qu'un refus lisible.
///
/// Son appelant est celui qui **read des lines**, donc la session.
/// `commands.rs` reçoit une commande déjà découpée — c'est ce qui lui permet
/// de n'avoir aucune E/S.
pub fn split(line: &str) -> Result<Vec<String>, Ack> {
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c == ' ' || c == '\t' {
            chars.next();
            continue;
        }
        if c == '"' {
            chars.next();
            let mut arg = String::new();
            loop {
                match chars.next() {
                    None => return Err(Ack::Arg),
                    Some('"') => break,
                    Some('\\') => match chars.next() {
                        None => return Err(Ack::Arg),
                        Some(e) => arg.push(e),
                    },
                    Some(autre) => arg.push(autre),
                }
            }
            args.push(arg);
        } else {
            let mut arg = String::new();
            while let Some(&c) = chars.peek() {
                if c == ' ' || c == '\t' {
                    break;
                }
                arg.push(c);
                chars.next();
            }
            args.push(arg);
        }
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_arguments_simples_se_decoupent_sur_les_espaces() {
        assert_eq!(split("status").unwrap(), vec!["status"]);
        assert_eq!(split("play 3").unwrap(), vec!["play", "3"]);
        // Les espaces multiples ne produisent pas d'argument clear.
        assert_eq!(split("play   3").unwrap(), vec!["play", "3"]);
    }

    #[test]
    fn un_argument_entre_guillemets_garde_ses_espaces() {
        assert_eq!(split(r#"load "France Inter""#).unwrap(), vec!["load", "France Inter"]);
    }

    #[test]
    fn les_echappements_dans_les_guillemets() {
        // `\"` est un guillemet litteral, `\\` une contre-oblique litterale.
        assert_eq!(split(r#"load "un \"name\"""#).unwrap(), vec!["load", r#"un "name""#]);
        assert_eq!(split(r#"load "a\\b""#).unwrap(), vec!["load", r"a\b"]);
    }

    #[test]
    fn un_guillemet_non_ferme_est_un_argument_invalide() {
        assert_eq!(split(r#"load "France"#), Err(Ack::Arg));
    }

    #[test]
    fn une_ligne_vide_ne_donne_aucun_argument() {
        assert!(split("").unwrap().is_empty());
        assert!(split("   ").unwrap().is_empty());
    }

    #[test]
    fn un_argument_vide_entre_guillemets_est_legal() {
        // `listplaylistinfo ""` doit arriver comme un name clear, pas disparaitre.
        assert_eq!(split(r#"listplaylistinfo """#).unwrap(), vec!["listplaylistinfo", ""]);
    }

    #[test]
    fn une_tabulation_separe_les_arguments_comme_une_espace() {
        // Le brief ne le teste pas explicitement, mais l'implementation traite
        // `\t` comme separateur au meme titre que ' ' (avant guillemets, dans
        // la boucle de saut, et comme fin d'un argument non guillemete) : ces
        // trois chemins meritent d'etre vus une fois.
        assert_eq!(split("play\t3").unwrap(), vec!["play", "3"]);
        assert_eq!(split("\tplay").unwrap(), vec!["play"]);
    }

    #[test]
    fn une_contre_oblique_hors_guillemets_est_litterale() {
        // Hors guillemets, `\` n'introduit aucun echappement : c'est un
        // caractere ordinaire de l'argument, au meme titre qu'une lettre.
        // Un client MPD qui envoie un path Windows non guillemete (rare,
        // mais MALP le permet en pratique) doit le retrouver intact.
        assert_eq!(split(r"load C:\musique").unwrap(), vec!["load", r"C:\musique"]);
    }

    #[test]
    fn une_contre_oblique_terminale_dans_une_chaine_est_un_argument_invalide() {
        // Le cas limit que la relecture de la Task 4 a signalé : `"abc\` finit
        // sur une contre-oblique qui appelle un caractere qui n'existe pas. Le
        // tolerer rendrait `abc`, donc un argument **tronque** presente comme
        // valide — exactement ce que le refus du guillemet non ferme evite.
        assert_eq!(split(r#"load "abc\"#), Err(Ack::Arg));
        // Et la variante ou l'echappement mange le guillemet fermant : la
        // chaine n'est alors plus fermee du tout.
        assert_eq!(split(r#"load "abc\""#), Err(Ack::Arg));
    }

    #[test]
    fn un_nom_accentue_survit_a_laller_retour() {
        // Les names de stations francaises sont accentues : `Chérie FM` doit
        // ressortir caractere pour caractere. Le decoupage travaille sur des
        // `char` et non sur des bytes, donc un `é` ne se coupe pas en deux —
        // mais rien ne le disait, et c'est le kind de propriete qui se casse
        // le jour ou quelqu'un passe aux bytes pour aller plus vite.
        let line = r#"load "Chérie FM""#;
        assert_eq!(split(line).unwrap(), vec!["load", "Chérie FM"]);
        // Un name entierement non ASCII, guillemets et espaces compris.
        assert_eq!(split(r#"load "Radio Nova — Résonances""#).unwrap()[1], "Radio Nova — Résonances");
    }

    #[test]
    fn lack_porte_son_code_son_indice_et_sa_commande() {
        assert_eq!(ack(Ack::NoExist, 0, "load", "no such playlist"), "ACK [50@0] {load} no such playlist");
        // L'index est le rang dans une liste de commands.
        assert_eq!(ack(Ack::Arg, 2, "setvol", "invalid volume"), "ACK [2@2] {setvol} invalid volume");
    }

    #[test]
    fn ligne_met_en_forme_une_paire_cle_valeur() {
        assert_eq!(line("volume", 42), "volume: 42");
        assert_eq!(line("state", "play"), "state: play");
    }
}
