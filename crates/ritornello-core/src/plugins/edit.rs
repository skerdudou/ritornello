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
//! travels with the plugin. This covers a header written with an internal
//! paragraph break — not something `deploy.sh` itself produces (it only ever
//! copies the example file verbatim or appends whole blocks, neither of
//! which puts a blank line inside the header), but a shape a hand-edited
//! file can still have, and this module must still handle it correctly.
//!
//! **The residual ambiguity, stated rather than hidden.** With no blank line
//! at all, "a file header glued to the first plugin" and "the first plugin's
//! own comment, no header" are the same bytes — the shipped file's own
//! `radio` entry is exactly this shape. The code always guesses "header",
//! which is right for the shipped file and is the safer of the two wrong
//! guesses: a header wrongly read as a comment travels away from the top the
//! first time that plugin moves, where a comment wrongly read as a header
//! merely stays put. `append_block` disambiguates the one case it fully
//! controls — a hand-written fragment's own comment, installed with no
//! header to attach to — by giving it a real blank-line boundary instead of
//! emitting it bare (see the `None` branch there); it cannot and does not
//! attempt to disambiguate pre-existing files it did not write.
//!
//! **What cannot be fixed at all:** a plugin's own comment that itself
//! contains a blank line, once that plugin is first, is indistinguishable
//! from "part header, part comment" — the "cut at the last blank line" rule
//! has no way to know the blank line is a paragraph break inside one
//! person's prose rather than a boundary between two different things.
//! Normalising an operator's blank line into a `#` line to sidestep this
//! would be a bigger liberty than the defect deserves, so it is left alone
//! and pinned as today's behaviour instead (see
//! `a_first_plugins_own_multi_paragraph_comment_is_partly_read_as_header`).
//! The shipped file avoids it by separating every multi-paragraph comment
//! with `#` lines rather than blank ones (see `musicbrainz`'s and `files`'
//! comments in `deploy/plugins.example.toml`), which is why nothing is
//! affected today.
//!
//! Every function here is pure text in, text out: no path, no I/O. That is
//! what lets the CRLF case be a test rather than a hope.

use toml_edit::{ArrayOfTables, DocumentMut, Item};

#[derive(Debug)]
pub enum EditError {
    Parse(String),
    NoPluginTable,
    NotDeclared(String),
    AlreadyDeclared(String),
    /// The fragment from the archive declares a name other than the one we
    /// asked to install.
    FragmentMismatch { expected: String, found: Option<String> },
    /// The fragment declares more than one plugin. Its own variant rather than
    /// `FragmentMismatch { found: None }`, which would report "declares None"
    /// about a block that declared two — sending the reader after a malformed
    /// fragment instead of an over-full one.
    FragmentDeclaresSeveral { expected: String },
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
            Self::FragmentDeclaresSeveral { expected } => write!(
                f,
                "the archive's plugins.toml block declares more than one plugin, not just {expected:?}"
            ),
            Self::OutOfRange => write!(f, "that plugin is already at the end it was asked to move towards"),
        }
    }
}

impl std::error::Error for EditError {}

/// Normalises `\r\n` to `\n` before anything else touches the text.
///
/// States an invariant, not just a fix for one function's arithmetic: every
/// byte index computed anywhere in this module refers to `\n`-only text. The
/// defect this exists to prevent was exactly an index computed against one
/// representation of a string and then applied to a different one — so the
/// invariant is worth holding here, at the one entry point every public
/// function shares, independently of whether any single downstream function
/// still needs it. It is lossless in practice, since `toml_edit` emits `\n`
/// regardless of what it read.
///
/// `split_header` (below) no longer mixes representations either, which on
/// its own would already prevent the original panic. **Neither guard is
/// pinned by a test on its own** — reverting either alone leaves the other
/// holding, and only removing both together reproduces it (see that test's
/// doc comment). This is deliberate, not an oversight: keep both.
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn parse(text: &str) -> Result<DocumentMut, EditError> {
    text.parse::<DocumentMut>().map_err(|e| EditError::Parse(e.to_string()))
}

fn name_of(table: &toml_edit::Table) -> Option<String> {
    table.get("name").and_then(|v| v.as_str()).map(str::to_string)
}

