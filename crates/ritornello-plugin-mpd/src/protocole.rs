//! La forme du protocole MPD : découper une ligne de commande, mettre en forme
//! les réponses et les refus. Aucune E/S ici — c'est ce qui rend tout le reste
//! testable sans socket.
//!
//! **Sans appelant avant la Task 6** (`commandes.rs`/`session.rs`), donc les
//! éléments publics ci-dessous ne sont exercés que par les tests de ce
//! fichier. `#[allow(dead_code)]` le dit explicitement plutôt que de laisser
//! `-D warnings` casser la compilation d'un module par ailleurs complet et
//! testé : la Task 6 câble l'appelant réel et retire ces attributs.

use std::fmt::Display;

/// Les seuls codes d'erreur que ce serveur emploie. Les valeurs sont celles de
/// `ack.h` de MPD et ne peuvent pas changer : les clients les lisent.
#[allow(dead_code)] // Task 6 cable l'appelant qui choisit entre ces variantes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    /// Argument absent, non numérique, ou hors bornes.
    Arg = 2,
    /// Commande inconnue **ou** volontairement non gérée. MPD ne distingue pas
    /// les deux, et c'est tant mieux : `commands` dit déjà ce qui existe.
    Unknown = 5,
    /// Liste enregistrée nommée qui n'existe pas.
    NoExist = 50,
}

/// `ACK [<code>@<indice>] {<commande>} <message>`. `indice` est le rang de la
/// commande dans une liste de commandes, 0 hors liste.
#[allow(dead_code)] // Task 6 en fait l'appelant reel, voir le commentaire de module.
pub fn ack(code: Ack, indice: usize, commande: &str, message: &str) -> String {
    format!("ACK [{}@{indice}] {{{commande}}} {message}", code as u16)
}

/// Une ligne `clé: valeur` de réponse.
#[allow(dead_code)] // Task 6 en fait l'appelant reel, voir le commentaire de module.
pub fn ligne(cle: &str, valeur: impl Display) -> String {
    format!("{cle}: {valeur}")
}

/// Découpe une ligne de commande. Les arguments sont séparés par des espaces ;
/// un argument entre guillemets doubles peut en contenir, et `\"` comme `\\` y
/// sont des littéraux.
///
/// Un guillemet non fermé est `Ack::Arg` et non une tolérance : accepter la
/// ligne ferait exécuter une commande dont l'argument est tronqué, ce qui est
/// pire qu'un refus lisible.
#[allow(dead_code)] // Task 6 en fait l'appelant reel, voir le commentaire de module.
pub fn decouper(ligne: &str) -> Result<Vec<String>, Ack> {
    let mut args = Vec::new();
    let mut chars = ligne.chars().peekable();
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
        assert_eq!(decouper("status").unwrap(), vec!["status"]);
        assert_eq!(decouper("play 3").unwrap(), vec!["play", "3"]);
        // Les espaces multiples ne produisent pas d'argument vide.
        assert_eq!(decouper("play   3").unwrap(), vec!["play", "3"]);
    }

    #[test]
    fn un_argument_entre_guillemets_garde_ses_espaces() {
        assert_eq!(decouper(r#"load "France Inter""#).unwrap(), vec!["load", "France Inter"]);
    }

    #[test]
    fn les_echappements_dans_les_guillemets() {
        // `\"` est un guillemet litteral, `\\` une contre-oblique litterale.
        assert_eq!(decouper(r#"load "un \"nom\"""#).unwrap(), vec!["load", r#"un "nom""#]);
        assert_eq!(decouper(r#"load "a\\b""#).unwrap(), vec!["load", r"a\b"]);
    }

    #[test]
    fn un_guillemet_non_ferme_est_un_argument_invalide() {
        assert_eq!(decouper(r#"load "France"#), Err(Ack::Arg));
    }

    #[test]
    fn une_ligne_vide_ne_donne_aucun_argument() {
        assert!(decouper("").unwrap().is_empty());
        assert!(decouper("   ").unwrap().is_empty());
    }

    #[test]
    fn un_argument_vide_entre_guillemets_est_legal() {
        // `listplaylistinfo ""` doit arriver comme un nom vide, pas disparaitre.
        assert_eq!(decouper(r#"listplaylistinfo """#).unwrap(), vec!["listplaylistinfo", ""]);
    }

    #[test]
    fn une_tabulation_separe_les_arguments_comme_une_espace() {
        // Le brief ne le teste pas explicitement, mais l'implementation traite
        // `\t` comme separateur au meme titre que ' ' (avant guillemets, dans
        // la boucle de saut, et comme fin d'un argument non guillemete) : ces
        // trois chemins meritent d'etre vus une fois.
        assert_eq!(decouper("play\t3").unwrap(), vec!["play", "3"]);
        assert_eq!(decouper("\tplay").unwrap(), vec!["play"]);
    }

    #[test]
    fn une_contre_oblique_hors_guillemets_est_litterale() {
        // Hors guillemets, `\` n'introduit aucun echappement : c'est un
        // caractere ordinaire de l'argument, au meme titre qu'une lettre.
        // Un client MPD qui envoie un chemin Windows non guillemete (rare,
        // mais MALP le permet en pratique) doit le retrouver intact.
        assert_eq!(decouper(r"load C:\musique").unwrap(), vec!["load", r"C:\musique"]);
    }

    #[test]
    fn lack_porte_son_code_son_indice_et_sa_commande() {
        assert_eq!(ack(Ack::NoExist, 0, "load", "no such playlist"), "ACK [50@0] {load} no such playlist");
        // L'indice est le rang dans une liste de commandes.
        assert_eq!(ack(Ack::Arg, 2, "setvol", "invalid volume"), "ACK [2@2] {setvol} invalid volume");
    }

    #[test]
    fn ligne_met_en_forme_une_paire_cle_valeur() {
        assert_eq!(ligne("volume", 42), "volume: 42");
        assert_eq!(ligne("state", "play"), "state: play");
    }
}
