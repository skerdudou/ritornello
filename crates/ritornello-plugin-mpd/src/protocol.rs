//! The shape of the MPD protocol: splitting a command line, formatting the
//! replies and the rejections. No I/O here — that is what makes everything
//! else testable without a socket.
//!
//! `ack` and `line` are called by `commands.rs`, `split` by the session — the
//! only one that reads lines. No more `#[allow(dead_code)]` here: all three
//! have their caller.

use std::fmt::Display;

/// The only error codes this server uses. The values are those of MPD's
/// `ack.h` and cannot change: clients read them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    /// Argument absent, non-numeric, or out of bounds.
    Arg = 2,
    /// Unknown command **or** deliberately unhandled. MPD does not distinguish
    /// the two, and so much the better: `commands` already says what exists.
    Unknown = 5,
    /// What is named does not exist: a saved playlist, or the image of a URI.
    ///
    /// Four producers in `commands.rs`, and the name they reject is always
    /// well formed — that is what distinguishes this code from an `Arg`:
    /// `load` and `listplaylistinfo` for a source name absent from the
    /// catalog, `albumart` and `readpicture` for a URI whose playing item has
    /// no image at that moment.
    NoExist = 50,
}

/// `ACK [<code>@<index>] {<command>} <message>`. `index` is the rank of the
/// command within a command list, 0 outside a list.
pub fn ack(code: Ack, index: usize, command: &str, message: &str) -> String {
    format!("ACK [{}@{index}] {{{command}}} {message}", code as u16)
}

/// A `key: value` reply line.
pub fn line(key: &str, value: impl Display) -> String {
    format!("{key}: {value}")
}

/// Splits a command line. Arguments are separated by spaces; a double-quoted
/// argument may contain some, and `\"` as well as `\\` are literals there.
///
/// An unclosed quote is `Ack::Arg` and not a tolerance: accepting the line
/// would execute a command whose argument is truncated, which is worse than a
/// readable rejection.
///
/// Its caller is the one that **reads lines**, hence the session.
/// `commands.rs` receives an already-split command — that is what lets it
/// have no I/O at all.
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
                    Some(other) => arg.push(other),
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
    fn simple_arguments_split_on_spaces() {
        assert_eq!(split("status").unwrap(), vec!["status"]);
        assert_eq!(split("play 3").unwrap(), vec!["play", "3"]);
        // Multiple spaces do not produce an empty argument.
        assert_eq!(split("play   3").unwrap(), vec!["play", "3"]);
    }

    #[test]
    fn a_quoted_argument_keeps_its_spaces() {
        assert_eq!(split(r#"load "France Inter""#).unwrap(), vec!["load", "France Inter"]);
    }

    #[test]
    fn escapes_inside_quotes() {
        // `\"` is a literal quote, `\\` a literal backslash.
        assert_eq!(split(r#"load "a \"name\"""#).unwrap(), vec!["load", r#"a "name""#]);
        assert_eq!(split(r#"load "a\\b""#).unwrap(), vec!["load", r"a\b"]);
    }

    #[test]
    fn an_unclosed_quote_is_an_invalid_argument() {
        assert_eq!(split(r#"load "France"#), Err(Ack::Arg));
    }

    #[test]
    fn an_empty_line_gives_no_argument() {
        assert!(split("").unwrap().is_empty());
        assert!(split("   ").unwrap().is_empty());
    }

    #[test]
    fn an_empty_quoted_argument_is_legal() {
        // `listplaylistinfo ""` must arrive as an empty name, not disappear.
        assert_eq!(split(r#"listplaylistinfo """#).unwrap(), vec!["listplaylistinfo", ""]);
    }

    #[test]
    fn a_tab_separates_arguments_like_a_space() {
        // The brief does not test it explicitly, but the implementation treats
        // `\t` as a separator on a par with ' ' (before quotes, in the skip
        // loop, and as the end of an unquoted argument): those three paths
        // deserve to be seen once.
        assert_eq!(split("play\t3").unwrap(), vec!["play", "3"]);
        assert_eq!(split("\tplay").unwrap(), vec!["play"]);
    }

    #[test]
    fn a_backslash_outside_quotes_is_literal() {
        // Outside quotes, `\` introduces no escape: it is an ordinary
        // character of the argument, on a par with a letter. An MPD client
        // that sends an unquoted Windows path (rare, but MALP allows it in
        // practice) must get it back intact.
        assert_eq!(split(r"load C:\music").unwrap(), vec!["load", r"C:\music"]);
    }

    #[test]
    fn a_trailing_backslash_inside_a_string_is_an_invalid_argument() {
        // The edge case the Task 4 review flagged: `"abc\` ends on a backslash
        // that calls for a character that does not exist. Tolerating it would
        // yield `abc`, hence a **truncated** argument presented as valid —
        // exactly what rejecting the unclosed quote avoids.
        assert_eq!(split(r#"load "abc\"#), Err(Ack::Arg));
        // And the variant where the escape eats the closing quote: the string
        // is then no longer closed at all.
        assert_eq!(split(r#"load "abc\""#), Err(Ack::Arg));
    }

    #[test]
    fn an_accented_name_survives_the_round_trip() {
        // French station names are accented: `Chérie FM` must come out
        // character for character. The splitting works on `char`s and not on
        // bytes, so an `é` is not cut in two — but nothing said so, and it is
        // the kind of property that breaks the day someone switches to bytes
        // to go faster.
        let line = r#"load "Chérie FM""#;
        assert_eq!(split(line).unwrap(), vec!["load", "Chérie FM"]);
        // An entirely non-ASCII name, quotes and spaces included.
        assert_eq!(split(r#"load "Radio Nova — Résonances""#).unwrap()[1], "Radio Nova — Résonances");
    }

    #[test]
    fn the_ack_carries_its_code_its_index_and_its_command() {
        assert_eq!(ack(Ack::NoExist, 0, "load", "no such playlist"), "ACK [50@0] {load} no such playlist");
        // The index is the rank within a command list.
        assert_eq!(ack(Ack::Arg, 2, "setvol", "invalid volume"), "ACK [2@2] {setvol} invalid volume");
    }

    #[test]
    fn line_formats_a_key_value_pair() {
        assert_eq!(line("volume", 42), "volume: 42");
        assert_eq!(line("state", "play"), "state: play");
    }
}
