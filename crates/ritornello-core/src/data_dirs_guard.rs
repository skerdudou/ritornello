//! Our plugins write only into their own data directory.
//!
//! A convention and not a lock (every plugin runs under the same account), so
//! it is held here instead: no plugin crate's non-test source may spell a path
//! under /etc/ritornello or /var/lib/ritornello, except the few that are not
//! data. The pattern is assembled at runtime so this file does not match
//! itself — the same care `langpack::archive`'s own source guard takes.
//!
//! **Fix round 1, finding C1.** The first version of this guard cut each
//! file at its FIRST `#[cfg(test)]` (`text.split("#[cfg(test)]").next()`),
//! on the assumption that a file's test module always comes last. Several
//! plugins carry an early, unrelated one instead — a one-line
//! `#[cfg(test)] mod placeholder;` gating a sibling file used only to render
//! the "no page" fallback — and everything after it, real `main()` code
//! included, was silently treated as a fixture and never scanned. About
//! 4,000 production lines, across nine files in six crates, went unscanned
//! this way. `production_lines` below replaces the cut with a structural
//! one: a `#[cfg(test)]` line gates exactly the item that follows it, never
//! the rest of the file.

/// The lines of `text` that are NOT part of any `#[cfg(test)]`-gated item,
/// each paired with its original 1-based line number — so an offender is
/// reported at its real position in the file, not at its position in this
/// filtered view.
///
/// A `#[cfg(test)]` line gates exactly the item that follows it:
/// - `mod x;` (any item ending in `;` before its first `{`) is skipped
///   alone, one line.
/// - anything else is skipped through its own matching closing `}`,
///   balanced while ignoring braces inside string literals, char literals
///   (a `'a` lifetime is treated as a bare token, not an unterminated char
///   literal — see `skip_gated_item`) and `//` line comments.
///
/// Everything before, between and after such items is production code and
/// is kept, however many of them a file carries — this is what a single
/// cut point (first OR last occurrence) cannot express.
///
/// A `#[cfg(test)]` line is recognised by its OWN trimmed text: a doc
/// comment merely mentioning the attribute (`/// ... #[cfg(test)] ...`)
/// starts with `//`, not `#[`, and never triggers a skip.
///
/// **Stated limitation.** A string or char literal that itself spans more
/// than one physical line (a raw string with an embedded newline, or a
/// backslash line-continuation) is not tracked across the line boundary —
/// a brace inside one could be miscounted. Sufficient here: no plugin's
/// `#[cfg(test)]`-gated item does that today. Widen `skip_gated_item` if
/// one ever does.
fn production_lines(text: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("#[cfg(test)]") {
            // The gated item: the next line that is neither blank nor
            // itself a further attribute (`#[cfg(test)]` stacked with e.g.
            // `#[allow(dead_code)]` before the real item).
            let mut j = i + 1;
            while j < lines.len() {
                let t = lines[j].trim_start();
                if t.is_empty() || t.starts_with('#') {
                    j += 1;
                } else {
                    break;
                }
            }
            if j >= lines.len() {
                break; // a dangling attribute at EOF: nothing left to skip or keep.
            }
            i = skip_gated_item(&lines, j) + 1;
        } else {
            out.push((i + 1, lines[i]));
            i += 1;
        }
    }
    out
}

/// The 0-based index of the last line of the item starting at `lines[start]`:
/// that same line, if it ends in `;` before any `{` (`mod x;`), otherwise the
/// line carrying the matching closing `}`.
fn skip_gated_item(lines: &[&str], start: usize) -> usize {
    let mut depth: i32 = 0;
    let mut found_brace = false;
    for (idx, line) in lines.iter().enumerate().skip(start) {
        let chars: Vec<char> = line.chars().collect();
        let mut k = 0usize;
        while k < chars.len() {
            match chars[k] {
                '/' if chars.get(k + 1) == Some(&'/') => break, // line comment: rest of line ignored
                '"' => {
                    k += 1;
                    while k < chars.len() {
                        if chars[k] == '\\' {
                            k += 2;
                            continue;
                        }
                        if chars[k] == '"' {
                            k += 1;
                            break;
                        }
                        k += 1;
                    }
                }
                '\'' => {
                    // A char literal closes within the next 1-2 characters
                    // (an ordinary character, or a one-character escape); a
                    // lifetime (`'a`) never does — treated as a bare token
                    // when it does not close that soon.
                    let closes_at = if chars.get(k + 1) == Some(&'\\') { k + 3 } else { k + 2 };
                    if chars.get(closes_at) == Some(&'\'') {
                        k = closes_at + 1;
                    } else {
                        k += 1;
                    }
                }
                '{' => {
                    depth += 1;
                    found_brace = true;
                    k += 1;
                }
                '}' => {
                    depth -= 1;
                    k += 1;
                    if found_brace && depth <= 0 {
                        return idx;
                    }
                }
                ';' if !found_brace && depth == 0 => return idx,
                _ => k += 1,
            }
        }
    }
    lines.len().saturating_sub(1)
}