/// The declared names, in file order — which **is** the priority, both for the
/// source cycle and for metadata arbitration.
pub fn names_in_order(text: &str) -> Result<Vec<String>, EditError> {
    let text = normalize(text);
    let doc = parse(&text)?;
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
///
/// Assumes `prefix` is already `\n`-only (every caller normalises at its own
/// entry point, see `normalize`): searching and slicing the same string is
/// what keeps the byte offset honest.
///
/// This is a local correctness fix, not a duplicate of `normalize`'s
/// invariant: it no longer needs both representations to agree, because it
/// stops mixing them (the old version searched a normalised copy but sliced
/// the original). `normalize` is still worth keeping independently — it holds
/// the invariant for the whole module, not just this one function's
/// arithmetic. **Neither guard is pinned by a test on its own**: reverting
/// either alone leaves the other holding, and only removing both together
/// reproduces the original panic (see
/// `crlf_with_a_blank_line_in_the_header_and_a_non_ascii_character_does_not_panic`).
/// Deliberate, not an oversight — keep both.
fn split_header(prefix: &str) -> (String, String) {
    match prefix.rfind("\n\n") {
        Some(at) => {
            let cut = at + 2;
            (prefix[..cut].to_string(), prefix[cut..].to_string())
        }
        None => (prefix.to_string(), String::new()),
    }
}

/// Joins a lifted file header with the comment of whoever becomes first, with
/// exactly one blank line between them when both are non-empty — the same
/// shape `split_header` cuts on.
///
/// This is what keeps the header and a plugin's own comment distinguishable
/// across any number of operations. Without a separator, a header handed to a
/// table that already carries its own comment glues onto it seamlessly; the
/// next operation's `split_header` then finds no blank line, reads the WHOLE
/// combined block as pure header, and carries it entirely along with whatever
/// moves next. Nothing is deleted when this happens — a plugin's comment
/// simply migrates onto its neighbour, silently, one operation at a time.
///
/// When the table taking the header has no comment of its own, the header is
/// attached with no trailing blank line, never growing one on repeated
/// extraction/reattachment (see `trim_trailing_blank_line`): a table with an
/// empty comment must render exactly as a bare header directly above
/// `[[plugin]]`, matching the shape of the real file's own `radio` entry.
fn glue_header(header: &str, comment: &str) -> String {
    let comment = comment.trim_start_matches('\n');
    if comment.is_empty() {
        // A header that already has an internal blank line (more than one
        // paragraph) must not have its trailing one trimmed: doing so would
        // leave that internal blank line as the ONLY one left, and the next
        // `split_header` would then cut there, reading the header's own
        // second paragraph as this table's comment.
        return if header.trim_end_matches('\n').contains("\n\n") {
            header.to_string()
        } else {
            trim_trailing_blank_line(header)
        };
    }
    // A header made only of newline characters — the marker `append_block`
    // writes in place of a genuinely absent header (see there) — carries no
    // real content: treat it exactly like an empty header rather than
    // perpetuating it as a stray blank line every time something new becomes
    // first.
    if header.chars().all(|c| c == '\n') {
        return comment.to_string();
    }
    if header.ends_with("\n\n") {
        format!("{header}{comment}")
    } else if header.ends_with('\n') {
        format!("{header}\n{comment}")
    } else {
        format!("{header}\n\n{comment}")
    }
}

/// Collapses a trailing blank line (`"\n\n"`) down to a single trailing
/// newline, leaving anything else untouched.
///
/// `split_header` always returns a `header` that either ends with a blank
/// line (one was found) or ends with a single newline (none was — the header
/// is the whole prefix). Reattaching it to a table with no comment of its own
/// must reproduce the second shape either way, or an extraction followed by a
/// reattachment with nothing to separate would grow the header by one blank
/// line every round trip.
fn trim_trailing_blank_line(s: &str) -> String {
    match s.strip_suffix("\n\n") {
        Some(stripped) => format!("{stripped}\n"),
        None => s.to_string(),
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

/// `prefix_of`, stripped of any leading blank line — a table's own comment
/// and nothing else, regardless of whether it currently sits first (no
/// leading blank line) or not (one leading blank line, by convention).
fn bare_comment(table: &toml_edit::Table) -> String {
    prefix_of(table).trim_start_matches('\n').to_string()
}

fn set_prefix(table: &mut toml_edit::Table, prefix: &str) {
    let suffix = table.decor().suffix().and_then(|s| s.as_str()).unwrap_or("").to_string();
    *table.decor_mut() = toml_edit::Decor::new(prefix, suffix);
}

/// Appends the `[[plugin]]` block an archive carries, at the end of the list.
///
/// `expected` is checked against the block's own `name`: the fragment comes
/// from inside a downloaded archive, and trusting it to declare the plugin we
/// asked for would let an archive declare something else entirely. A fragment
/// carrying more than one entry is refused too (`FragmentDeclaresSeveral`):
/// taking only the first and silently dropping the rest would hide that the
/// archive did not cleanly declare the one plugin asked for.
///
/// A file with no `[[plugin]]` entry at all is not refused: it covers both a
/// genuinely fresh installation and a file that just had its last plugin
/// removed, header preserved in the document's trailing slot by
/// `remove_entry` — this is the only way back from that state.
pub fn append_block(text: &str, fragment: &str, expected: &str) -> Result<String, EditError> {
    let fragment = normalize(fragment);
    let text = normalize(text);

    let fragment_doc = parse(&fragment)?;
    let mut fragment_blocks =
        fragment_doc.get("plugin").and_then(Item::as_array_of_tables).ok_or(EditError::NoPluginTable)?.iter();
    let incoming = fragment_blocks.next().ok_or(EditError::NoPluginTable)?.clone();
    if fragment_blocks.next().is_some() {
        return Err(EditError::FragmentDeclaresSeveral { expected: expected.to_string() });
    }
    let found = name_of(&incoming);
    if found.as_deref() != Some(expected) {
        return Err(EditError::FragmentMismatch { expected: expected.to_string(), found });
    }

    let mut doc = parse(&text)?;
    let mut incoming = incoming;
    // `toml_edit` renders array-of-tables entries by their own `doc_position`
    // (falling back to the previous entry's when unset), NOT by their index in
    // the `Vec` — a table cloned from another document keeps that document's
    // position, and would render there instead of at the end. Clearing it
    // makes this entry inherit whatever comes right before it once pushed.
    incoming.set_position(None);

    match doc.get_mut("plugin").and_then(Item::as_array_of_tables_mut) {
        Some(blocks) => {
            if blocks.iter().any(|t| name_of(t).as_deref() == Some(expected)) {
                return Err(EditError::AlreadyDeclared(expected.to_string()));
            }
            // One blank line before the block, whatever the fragment's own
            // leading decoration was: the separator belongs to this file's
            // layout, not to the archive's. Unlike `move_entry`'s use of
            // `split_header`, the fragment is a single entry — there is no
            // larger "file" for its own leading comment to be a header OF, so
            // the whole prefix is that plugin's own comment.
            let own = bare_comment(&incoming);
            set_prefix(&mut incoming, &format!("\n{own}"));
            blocks.push(incoming);
        }
        None => {
            // No plugin table at all. Any header preserved by `remove_entry`
            // when the last plugin was removed lives in the document's
            // trailing slot — the same slot a comment-only document round
            // trips through.
            let header = doc.trailing().as_str().unwrap_or("").to_string();
            let own = bare_comment(&incoming);
            let prefix = if header.is_empty() {
                if own.is_empty() {
                    String::new()
                } else {
                    // There is no parked header to attach this comment to
                    // (archive fragments never carry one — see the module
                    // doc — so `own` non-empty here only ever comes from a
                    // hand-written fragment). Emitting it bare, with no
                    // leading blank line, would be structurally IDENTICAL to
                    // "a file header glued to the first plugin": exactly the
                    // shape a later `split_header` reads as a header, which
                    // would carry this plugin's own description away to
                    // whoever displaces it, permanently, rather than staying
                    // with the plugin it describes.
                    //
                    // A leading blank line of two newlines gives
                    // `split_header` an actual boundary to cut at, so it
                    // recovers this text as the plugin's own comment on any
                    // later read. `glue_header` treats a header made only of
                    // newlines as no header at all (see there), so this
                    // marker does not linger once some other plugin becomes
                    // first — it is consumed the moment that happens.
                    format!("\n\n{own}")
                }
            } else {
                // A parked header IS attached with a blank line before the
                // incoming comment, via `glue_header` — the same shape it
                // produces everywhere else a header meets a comment.
                glue_header(&header, &own)
            };
            set_prefix(&mut incoming, &prefix);
            let mut fresh = ArrayOfTables::new();
            fresh.push(incoming);
            doc.insert("plugin", Item::ArrayOfTables(fresh));
            doc.set_trailing("");
        }
    }
    Ok(doc.to_string())
}

/// Removes the entry named `name`.
///
/// Removing every plugin does not lose the file's header: with nobody left to
/// carry it, it is stashed in the document's own trailing slot (the same slot
/// a comment-only document round-trips through) so `append_block` can find it
/// again — otherwise uninstalling the last plugin would be a one-way door,
/// since `append_block` requires a `[[plugin]]` table to attach to.
pub fn remove_entry(text: &str, name: &str) -> Result<String, EditError> {
    let text = normalize(text);
    let mut doc = parse(&text)?;
    let blocks = doc
        .get_mut("plugin")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or(EditError::NoPluginTable)?;
    let at = blocks
        .iter()
        .position(|t| name_of(t).as_deref() == Some(name))
        .ok_or_else(|| EditError::NotDeclared(name.to_string()))?;
    // Removing the first entry would take the file header with it, exactly as
    // moving it would. Hand the header to whoever becomes first — or, if
    // nobody does, to the document's trailing slot.
    let header = if at == 0 {
        Some(split_header(&prefix_of(blocks.get(0).expect("index 0 exists"))).0)
    } else {
        None
    };
    blocks.remove(at);
    match (header, blocks.get_mut(0)) {
        (Some(header), Some(first)) => {
            // `glue_header`, not a direct concatenation: gluing the header
            // straight onto the new first entry's own comment with no
            // separator is exactly the defect a round trip (move down, move
            // back up) exposed — the next `split_header` cannot tell the two
            // apart any more and carries both away together.
            let comment = bare_comment(first);
            set_prefix(first, &glue_header(&header, &comment));
        }
        (Some(header), None) => {
            doc.remove("plugin");
            // Append, don't replace: the document may already carry
            // something in its trailing slot — an end-of-file comment, say
            // — and the header belongs BEFORE it, in the same top-to-bottom
            // order it had when the plugin that carried it still stood
            // between the two.
            let existing = doc.trailing().as_str().unwrap_or("").to_string();
            doc.set_trailing(format!("{header}{existing}"));
        }
        (None, _) => {}
    }
    Ok(doc.to_string())
}

/// Moves one entry by `delta` positions. `-1` is up, `+1` is down.
///
/// Refused rather than clamped at either end: the page disables the arrow
/// there, so a request that arrives anyway is a bug or a stale page, and
/// answering "done" to it would be a lie.
pub fn move_entry(text: &str, name: &str, delta: i32) -> Result<String, EditError> {
    let text = normalize(text);
    let mut doc = parse(&text)?;
    let blocks = doc
        .get_mut("plugin")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or(EditError::NoPluginTable)?;
    let from = blocks
        .iter()
        .position(|t| name_of(t).as_deref() == Some(name))
        .ok_or_else(|| EditError::NotDeclared(name.to_string()))?;
    let to = i64::try_from(from).expect("a plugin count fits in i64") + i64::from(delta);
    // Not just a nicer error than a library panic: `ArrayOfTables::insert`
    // below panics if given an index past the end, so this guard is the only
    // thing standing between a caller's bad index and a panic inside
    // `toml_edit` rather than the `OutOfRange` a caller can actually handle.
    if to < 0 || to as usize >= blocks.len() {
        return Err(EditError::OutOfRange);
    }
    let to = to as usize;

    // Lift the file header off whoever is CURRENTLY first — only that table
    // can carry one.
    let (header, first_own) = split_header(&prefix_of(blocks.get(0).expect("non-empty")));
    set_prefix(blocks.get_mut(0).expect("non-empty"), &first_own);

    let mut taken = blocks.get(from).expect("found above").clone();
    // `toml_edit` renders array-of-tables entries by their own `doc_position`
    // (falling back to the previous entry's when unset), NOT by their index in
    // the `Vec` that `remove`/`insert` below reorder: without clearing it, the
    // moved table would keep rendering at its OLD spot regardless of where it
    // now sits in the array.
    taken.set_position(None);
    blocks.remove(from);
    // `insert` shifts the rest right, which is what both directions want once
    // the source has been removed.
    blocks.insert(to, taken);

    // Decide every entry's separator from scratch, based only on where it
    // ends up — never on where it used to be. Whoever is first now gets the
    // header (glued with exactly one blank line before its own comment, or
    // none if it has none); everyone else gets exactly one leading blank
    // line before their own comment, full stop.
    //
    // This uniform pass is what fixes the table displaced FROM first place:
    // treating index 0 as a special case only going IN (lifting the header)
    // and not coming OUT left whichever table took over from the old first
    // one keeping its bare, no-leading-blank-line prefix even after sliding
    // down to a non-first position — glued directly onto the entry above it.
    // It is also what makes an operation followed by its own undo restore
    // the file byte for byte: nothing here depends on history, only on the
    // final arrangement.
    for i in 0..blocks.len() {
        let table = blocks.get_mut(i).expect("index in range");
        let comment = bare_comment(table);
        let prefix = if i == 0 { glue_header(&header, &comment) } else { format!("\n{comment}") };
        set_prefix(table, &prefix);
    }
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

    /// Only the first entry would be taken and the second silently dropped
    /// otherwise. Silence is the problem: the caller asked to install exactly
    /// one plugin, and a fragment declaring more than one no longer says
    /// clearly which.
    #[test]
    fn a_fragment_carrying_more_than_one_entry_is_refused() {
        let fragment = "\
[[plugin]]
name = \"nrj-metas\"
exec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-nrj-metas\"

[[plugin]]
name = \"radiofrance-metas\"
exec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-radiofrance-metas\"
";
        assert!(matches!(
            append_block(&realistic(), fragment, "nrj-metas"),
            Err(EditError::FragmentDeclaresSeveral { .. })
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

    /// Uninstalling the last plugin must not be a one-way door: the file
    /// header has nobody left to attach to, so it is stashed in the
    /// document's own trailing slot — the same slot a comment-only document
    /// round-trips through — and `append_block` must be able to find it
    /// again and hand it to the first plugin installed afterwards.
    #[test]
    fn removing_every_plugin_preserves_the_header_for_the_next_append() {
        let doc = "\
# Each entry only needs `name` and `exec`.
[[plugin]]
name = \"radio\"
exec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-radio\"
";
        let after_remove = remove_entry(doc, "radio").unwrap();
        assert!(
            matches!(names_in_order(&after_remove), Err(EditError::NoPluginTable)),
            "no plugin should remain declared: {after_remove:?}"
        );
        assert!(
            after_remove.contains("Each entry only needs"),
            "the header must survive with no plugin left:\n{after_remove}"
        );

        let fragment = "# A freshly installed plugin.\n[[plugin]]\nname = \"mpd\"\nexec = \"/y\"\n";
        let restored = append_block(&after_remove, fragment, "mpd").unwrap();
        assert_eq!(names_in_order(&restored).unwrap(), vec!["mpd"]);
        assert!(
            restored.trim_start().starts_with("# Each entry only needs"),
            "the header did not reach the newly installed plugin:\n{restored}"
        );
        assert!(restored.contains("A freshly installed plugin"), "{restored}");
    }

    /// `append_block` must also work directly on a document that never had a
    /// `[[plugin]]` table at all — not only one that had its header rescued
    /// by `remove_entry`.
    #[test]
    fn appending_to_a_file_with_no_plugin_table_creates_one() {
        let fragment = "# The web tuner.\n[[plugin]]\nname = \"radio\"\nexec = \"/x\"\n";
        let out = append_block("", fragment, "radio").unwrap();
        assert_eq!(names_in_order(&out).unwrap(), vec!["radio"]);
        assert!(out.contains("The web tuner"), "{out}");
        assert!(out.trim_start().starts_with("# The web tuner."), "no stray separator on a fresh file:\n{out:?}");
    }

    /// And on a document that is nothing but a comment — the shape a
    /// comment-only `plugins.toml` round-trips through in `toml_edit`.
    #[test]
    fn appending_to_a_comment_only_file_keeps_the_comment_as_header() {
        let doc = "# Nothing installed yet.\n";
        let fragment = "# The web tuner.\n[[plugin]]\nname = \"radio\"\nexec = \"/x\"\n";
        let out = append_block(doc, fragment, "radio").unwrap();
        assert_eq!(names_in_order(&out).unwrap(), vec!["radio"]);
        assert!(
            out.trim_start().starts_with("# Nothing installed yet."),
            "the pre-existing comment must stay first:\n{out}"
        );
        assert!(out.contains("The web tuner"), "{out}");
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

    /// Move a plugin down, then back up: the most natural undo there is. The
    /// mechanism this pins is a header handed to a table that already has its
    /// own comment with no separator between them — the next `split_header`
    /// then reads the whole glued block as pure header, so the comment stays
    /// behind (glued to the header) while its plugin moves away. Nothing is
    /// deleted, so `contains(...)` cannot see it: only a byte-for-byte
    /// comparison of the round trip can.
    #[test]
    fn moving_a_plugin_down_then_back_up_restores_the_file_byte_for_byte() {
        let doc = realistic();
        let down = move_entry(&doc, "radio", 1).unwrap();
        let back = move_entry(&down, "radio", -1).unwrap();
        assert_eq!(back, doc, "the round trip did not restore the original file");
    }

    /// The same property, on the actual deployed file rather than a fixture
    /// that only mirrors its shape: this is where the defect was first
    /// measured (`cd`'s description ending up above `radio`).
    ///
    /// The file on disk is CRLF (this checkout has `core.autocrlf=true`), but
    /// every transformation normalises to `\n` on entry and `toml_edit` emits
    /// `\n` regardless of what it read — so the round trip is compared
    /// against the file's own LF-normalised content, not its raw bytes; a
    /// changed line ending is not the defect this test is watching for.
    #[test]
    fn the_real_example_file_survives_a_move_down_then_back_up() {
        let doc = include_str!("../../../../deploy/plugins.example.toml").replace("\r\n", "\n");
        let down = move_entry(&doc, "radio", 1).unwrap();
        let back = move_entry(&down, "radio", -1).unwrap();
        assert_eq!(back, doc, "the round trip did not restore deploy/plugins.example.toml");
    }

    /// Same file, the other reported scenario: remove `radio`, then move `cd`
    /// down. `cd`'s own three-line description must stay immediately above
    /// `cd`, not drift onto `files` (which takes `cd`'s old spot) or onto
    /// whoever `cd` displaces.
    #[test]
    fn the_real_example_file_keeps_comments_with_their_plugin_after_remove_then_move() {
        let doc = include_str!("../../../../deploy/plugins.example.toml");
        let after_remove = remove_entry(doc, "radio").unwrap();
        let out = move_entry(&after_remove, "cd", 1).unwrap();
        let comment_at = out.find("Its page carries one setting").unwrap();
        let cd_at = out.find("name = \"cd\"").unwrap();
        assert!(
            comment_at < cd_at && cd_at - comment_at < 400,
            "cd's own comment drifted away from cd:\n{out}"
        );
        // And the file header — four lines describing the file itself — must
        // still lead the file, not have been swallowed into `files`'s own
        // comment or lost.
        assert!(
            out.trim_start().starts_with("# Each entry only needs"),
            "the file header must still lead the file:\n{out}"
        );
    }

    /// The same defect, reached from `remove_entry` instead: remove the first
    /// plugin (handing its header to the new first one), then move that new
    /// first entry away. Each remaining plugin's own description must still
    /// sit immediately above its own `[[plugin]]` — not have picked up, or
    /// left behind, a piece of the file header.
    #[test]
    fn removing_the_first_then_moving_the_new_first_keeps_each_comment_with_its_own_plugin() {
        let after_remove = remove_entry(&realistic(), "radio").unwrap();
        let out = move_entry(&after_remove, "cd", 1).unwrap();
        assert_eq!(names_in_order(&out).unwrap(), vec!["musicbrainz", "cd"]);
        assert!(
            out.trim_start().starts_with("# Each entry only needs"),
            "the file header must still lead the file:\n{out}"
        );
        // musicbrainz has no comment of its own in this file: nothing but the
        // header may sit above it.
        let header_end = out.find("means active.").unwrap();
        let musicbrainz_at = out.find("name = \"musicbrainz\"").unwrap();
        let between = out[header_end..musicbrainz_at].matches('#').count();
        assert_eq!(between, 0, "musicbrainz picked up a comment that is not its own:\n{out}");
        // cd's own comment must sit immediately above cd, not stranded above
        // musicbrainz instead.
        let comment_at = out.find("arriving at this source").unwrap();
        let cd_at = out.find("name = \"cd\"").unwrap();
        assert!(comment_at < cd_at && cd_at - comment_at < 120, "cd's comment drifted away from cd:\n{out}");
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

    /// Format only, but the file's own header promises the core preserves the
    /// operator's layout: a table displaced FROM first place by another
    /// table taking its spot must not end up glued directly onto the
    /// previous entry's `exec` line — exactly one blank line must separate
    /// them, pinned by exact text rather than by mere presence or order.
    #[test]
    fn a_displaced_first_plugin_gets_a_blank_line_before_its_own_table() {
        let out = move_entry(&realistic(), "radio", 1).unwrap();
        assert_eq!(names_in_order(&out).unwrap(), vec!["cd", "radio", "musicbrainz"]);
        let cd_exec_end = out.find("ritornello-plugin-cd\"").unwrap() + "ritornello-plugin-cd\"".len();
        let radio_table_at = out.find("[[plugin]]\nname = \"radio\"").unwrap();
        let between = &out[cd_exec_end..radio_table_at];
        assert_eq!(
            between, "\n\n",
            "radio's table must be preceded by exactly one blank line, not glued to cd's exec line above it:\n{out:?}"
        );
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

    /// `split_header` used to search a `\n`-normalised copy of the prefix but
    /// slice the ORIGINAL, un-normalised one: on CRLF text that offset drifts
    /// by one byte per `\r\n` line before the cut, and a multibyte character
    /// positioned so the drift lands inside it panics with "byte index is not
    /// a char boundary" rather than merely misreading the split.
    ///
    /// This exact fixture was chosen by computing the drift by hand: three
    /// plain `\r\n`-terminated header lines, then a fourth ending in `café`
    /// (a 2-byte UTF-8 character) immediately before its own `\r\n`, then the
    /// blank line, then `radio`'s own comment. Fewer or more preceding CRLF
    /// lines shift the drift by one byte and land back on a valid boundary —
    /// this is not "any CRLF plus any accent", it is this specific alignment,
    /// verified against the fixed `split_header` to land mid-character in the
    /// unfixed one.
    ///
    /// Reachable in practice: the real example file is CRLF on this
    /// checkout's disk, `deploy.sh` copies it as-is, and another script in
    /// this repository already strips `\r` from it for exactly this reason.
    #[test]
    fn crlf_with_a_blank_line_in_the_header_and_a_non_ascii_character_does_not_panic() {
        // Built from explicit `\r\n` escapes rather than a physical
        // multi-line literal: this file's own line endings must not decide
        // what bytes this test actually exercises.
        let doc = "# En-tête, ligne un.\r\n\
                   # En-tête, ligne deux.\r\n\
                   # En-tête, ligne trois.\r\n\
                   # En-tête, dernière ligne, se terminant par café\r\n\
                   \r\n\
                   # Commentaire propre à radio.\r\n\
                   [[plugin]]\r\n\
                   name = \"radio\"\r\n\
                   exec = \"/x\"\r\n\
                   \r\n\
                   [[plugin]]\r\n\
                   name = \"cd\"\r\n\
                   exec = \"/y\"\r\n";
        let out = move_entry(doc, "radio", 1).unwrap();
        assert_eq!(names_in_order(&out).unwrap(), vec!["cd", "radio"]);
        assert!(
            out.trim_start().starts_with("# En-tête, ligne un."),
            "the header did not stay at the top:\n{out}"
        );
        assert!(out.contains("café"), "the accented header line did not survive:\n{out}");
        let header_end_at = out.find("café").unwrap();
        let cd_at = out.find("name = \"cd\"").unwrap();
        assert!(header_end_at < cd_at, "the header must stay at the top, not travel with radio:\n{out}");
        let comment_at = out.find("Commentaire propre").unwrap();
        let radio_at = out.find("name = \"radio\"").unwrap();
        assert!(
            comment_at < radio_at && radio_at - comment_at < 80,
            "radio's own comment must travel with it, immediately above it:\n{out}"
        );
        assert!(!out.contains('\r'), "output must be normalised to LF, not CRLF:\n{out:?}");
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

    /// Regression: `trim_trailing_blank_line`, called when a comment-less
    /// table takes the header, collapsed `"P1\n\nP2\n\n"` to `"P1\n\nP2\n"` —
    /// removing ONE trailing blank line, as intended for a single-paragraph
    /// header, but for a multi-paragraph one this leaves the header's own
    /// internal blank line as the only one left. The next `split_header` then
    /// cuts there, reading the header's own second paragraph as the comment
    /// of whoever holds the header next.
    ///
    /// Not reachable on the untouched real file: its own header is a single
    /// paragraph. Reachable the moment a hand-edited file's header has two
    /// paragraphs (a real blank line inside it, unlike the shipped file's `#`
    /// separators — see the module doc) and passes through a comment-less
    /// plugin at least once. Here: `radio` (no comment of its own) holds the
    /// two-paragraph header; moving it away hands the header to `cd`
    /// (comment-less too); moving `z` (also comment-less) up in turn takes it
    /// from `cd` — the second hand-off is where the buggy
    /// `trim_trailing_blank_line` call fires.
    #[test]
    fn a_multi_paragraph_file_header_survives_two_comment_less_hand_offs() {
        // radio's OWN comment ("radio's own comment.") is what lets
        // `split_header` correctly recover paragraphs one AND two together
        // as the file's header on the very first split: without a real own
        // comment for radio to peel off, the last blank line would instead
        // be read as separating radio's own comment from the header — the
        // ambiguity `a_first_plugins_own_multi_paragraph_comment_is_partly_read_as_header`
        // documents — and this test would be pinning that, not the
        // regression.
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

[[plugin]]
name = \"z\"
exec = \"/w\"
";
        // radio moves away: cd (comment-less) inherits the two-paragraph
        // header, first hand-off. radio's own comment correctly travels
        // with radio, not with the header.
        let after_radio = move_entry(doc, "radio", 1).unwrap();
        assert_eq!(names_in_order(&after_radio).unwrap(), vec!["cd", "radio", "z"]);
        assert!(after_radio.trim_start().starts_with("# File header, paragraph one."), "{after_radio}");
        assert!(after_radio.contains("File header, paragraph two."), "{after_radio}");
        assert!(after_radio.contains("radio's own comment"), "{after_radio}");

        // z (also comment-less) displaces cd, which currently holds nothing
        // but the two-paragraph header — second hand-off, where the
        // regression fires: the header must arrive at z with BOTH
        // paragraphs, not just the first one.
        let after_z = move_entry(&after_radio, "z", -2).unwrap();
        assert_eq!(names_in_order(&after_z).unwrap(), vec!["z", "cd", "radio"]);
        assert!(
            after_z.trim_start().starts_with("# File header, paragraph one."),
            "the header's first paragraph did not reach z:\n{after_z}"
        );
        assert!(
            after_z.contains("File header, paragraph two."),
            "the header's second paragraph was lost on the second comment-less hand-off — the trim_trailing_blank_line regression:\n{after_z}"
        );
        let para2_at = after_z.find("File header, paragraph two.").unwrap();
        let z_at = after_z.find("name = \"z\"").unwrap();
        assert!(para2_at < z_at, "paragraph two must stay at the top with z, not travel elsewhere:\n{after_z}");
        // cd, holding nothing but the header at the point z displaced it,
        // must end up with nothing of its own — not paragraph two
        // masquerading as "cd's own comment".
        let cd_at = after_z.find("name = \"cd\"").unwrap();
        let between = &after_z[z_at..cd_at];
        assert!(
            !between.contains("File header"),
            "the header must not be split between z and cd:\n{after_z}"
        );
    }

    /// Reachable via a hand-written fragment (archive fragments never carry
    /// a comment of their own — `scripts/package-release.sh`'s `awk` resets
    /// its block on every `[[plugin]]` line — so this path is only ever
    /// exercised by a hand-edited install): `glue_header("", comment)` used
    /// to yield the comment bare, indistinguishable from a real file header.
    /// The next `split_header` read the whole thing as a header, so once a
    /// second, comment-less plugin displaced this one, the first plugin's
    /// own description stayed behind at the top of the file, permanently
    /// describing a plugin it has nothing to do with.
    #[test]
    fn an_own_comment_with_no_parked_header_stays_with_its_plugin_once_displaced() {
        // A genuinely empty document: `newcomer` becomes the first (and
        // only) entry directly through `append_block`'s no-plugin-table
        // branch — the exact write this fix targets — rather than through
        // an ordinary move onto an already-existing first entry.
        let fragment = "# newcomer's own comment.\n[[plugin]]\nname = \"newcomer\"\nexec = \"/b\"\n";
        let with_newcomer = append_block("", fragment, "newcomer").unwrap();
        assert_eq!(names_in_order(&with_newcomer).unwrap(), vec!["newcomer"]);

        // A second, comment-less plugin — the shape an archive fragment
        // actually produces — is installed, then moves up to displace
        // newcomer directly.
        let with_third =
            append_block(&with_newcomer, "[[plugin]]\nname = \"third\"\nexec = \"/c\"\n", "third").unwrap();
        assert_eq!(names_in_order(&with_third).unwrap(), vec!["newcomer", "third"]);
        let up = move_entry(&with_third, "third", -1).unwrap();
        assert_eq!(names_in_order(&up).unwrap(), vec!["third", "newcomer"]);

        // The property: newcomer's own comment must still be immediately
        // above newcomer, not stranded at the top of the file above third.
        assert!(
            !up.trim_start().starts_with("# newcomer's own comment."),
            "newcomer's comment must not have become a permanent file header:\n{up}"
        );
        let comment_at = up.find("newcomer's own comment").unwrap();
        let newcomer_at = up.find("name = \"newcomer\"").unwrap();
        assert!(
            comment_at < newcomer_at && newcomer_at - comment_at < 60,
            "newcomer's own comment must stay with newcomer:\n{up}"
        );
    }

    /// What `split_header`'s "cut at the last blank line" rule cannot do,
    /// and is not asked to: tell a genuine header boundary apart from a
    /// paragraph break INSIDE a single plugin's own comment, once that
    /// plugin is first. This is inherent to the encoding — a plugin's own
    /// comment and the file's header occupy the same textual space once a
    /// plugin is first, and the cut always favours "more of this is header"
    /// — not a bug in this module, and not something worth fixing by
    /// normalising an operator's blank line into a `#` line, which would be
    /// a bigger liberty than the defect deserves. The shipped file avoids it
    /// by separating every plugin's own multi-paragraph comment with `#`
    /// lines rather than blank ones (see `musicbrainz`'s and `files`'
    /// comments in `deploy/plugins.example.toml`), which is why nothing is
    /// affected today. This test pins TODAY's behaviour precisely so a
    /// future change to `split_header` or `glue_header` does not alter it
    /// silently.
    #[test]
    fn a_first_plugins_own_multi_paragraph_comment_is_partly_read_as_header() {
        let doc = "\
# File header, one paragraph, no blank line of its own.
[[plugin]]
name = \"solo\"
exec = \"/a\"
";
        let fragment = "\
# Paragraph one of newcomer's own comment.

# Paragraph two of newcomer's own comment.
[[plugin]]
name = \"newcomer\"
exec = \"/b\"
";
        let with_newcomer = append_block(doc, fragment, "newcomer").unwrap();
        let with_third =
            append_block(&with_newcomer, "[[plugin]]\nname = \"third\"\nexec = \"/c\"\n", "third").unwrap();
        assert_eq!(names_in_order(&with_third).unwrap(), vec!["solo", "newcomer", "third"]);

        // newcomer moves up, becoming first: its own two-paragraph comment
        // now occupies the same textual space as the file's header.
        let up = move_entry(&with_third, "newcomer", -1).unwrap();
        assert_eq!(names_in_order(&up).unwrap(), vec!["newcomer", "solo", "third"]);

        // third (comment-less) displaces newcomer. TODAY's behaviour: only
        // the LAST paragraph of newcomer's own comment is recognised as
        // "its own" and travels with it — the first paragraph is read as
        // part of the file header and stays behind with third.
        let displaced = move_entry(&up, "third", -2).unwrap();
        assert_eq!(names_in_order(&displaced).unwrap(), vec!["third", "newcomer", "solo"]);

        let para1_at = displaced.find("Paragraph one").unwrap();
        let para2_at = displaced.find("Paragraph two").unwrap();
        let third_at = displaced.find("name = \"third\"").unwrap();
        let newcomer_at = displaced.find("name = \"newcomer\"").unwrap();
        assert!(
            para1_at < third_at,
            "today's behaviour: paragraph one is read as file header and stays at the top, above third:\n{displaced}"
        );
        assert!(
            third_at < para2_at && para2_at < newcomer_at,
            "today's behaviour: only paragraph two is recognised as newcomer's own and travels with it:\n{displaced}"
        );
    }

    /// `remove_entry` must hand only the SPLIT-OFF header to the new first
    /// entry, not the first plugin's WHOLE, un-split prefix: the removed
    /// plugin's own comment must not leak into what the next plugin inherits
    /// as "the file header".
    #[test]
    fn removing_a_first_plugin_with_its_own_comment_hands_only_the_header_not_its_comment() {
        let doc = "\
# File header line one.
# File header line two.

# radio's own comment, not the file's.
[[plugin]]
name = \"radio\"
exec = \"/x\"

[[plugin]]
name = \"cd\"
exec = \"/y\"
";
        let out = remove_entry(doc, "radio").unwrap();
        assert_eq!(names_in_order(&out).unwrap(), vec!["cd"]);
        assert!(out.trim_start().starts_with("# File header line one."), "{out}");
        assert!(
            !out.contains("radio's own comment"),
            "radio's own comment must not survive attached to cd as if it were the file header:\n{out}"
        );
    }

    /// `append_block` must clear the document's trailing slot after reading
    /// a parked header back — otherwise the header remains there AND gets
    /// attached to the newly installed plugin, so it appears twice: once
    /// correctly at the top, and once more trailing at the end of the file
    /// (the trailing slot is still rendered after the `[[plugin]]` array).
    #[test]
    fn appending_after_a_parked_header_clears_the_trailing_slot() {
        let doc = "# The parked header.\n[[plugin]]\nname = \"radio\"\nexec = \"/x\"\n";
        let after_remove = remove_entry(doc, "radio").unwrap();
        let restored = append_block(&after_remove, "[[plugin]]\nname = \"mpd\"\nexec = \"/y\"\n", "mpd").unwrap();
        let count = restored.matches("The parked header").count();
        assert_eq!(count, 1, "the header must not be duplicated at the end of the file:\n{restored}");
    }

    /// Appending to a NON-empty file must separate the new block from the
    /// previous one with exactly one blank line — not glue it directly onto
    /// the previous entry's `exec` line.
    #[test]
    fn appending_to_a_non_empty_file_separates_the_new_block_with_a_blank_line() {
        let out = append_block(&realistic(), "[[plugin]]\nname = \"mpd\"\nexec = \"/z\"\n", "mpd").unwrap();
        let musicbrainz_exec_end =
            out.find("ritornello-plugin-musicbrainz\"").unwrap() + "ritornello-plugin-musicbrainz\"".len();
        let mpd_table_at = out.find("[[plugin]]\nname = \"mpd\"").unwrap();
        assert_eq!(
            &out[musicbrainz_exec_end..mpd_table_at],
            "\n\n",
            "the appended block must be separated by exactly one blank line:\n{out:?}"
        );
    }

    /// When a header already ends in a blank line (a two-paragraph header,
    /// say) and gets reattached to a plugin with its own comment,
    /// `glue_header` must not add a SECOND blank line on top of the one the
    /// header already carries.
    #[test]
    fn a_header_already_ending_in_a_blank_line_gets_no_extra_one_when_reattached() {
        let doc = "\
# Header paragraph one.

# Header paragraph two.

# radio's own comment.
[[plugin]]
name = \"radio\"
exec = \"/x\"

# cd's own comment.
[[plugin]]
name = \"cd\"
exec = \"/y\"
";
        let out = move_entry(doc, "radio", 1).unwrap();
        assert_eq!(names_in_order(&out).unwrap(), vec!["cd", "radio"]);
        let para2_end = out.find("Header paragraph two.").unwrap() + "Header paragraph two.".len();
        let cd_comment_at = out.find("# cd's own comment").unwrap();
        assert_eq!(
            &out[para2_end..cd_comment_at],
            "\n\n",
            "exactly one blank line between the header and cd's own comment, not two:\n{out:?}"
        );
    }
}
