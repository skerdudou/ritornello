//! Rewriting `plugins.toml` while keeping it readable by a human.
//!
//! The file is made of comments — what each plugin is for, why a metadata
//! order matters — and `deploy.sh` appends more of them. `toml_edit` preserves
//! them, which is why it is here rather than `toml`.
//!
//! **The trap, and it is not obvious.** A comment belongs to the key that
//! follows it, so moving a `[[plugin]]` moves its comment — which is right,
//! except for the first one. The real file opens with four lines describing
//! the *file*, then `[[plugin]]` with no blank line between: syntactically
//! indistinguishable from a comment about `radio`. Moving `radio` would carry
//! the file's header into the middle of the file.
//!
//! The rule: **what precedes the first plugin in the incoming document is the
//! header, and it stays first.** If that block contains a blank line, the cut
//! is at the last one — everything before it is the header, everything after
//! travels with the plugin. That covers the file `deploy.sh` produces, whose
//! first entry has both.
//!
//! Every function here is pure text in, text out: no path, no I/O. That is
//! what lets the CRLF case be a test rather than a hope.

use toml_edit::{DocumentMut, Item};

#[derive(Debug)]
pub enum EditError {
    Parse(String),
    NoPluginTable,
    NotDeclared(String),
    AlreadyDeclared(String),
    /// The fragment from the archive declares a name other than the one we
    /// asked to install.
    FragmentMismatch { expected: String, found: Option<String> },
    OutOfRange,
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(d) => write!(f, "plugins.toml could not be parsed: {d}"),
            Self::NoPluginTable => write!(f, "plugins.toml declares no [[plugin]] entry"),
            Self::NotDeclared(n) => write!(f, "plugin {n:?} is not declared in plugins.toml"),
            Self::AlreadyDeclared(n) => write!(f, "plugin {n:?} is already declared in plugins.toml"),
            Self::FragmentMismatch { expected, found } => write!(
                f,
                "the archive's plugins.toml block declares {found:?}, not {expected:?}"
            ),
            Self::OutOfRange => write!(f, "that plugin is already at the end it was asked to move towards"),
        }
    }
}

impl std::error::Error for EditError {}

fn parse(text: &str) -> Result<DocumentMut, EditError> {
    text.parse::<DocumentMut>().map_err(|e| EditError::Parse(e.to_string()))
}

fn name_of(table: &toml_edit::Table) -> Option<String> {
    table.get("name").and_then(|v| v.as_str()).map(str::to_string)
}

/// The declared names, in file order — which **is** the priority, both for the
/// source cycle and for metadata arbitration.
pub fn names_in_order(text: &str) -> Result<Vec<String>, EditError> {
    let doc = parse(text)?;
    let blocks = doc
        .get("plugin")
        .and_then(Item::as_array_of_tables)
        .ok_or(EditError::NoPluginTable)?;
    Ok(blocks.iter().filter_map(name_of).collect())
}

/// Splits the first table's leading decoration into (file header, the
/// plugin's own comment).
///
/// The cut is the **last** blank line: everything up to and including it
/// describes the file, everything after describes the plugin. With no blank
/// line the whole block is the header — which is the real file's case, and the
/// reason `radio` owns no comment of its own there.
fn split_header(prefix: &str) -> (String, String) {
    let normalized = prefix.replace("\r\n", "\n");
    match normalized.rfind("\n\n") {
        Some(at) => {
            let cut = at + 2;
            (prefix[..cut.min(prefix.len())].to_string(), prefix[cut.min(prefix.len())..].to_string())
        }
        None => (prefix.to_string(), String::new()),
    }
}

fn prefix_of(table: &toml_edit::Table) -> String {
    table
        .decor()
        .prefix()
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string()
}

fn set_prefix(table: &mut toml_edit::Table, prefix: &str) {
    let suffix = table.decor().suffix().and_then(|s| s.as_str()).unwrap_or("").to_string();
    *table.decor_mut() = toml_edit::Decor::new(prefix, suffix);
}