/// The module name a `#[cfg(test)] mod <name>;` declaration on `line` names,
/// or `None` if `line` is not exactly that shape. An inline `mod x { .. }`
/// is a different item — already handled by `skip_gated_item`'s own brace
/// matching — and is not resolved to a sibling file here: only a
/// semicolon-terminated declaration points at another file at all.
fn declared_test_only_module(line: &str) -> Option<&str> {
    let l = line.trim();
    let l = l.strip_prefix("pub(crate)").map(str::trim_start).unwrap_or(l);
    let l = l.strip_prefix("pub(super)").map(str::trim_start).unwrap_or(l);
    let l = l.strip_prefix("pub").map(str::trim_start).unwrap_or(l);
    let rest = l.strip_prefix("mod ")?.strip_suffix(';')?;
    let name = rest.trim();
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(name)
}

/// Every file, among `files`, that is test-only as a WHOLE: under a `tests/`
/// directory, or named by a `#[cfg(test)] mod <name>;` declaration found
/// anywhere in the crate (resolved to `<name>.rs` or `<name>/mod.rs` beside
/// the declaring file — the same two shapes `rustc` itself resolves a
/// module declaration to).
///
/// `production_lines` only ever excludes an item WITHIN the file it is
/// given; a file that is entirely test-only — compiled at all only under
/// `cfg(test)`, e.g. a plugin's `placeholder.rs`, rendered only by its own
/// test module — must never be handed to it, or counted as scanned, in the
/// first place: every line of such a file is fixture-adjacent by
/// construction, whatever it happens to contain.
fn test_only_files(files: &[std::path::PathBuf]) -> std::collections::HashSet<std::path::PathBuf> {
    let mut excluded = std::collections::HashSet::new();
    for file in files {
        if file.components().any(|c| c.as_os_str() == "tests") {
            excluded.insert(file.clone());
        }
    }
    for file in files {
        let Ok(text) = std::fs::read_to_string(file) else { continue };
        let lines: Vec<&str> = text.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            if !line.trim_start().starts_with("#[cfg(test)]") {
                continue;
            }
            let mut j = idx + 1;
            while j < lines.len() {
                let t = lines[j].trim_start();
                if t.is_empty() || t.starts_with('#') {
                    j += 1;
                } else {
                    break;
                }
            }
            let Some(name) = lines.get(j).and_then(|l| declared_test_only_module(l)) else { continue };
            let Some(parent) = file.parent() else { continue };
            let flat = parent.join(format!("{name}.rs"));
            let nested = parent.join(name).join("mod.rs");
            if flat.is_file() {
                excluded.insert(flat);
            } else if nested.is_file() {
                excluded.insert(nested);
            }
        }
    }
    excluded
}