/// Appends the `[[plugin]]` block an archive carries, at the end of the list.
///
/// `expected` is checked against the block's own `name`: the fragment comes
/// from inside a downloaded archive, and trusting it to declare the plugin we
/// asked for would let an archive declare something else entirely.
pub fn append_block(text: &str, fragment: &str, expected: &str) -> Result<String, EditError> {
    let fragment_doc = parse(fragment)?;
    let incoming = fragment_doc
        .get("plugin")
        .and_then(Item::as_array_of_tables)
        .and_then(|a| a.get(0).cloned())
        .ok_or(EditError::NoPluginTable)?;
    let found = name_of(&incoming);
    if found.as_deref() != Some(expected) {
        return Err(EditError::FragmentMismatch { expected: expected.to_string(), found });
    }

    let mut doc = parse(text)?;
    let blocks = doc
        .get_mut("plugin")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or(EditError::NoPluginTable)?;
    if blocks.iter().any(|t| name_of(t).as_deref() == Some(expected)) {
        return Err(EditError::AlreadyDeclared(expected.to_string()));
    }
    let mut incoming = incoming;
    // `toml_edit` renders array-of-tables entries by their own `doc_position`
    // (falling back to the previous entry's when unset), NOT by their index in
    // the `Vec` — a table cloned from another document keeps that document's
    // position, and would render there instead of at the end. Clearing it
    // makes this entry inherit whatever comes right before it once pushed.
    incoming.set_position(None);
    // One blank line before the block, whatever the fragment's own leading
    // decoration was: the separator belongs to this file's layout, not to the
    // archive's. Unlike `move_entry`'s use of `split_header`, the fragment is
    // a single entry — there is no larger "file" for its own leading comment
    // to be a header OF, so the whole prefix is that plugin's own comment.
    let own = prefix_of(&incoming);
    set_prefix(&mut incoming, &format!("\n{}", own.trim_start_matches('\n')));
    blocks.push(incoming);
    Ok(doc.to_string())
}

pub fn remove_entry(text: &str, name: &str) -> Result<String, EditError> {
    let mut doc = parse(text)?;
    let blocks = doc
        .get_mut("plugin")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or(EditError::NoPluginTable)?;
    let at = blocks
        .iter()
        .position(|t| name_of(t).as_deref() == Some(name))
        .ok_or_else(|| EditError::NotDeclared(name.to_string()))?;
    // Removing the first entry would take the file header with it, exactly as
    // moving it would. Hand the header to whoever becomes first.
    let header = if at == 0 {
        let (header, _) = split_header(&prefix_of(blocks.get(0).expect("index 0 exists")));
        Some(header)
    } else {
        None
    };
    blocks.remove(at);
    if let (Some(header), Some(first)) = (header, blocks.get_mut(0)) {
        // The new first entry's prefix is already, in full, its own comment —
        // never a header candidate — so `split_header` must not be applied to
        // it a second time: with no blank line of its own (as in the real
        // file, e.g. `cd`'s comment), it would read as "all header, no
        // comment" and drop it, the same trap `move_entry` hit.
        let comment = prefix_of(first);
        set_prefix(first, &format!("{header}{}", comment.trim_start_matches('\n')));
    }
    Ok(doc.to_string())
}

/// Moves one entry by `delta` positions. `-1` is up, `+1` is down.
///
/// Refused rather than clamped at either end: the page disables the arrow
/// there, so a request that arrives anyway is a bug or a stale page, and
/// answering "done" to it would be a lie.
pub fn move_entry(text: &str, name: &str, delta: i32) -> Result<String, EditError> {
    let mut doc = parse(text)?;
    let blocks = doc
        .get_mut("plugin")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or(EditError::NoPluginTable)?;
    let from = blocks
        .iter()
        .position(|t| name_of(t).as_deref() == Some(name))
        .ok_or_else(|| EditError::NotDeclared(name.to_string()))?;
    let to = i64::try_from(from).expect("a plugin count fits in i64") + i64::from(delta);
    if to < 0 || to as usize >= blocks.len() {
        return Err(EditError::OutOfRange);
    }
    let to = to as usize;

    // Lift the file header off whoever is first, before anything moves.
    let (header, first_comment) = split_header(&prefix_of(blocks.get(0).expect("non-empty")));
    {
        let first = blocks.get_mut(0).expect("non-empty");
        set_prefix(first, &first_comment);
    }

    let mut taken = blocks.get(from).expect("found above").clone();
    // `toml_edit` renders array-of-tables entries by their own `doc_position`
    // (falling back to the previous entry's when unset), NOT by their index in
    // the `Vec` that `remove`/`insert` below reorder: without clearing it, the
    // moved table would keep rendering at its OLD spot regardless of where it
    // now sits in the array.
    taken.set_position(None);
    // A moved block keeps its own comment and gets this file's separator.
    // `split_header` is not needed here even when `from == 0`: the header-lift
    // above already removed the file's header from whichever table was first,
    // so what remains on `taken`'s prefix — first or not — is only ever its
    // own comment, never a header candidate.
    let own = prefix_of(&taken);
    set_prefix(&mut taken, &format!("\n{}", own.trim_start_matches('\n')));
    blocks.remove(from);
    // `insert` shifts the rest right, which is what both directions want once
    // the source has been removed.
    blocks.insert(to, taken);

    // Give the header back to whoever is first now. Its current prefix is
    // already exactly its own comment and nothing else — `split_header` would
    // wrongly read a comment with no blank line of its own as "all header,
    // no comment" and drop it, the same trap as above.
    let first = blocks.get_mut(0).expect("non-empty");
    let comment = prefix_of(first);
    set_prefix(first, &format!("{header}{}", comment.trim_start_matches('\n')));
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real file's shape: a four-line header glued to the first
    /// `[[plugin]]` with no blank line, then one commented block per plugin.
    fn realistic() -> String {
        "\
# Each entry only needs `name` and `exec`. A third key, `enabled`, appears
# when a plugin is switched off from the configuration page; its absence
# means active.
[[plugin]]
name = \"radio\"
exec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-radio\"

# Its page carries one setting: what happens on arriving at this source.
[[plugin]]
name = \"cd\"
exec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-cd\"

[[plugin]]
name = \"musicbrainz\"
exec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-musicbrainz\"
"
        .to_string()
    }

    #[test]
    fn names_are_read_in_file_order_because_that_order_is_the_priority() {
        assert_eq!(names_in_order(&realistic()).unwrap(), vec!["radio", "cd", "musicbrainz"]);
    }

    #[test]
    fn a_block_is_appended_at_the_end_with_its_own_comment() {
        let fragment = "\
# NRJ group webradios, by stream token.
[[plugin]]
name = \"nrj-metas\"
exec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-nrj-metas\"
";
        let out = append_block(&realistic(), fragment, "nrj-metas").unwrap();
        assert_eq!(
            names_in_order(&out).unwrap(),
            vec!["radio", "cd", "musicbrainz", "nrj-metas"]
        );
        // Appended at the end, so a newly installed metadata plugin arrives
        // with the LOWEST priority and cannot take the stage from what already
        // works. Moving it up afterwards is a separate, deliberate gesture.
        assert!(out.contains("# NRJ group webradios"), "the comment travelled with it");
        assert!(out.trim_end().ends_with("ritornello-plugin-nrj-metas\""), "{out}");
    }

    #[test]
    fn appending_a_name_already_declared_is_refused() {
        let fragment = "[[plugin]]\nname = \"cd\"\nexec = \"/x\"\n";
        assert!(matches!(
            append_block(&realistic(), fragment, "cd"),
            Err(EditError::AlreadyDeclared(_))
        ));
    }

    #[test]
    fn a_fragment_whose_name_is_not_the_expected_one_is_refused() {
        // The fragment comes from inside a downloaded archive. Trusting it to
        // declare the plugin we asked for would let an archive named
        // `ritornello-plugin-radio-…` declare something else entirely.
        let fragment = "[[plugin]]\nname = \"somethingelse\"\nexec = \"/x\"\n";
        assert!(matches!(
            append_block(&realistic(), fragment, "nrj-metas"),
            Err(EditError::FragmentMismatch { .. })
        ));
    }

    #[test]
    fn an_entry_is_removed_and_the_others_keep_their_comments() {
        let out = remove_entry(&realistic(), "cd").unwrap();
        assert_eq!(names_in_order(&out).unwrap(), vec!["radio", "musicbrainz"]);
        assert!(!out.contains("arriving at this source"), "its comment left with it");
        assert!(out.contains("Each entry only needs"), "the file header stayed");
    }

    #[test]
    fn removing_a_name_that_is_not_declared_is_refused() {
        assert!(matches!(remove_entry(&realistic(), "nope"), Err(EditError::NotDeclared(_))));
    }

    /// Removing the FIRST plugin is the same trap as moving it: the file
    /// header must reach the new first entry, but that entry's own comment —
    /// here `cd`'s, which has no blank line of its own — must not be read as
    /// a second header and dropped in the process.
    #[test]
    fn removing_the_first_plugin_hands_the_header_to_the_next_one_without_losing_its_comment() {
        let out = remove_entry(&realistic(), "radio").unwrap();
        assert_eq!(names_in_order(&out).unwrap(), vec!["cd", "musicbrainz"]);
        assert!(
            out.trim_start().starts_with("# Each entry only needs"),
            "the header did not reach the new first entry:\n{out}"
        );
        assert!(
            out.contains("arriving at this source"),
            "cd's own comment was lost when it inherited the header:\n{out}"
        );
    }

    /// The trap, stated as the test that proves it fixed: moving the first
    /// plugin must not take the file's own header along with it.
    #[test]
    fn moving_the_first_plugin_leaves_the_file_header_at_the_top() {
        let out = move_entry(&realistic(), "radio", 1).unwrap();
        assert_eq!(names_in_order(&out).unwrap(), vec!["cd", "radio", "musicbrainz"]);
        assert!(
            out.trim_start().starts_with("# Each entry only needs"),
            "the header moved with radio:\n{out}"
        );
        // And radio kept nothing that was not its own — it had no comment of
        // its own in this file.
        let radio_at = out.find("name = \"radio\"").unwrap();
        let header_at = out.find("Each entry only needs").unwrap();
        assert!(header_at < radio_at);
    }

    #[test]
    fn a_plugin_with_its_own_comment_keeps_it_when_it_moves() {
        let out = move_entry(&realistic(), "cd", -1).unwrap();
        assert_eq!(names_in_order(&out).unwrap(), vec!["cd", "radio", "musicbrainz"]);
        let comment_at = out.find("arriving at this source").unwrap();
        let cd_at = out.find("name = \"cd\"").unwrap();
        assert!(comment_at < cd_at, "cd's own comment did not travel with it:\n{out}");
    }

    /// The shape `deploy.sh` actually produces: the file header is itself two
    /// paragraphs (a blank line inside it, for readability), then a further
    /// blank line, then the first plugin's own one-line comment. The cut must
    /// be at the LAST blank line: cutting at the first would read the
    /// header's own second paragraph as `radio`'s comment and carry it away
    /// when `radio` moves.
    #[test]
    fn a_multi_paragraph_header_stays_whole_when_the_first_plugin_moves() {
        let doc = "\
# File header, paragraph one.

# File header, paragraph two.

# radio's own comment.
[[plugin]]
name = \"radio\"
exec = \"/x\"

[[plugin]]
name = \"cd\"
exec = \"/y\"
";
        let out = move_entry(doc, "radio", 1).unwrap();
        assert_eq!(names_in_order(&out).unwrap(), vec!["cd", "radio"]);
        assert!(
            out.trim_start().starts_with("# File header, paragraph one."),
            "the header's first paragraph did not stay at the top:\n{out}"
        );
        let header2_at = out.find("File header, paragraph two.").unwrap();
        let cd_at = out.find("name = \"cd\"").unwrap();
        assert!(
            header2_at < cd_at,
            "the header's second paragraph must stay at the top, not travel with radio:\n{out}"
        );
        let comment_at = out.find("radio's own comment").unwrap();
        let radio_at = out.find("name = \"radio\"").unwrap();
        assert!(comment_at < radio_at, "radio's own comment must travel with it:\n{out}");
    }

    #[test]
    fn moving_past_either_end_is_refused_rather_than_clamped() {
        // Refused and not silently clamped: the page disables the arrow at the
        // ends, so a request that arrives anyway is a bug or a stale page, and
        // answering "done" to it would be a lie.
        assert!(matches!(move_entry(&realistic(), "radio", -1), Err(EditError::OutOfRange)));
        assert!(matches!(move_entry(&realistic(), "musicbrainz", 1), Err(EditError::OutOfRange)));
    }

    /// This checkout has `core.autocrlf=true`, and a CRLF tangle has already
    /// silently cancelled a mutation in this repository. Every transformation
    /// must survive both endings.
    #[test]
    fn every_transformation_works_on_crlf_too() {
        let crlf = realistic().replace('\n', "\r\n");
        assert_eq!(names_in_order(&crlf).unwrap(), vec!["radio", "cd", "musicbrainz"]);

        let moved = move_entry(&crlf, "radio", 1).unwrap();
        assert_eq!(names_in_order(&moved).unwrap(), vec!["cd", "radio", "musicbrainz"]);
        assert!(moved.trim_start().starts_with("# Each entry only needs"), "{moved:?}");

        let removed = remove_entry(&crlf, "cd").unwrap();
        assert_eq!(names_in_order(&removed).unwrap(), vec!["radio", "musicbrainz"]);

        let fragment = "[[plugin]]\r\nname = \"mpd\"\r\nexec = \"/x\"\r\n";
        let appended = append_block(&crlf, fragment, "mpd").unwrap();
        assert_eq!(names_in_order(&appended).unwrap(), vec!["radio", "cd", "musicbrainz", "mpd"]);
    }

    #[test]
    fn an_enabled_false_key_survives_a_move() {
        let with_key = realistic().replace(
            "name = \"cd\"",
            "name = \"cd\"\nenabled = false",
        );
        let out = move_entry(&with_key, "cd", -1).unwrap();
        assert!(out.contains("enabled = false"), "the switch was lost:\n{out}");
    }

    #[test]
    fn a_document_without_any_plugin_is_refused_clearly() {
        assert!(matches!(names_in_order("# empty\n"), Err(EditError::NoPluginTable)));
    }
}