#[test]
fn no_plugin_spells_a_data_path_of_its_own() {
    // No trailing slash: `Path::new("/var/lib/ritornello").join(name)` (no
    // slash in the literal itself) must be caught exactly like a literal
    // that already carries one (fix round 1, M-a).
    let etc = ["/etc/", "ritornello"].concat();
    let var = ["/var/lib/", "ritornello"].concat();
    // (file suffix, substring) pairs, checked together — never a bare
    // substring any file could carry (fix round 1, M-a): a plugin echoing
    // another plugin's allowed path in a string of its own must still be
    // flagged, which a flat, file-independent allow-list cannot express.
    let allowed: [(&str, String); 2] = [
        ("ritornello-plugin-generic-input/src/main.rs", ["/etc/ritornello/", "input-presets"].concat()),
        ("ritornello-plugin-files/src/bin/media-mount.rs", ["/var/lib/ritornello/plugins/", "files"].concat()),
    ];
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut offenders = Vec::new();
    let mut scanned = 0;
    for entry in std::fs::read_dir(crates).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("ritornello-plugin-") || name == "ritornello-plugin-sdk" {
            continue;
        }
        let files = walk(&entry.path().join("src"));
        let excluded = test_only_files(&files);
        for file in &files {
            if excluded.contains(file) {
                continue;
            }
            let text = std::fs::read_to_string(file).unwrap();
            scanned += 1;
            let normalised = file.to_string_lossy().replace('\\', "/");
            for (line_no, line) in production_lines(&text) {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if !(line.contains(&etc) || line.contains(&var)) {
                    continue;
                }
                let excused = allowed
                    .iter()
                    .any(|(suffix, substring)| normalised.ends_with(suffix) && line.contains(substring.as_str()));
                if !excused {
                    offenders.push(format!("{}:{}: {}", file.display(), line_no, line.trim()));
                }
            }
        }
    }
    // 57 files today: 63 under `src/` across the ten plugin crates (the SDK
    // excluded), minus the 6 whole-file-excluded `#[cfg(test)] mod
    // placeholder;` targets (fix round 1, M-b — raised from the first
    // version's `> 10`, which was never tied to a real count; say the new
    // one when you next raise it). A floor against an empty or broken walk,
    // never a business number: raise it only upward.
    assert!(scanned > 50, "scanned only {scanned} files — the walk is wrong");
    assert!(
        offenders.is_empty(),
        "a plugin spells a data path instead of using ritornello_plugin_sdk::data_dir():\n{}",
        offenders.join("\n")
    );
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::production_lines;

    /// `mod x;` — an item ending in `;` before any `{` — is skipped alone,
    /// one line; what comes after stays.
    #[test]
    fn a_one_line_cfg_test_mod_is_skipped_but_what_follows_stays() {
        let src = "fn before() {}\n#[cfg(test)]\nmod placeholder;\nfn after() {}\n";
        let kept: Vec<&str> = production_lines(src).into_iter().map(|(_, l)| l).collect();
        assert_eq!(kept, vec!["fn before() {}", "fn after() {}"]);
    }

    /// A `#[cfg(test)] fn` is skipped through its own matching closing brace,
    /// not just its own line.
    #[test]
    fn a_cfg_test_fn_is_skipped_through_its_closing_brace() {
        let src = "fn before() {}\n#[cfg(test)]\nfn helper() {\n    let x = 1;\n}\nfn after() {}\n";
        let kept: Vec<&str> = production_lines(src).into_iter().map(|(_, l)| l).collect();
        assert_eq!(kept, vec!["fn before() {}", "fn after() {}"]);
    }

    /// A `}` inside a string literal must not be mistaken for the item's own
    /// closing brace — this is exactly the "balance while ignoring braces
    /// inside string literals" clause this function's own doc promises.
    #[test]
    fn a_closing_brace_inside_a_string_does_not_end_the_item_early() {
        let src = "fn before() {}\n#[cfg(test)]\nmod tests {\n    fn odd() { let s = \"}\"; }\n}\nfn after() {}\n";
        let kept: Vec<&str> = production_lines(src).into_iter().map(|(_, l)| l).collect();
        assert_eq!(kept, vec!["fn before() {}", "fn after() {}"]);
    }

    /// A doc comment merely mentioning the attribute is not the attribute:
    /// its trimmed text starts with `//`, never `#[`, so it triggers no skip
    /// and the line after it is production code like any other.
    #[test]
    fn a_doc_comment_mentioning_the_attribute_triggers_no_skip() {
        let src = "/// discussed elsewhere: #[cfg(test)] mod tests\nfn after() {}\n";
        let kept: Vec<&str> = production_lines(src).into_iter().map(|(_, l)| l).collect();
        assert_eq!(kept, vec!["/// discussed elsewhere: #[cfg(test)] mod tests", "fn after() {}"]);
    }

    /// The line numbers returned are the ORIGINAL ones, not positions in the
    /// filtered output — an offender must be reportable at its real place in
    /// the file.
    #[test]
    fn line_numbers_survive_the_filtering() {
        let src = "fn a() {}\n#[cfg(test)]\nmod placeholder;\nfn b() {}\n";
        let numbers: Vec<usize> = production_lines(src).into_iter().map(|(n, _)| n).collect();
        assert_eq!(numbers, vec![1, 4]);
    }

    /// A lifetime (`'a`) inside a gated item's body must not be mistaken for
    /// the start of an unterminated char literal — which would otherwise
    /// consume the rest of the item hunting for a closing quote and miscount
    /// the braces along the way.
    #[test]
    fn a_lifetime_inside_a_gated_item_does_not_confuse_the_brace_count() {
        let src = "fn before() {}\n#[cfg(test)]\nfn helper<'a>(x: &'a str) -> &'a str {\n    x\n}\nfn after() {}\n";
        let kept: Vec<&str> = production_lines(src).into_iter().map(|(_, l)| l).collect();
        assert_eq!(kept, vec!["fn before() {}", "fn after() {}"]);
    }
}
