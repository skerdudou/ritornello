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
//!
//! **Fix round 2.** A re-review ported `production_lines`/`skip_gated_item`
//! to Python and attacked the port. Four more gaps, all fixed here:
//! - **N1**: the very shape round 1 fixed for (`#[cfg(test)] mod
//!   placeholder;`) still broke it when written on ONE line rather than
//!   two — nothing in this project's CI runs `rustfmt`, so nothing stops
//!   that shape from appearing. `locate_gated_item` now recognises text
//!   following the attribute on its OWN line as the item's own first line,
//!   for both the scanning (`production_lines`) and the whole-file
//!   exclusion (`test_only_files`) paths.
//! - **N2**: three shapes made `skip_gated_item` skip silently past
//!   production code without a word — a gated struct field or enum variant
//!   (no `;` or `{` of its own before the enclosing item's `}`), a raw
//!   string holding brace characters, and a block comment holding one. The
//!   first is now a loud offender ("cannot delimit …") instead of a silent
//!   miss; the other two are now genuinely delimited, tracked across line
//!   boundaries, including nested block comments and raw strings of any
//!   number of `#`.
//! - **N3**: the limitation this file used to claim ("no gated item has a
//!   multi-line string today") was false — three real test modules
//!   (`musicbrainz.rs`, `radio/directory.rs`, `radiofrance-metas/live.rs`)
//!   hold raw JSON fixtures that used to end the skip early. Fixed by the
//!   same raw-string tracking as N2; see `tests::the_three_known_raw_string_modules_are_now_skipped_whole`.
//! - **N4**: `mod name;` was always resolved beside the declaring file,
//!   which is only where `rustc` itself resolves it for `main.rs`, `lib.rs`,
//!   `mod.rs` or a `src/bin/` root — for any other file `foo.rs`, the rule
//!   is `foo/name.rs` (or `foo/name/mod.rs`). `#[path = "..."]` is not
//!   guessed at all; it is reported as an offender instead.
//!
//! **Fix round 3 (the last on this parser, per the controller).** A second
//! re-review ported the parser to Python again and reports zero offenders on
//! the real fleet — every remaining gap is contrived, none exploited today.
//! Fixed anyway, kept small and local:
//! - **B1**: `skip_gated_item` reported only the LINE an item ended on, so
//!   `production_lines` always resumed scanning at the START of the next
//!   line — production code sharing the item's own end-of-line (after a
//!   same-line `;`/`}`) was silently dropped. It now reports the leftover
//!   text on that same line too, scanned like any other production text.
//! - **B2**: a gated match arm or struct-expression field ending in `,`
//!   (never `;` or `{` of its own) let the skip adopt its next braced
//!   SIBLING's braces as if they were its own, silently swallowing that
//!   sibling whole. A `,` at brace depth 0, outside `()`/`[]`, before the
//!   item's own first `{`/`;`, is now the same kind of undelimited failure
//!   as a negative brace depth — reported, never silently adopted.
//! - **B3 (Minor)**: the char-literal heuristic only knew ONE-character
//!   escapes (`\n`, `\'`, …); a long one (`\x41`, `\u{7b}`) left a
//!   `'`-that-does-not-really-close behind, which could then pair with a
//!   later, unrelated `'` and miscount a real brace in between. Escape
//!   sequences of any length are now measured properly.
//! - **B4 (latent)**: a gated `mod x;` found INSIDE an inline `mod m { .. }`
//!   resolves, in `rustc`, to `m/x.rs` — a directory this guard does not
//!   know the name of without parsing `m`'s own declaration, so it is
//!   reported rather than guessed at (wrongly, beside the whole file) as
//!   round 2 did. A one-line `#[cfg(test)] #[path = ".."] mod m;` used to
//!   be silently ignored (neither resolved nor reported, since the "item"
//!   `locate_gated_item` returned was the literal text `#[path = ".."] mod
//!   m;`, which matches no pattern `declared_test_only_module` knows); a
//!   stacked attribute sharing the `#[cfg(test)]` attribute's own line is
//!   now walked past exactly like one on its own line, so R20's `#[path]`
//!   offender fires either way.

/// What a `#[cfg(test)]` attribute at `lines[attr_line]` gates, once the
/// item it applies to is actually located (Ruling R18, N1): text following
/// the attribute ON ITS OWN LINE is the item's own first line — never
/// assumed to start on the next physical line, which is what let a
/// same-line `#[cfg(test)] mod placeholder;` escape both `production_lines`
/// and `test_only_files` in round 1.
enum GatedItem<'a> {
    /// The item starts at this line, with this text (a suffix of the
    /// physical line when found on the attribute's own line, or the whole
    /// of a later line otherwise).
    Here(usize, &'a str),
    /// A `#[path = "..."]` attribute stood between the `#[cfg(test)]` and
    /// the item (Ruling R20, N4): which file a gated `mod name;` names is
    /// not this guard's to guess. The line named is the `#[path]` line
    /// itself, kept for the offender message.
    UnresolvablePath(usize),
    /// A dangling `#[cfg(test)]` at end of file, gating nothing.
    Dangling,
}

fn locate_gated_item<'a>(lines: &[&'a str], attr_line: usize) -> GatedItem<'a> {
    let trimmed = lines[attr_line].trim_start();
    let mut rest = trimmed["#[cfg(test)]".len()..].trim_start();
    let line = attr_line;
    loop {
        if rest.is_empty() {
            // Nothing more on this physical line: the item is the next line
            // that is neither blank nor itself a further attribute
            // (`#[cfg(test)]` stacked with e.g. `#[allow(dead_code)]`, or
            // with `#[path = "..."]`, before the real item).
            let mut j = line + 1;
            while j < lines.len() {
                let t = lines[j].trim_start();
                if t.is_empty() {
                    j += 1;
                    continue;
                }
                if t.starts_with("#[path") {
                    return GatedItem::UnresolvablePath(j);
                }
                if t.starts_with('#') {
                    j += 1;
                    continue;
                }
                return GatedItem::Here(j, t);
            }
            return GatedItem::Dangling;
        }
        if rest.starts_with("#[path") {
            return GatedItem::UnresolvablePath(line);
        }
        if rest.starts_with('#') {
            // Ruling R24, B4: a stacked attribute SHARING the `#[cfg(test)]`
            // attribute's own line (`#[cfg(test)] #[path = ".."] mod m;`,
            // all one line) used to be swallowed whole as "the item" itself
            // — which matches no pattern `declared_test_only_module` knows,
            // so it was silently ignored rather than resolved OR reported.
            // Walked past here exactly like a stacked attribute on its own
            // line, so the loop above still finds `#[path]` (or the real
            // item) on the other side of it.
            if let Some(end) = rest.find(']') {
                rest = rest[end + 1..].trim_start();
                continue;
            }
            // Malformed (no closing bracket on this line): fall through and
            // let whatever is left stand as the item, rather than looping
            // forever on text this function cannot make sense of.
        }
        return GatedItem::Here(line, rest);
    }
}

/// The lines of `text` that are NOT part of any `#[cfg(test)]`-gated item,
/// each paired with its original 1-based line number — so an offender is
/// reported at its real position in the file, not at its position in this
/// filtered view — plus the 0-based line index of every `#[cfg(test)]`
/// attribute whose item could not be safely delimited at all (Ruling R19,
/// N2): scanning resumes right after the point of failure, which is always
/// safe in the direction that matters — it can only cause MORE of the file
/// to be treated as production (a possible false failure on a fixture
/// string), never less (a real violation silently missed).
///
/// A `#[cfg(test)]` line gates exactly the item that follows it (Ruling
/// R18, N1 — see `locate_gated_item` for where that item is found,
/// including on the attribute's own line):
/// - `mod x;` (any item ending in `;` before its first `{`) is skipped
///   alone.
/// - anything else is skipped through its own matching closing `}`,
///   balanced while ignoring braces inside string literals, char literals
///   (a `'a` lifetime is treated as a bare token, not an unterminated char
///   literal), raw strings of any number of `#` and nested block comments
///   — all tracked across line boundaries — and `//` line comments.
///
/// Everything before, between and after such items is production code and
/// is kept, however many of them a file carries — this is what a single
/// cut point (first OR last occurrence) cannot express.
///
/// A `#[cfg(test)]` line is recognised by its OWN trimmed text: a doc
/// comment merely mentioning the attribute (`/// ... #[cfg(test)] ...`)
/// starts with `//`, not `#[`, and never triggers a skip.
///
/// **What is deliberately NOT tracked across lines**: an ordinary
/// double-quoted string is not specially continued across a backslash
/// line-continuation the way a raw string or a block comment now is — in
/// practice this is harmless, since running off the end of the current
/// line while `"`-tracking merely rolls onto the next line's own
/// characters with the same open quote still remembered, which for a
/// genuine line-continuation closes correctly anyway. No plugin's gated
/// item relies on the difference either way.
///
/// Owned `String`s, not borrowed `&str` slices (Ruling R24, B1): the item's
/// own end-of-line leftover — production code sharing the item's closing
/// `;`/`}` on the same physical line — has to be reconstructed from the
/// character-indexed scan `skip_gated_item` runs, which cannot be sliced
/// back out of the original `&str` without redoing UTF-8 byte-offset
/// arithmetic `skip_gated_item` has already done the character-safe way.
fn production_lines(text: &str) -> (Vec<(usize, String)>, Vec<usize>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut undelimited = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("#[cfg(test)]") {
            let start = match locate_gated_item(&lines, i) {
                GatedItem::Dangling => break, // nothing left to skip or keep.
                GatedItem::Here(line, text) => Some((line, text)),
                // Still just an ordinary item to SKIP here — resolving
                // *which file* a gated `mod name;` names is
                // `test_only_files`' problem, not this one's.
                GatedItem::UnresolvablePath(line) => Some((line, lines[line])),
            };
            let Some((line, item_text)) = start else { continue };
            // Ruling R24, B1: whichever of `Ok`/`Err` this is, the item's
            // own end line may carry production text AFTER the point that
            // ended (or failed to delimit) it — a same-line `#[cfg(test)]
            // mod placeholder; fn main() { .. }`, or a gated fn's closing
            // `}` immediately followed by `const P: &str = "..";` on that
            // same line. That leftover is production and must be scanned,
            // not silently carried along with whatever was just skipped.
            let (end_line, rest) = match skip_gated_item(&lines, line, item_text) {
                Ok((end_line, rest)) => {
                    i = end_line + 1;
                    (end_line, rest)
                }
                Err((bad_line, rest)) => {
                    undelimited.push(i); // reported at the ATTRIBUTE's own line.
                    i = bad_line + 1;
                    (bad_line, rest)
                }
            };
            if !rest.trim().is_empty() {
                out.push((end_line + 1, rest));
            }
        } else {
            out.push((i + 1, lines[i].to_string()));
            i += 1;
        }
    }
    (out, undelimited)
}

/// The scanning state `skip_gated_item` carries ACROSS physical lines — the
/// one thing round 1's per-line reset did not do, which is exactly what let
/// a raw string or a block comment holding a brace character escape to EOF
/// (Ruling R19, N2/N3).
enum Mode {
    Normal,
    InString,
    InRawString { hashes: usize },
    /// Nested, like `rustc` itself: `depth` counts additional `/*` seen
    /// since the outermost one that put us in this mode.
    InBlockComment { depth: u32 },
}

/// The number of consecutive `#` characters in `chars` starting at `from`.
fn count_hashes(chars: &[char], from: usize) -> usize {
    let mut n = 0;
    while chars.get(from + n) == Some(&'#') {
        n += 1;
    }
    n
}

/// `Some(hashes)` if `chars[r_idx]` (a `'r'`) opens a raw string — `r"`,
/// `r#"`, `r##"`, and so on — `None` if it is an ordinary token character
/// (an identifier, e.g. a local named `r`).
fn raw_string_hashes_at(chars: &[char], r_idx: usize) -> Option<usize> {
    let hashes = count_hashes(chars, r_idx + 1);
    (chars.get(r_idx + 1 + hashes) == Some(&'"')).then_some(hashes)
}

/// Whether `chars[quote_idx]` (a `"`) closes a raw string opened with
/// `hashes` many `#`: exactly that many `#` must follow immediately. Fewer
/// (including running off the end of the current line) means the string is
/// not closed yet — safe in the direction that matters, since it only ever
/// extends the string, never ends it early.
fn closes_raw_string(chars: &[char], quote_idx: usize, hashes: usize) -> bool {
    let end = quote_idx + 1 + hashes;
    end <= chars.len() && chars[quote_idx + 1..end].iter().all(|&c| c == '#')
}

/// The number of characters, starting at `idx` (which must be a `\`), that
/// make up ONE escape sequence inside a char or string literal (Ruling
/// R24, B3): `\x41` (4: backslash, `x`, two hex digits), `\u{7b}` (however
/// many it takes to reach its own closing `}`, inclusive), or anything else
/// — `\n`, `\'`, `\\`, `\0`, `\r`, `\t` — a single character after the
/// backslash, hence 2. Round 2's heuristic only ever knew this last, most
/// common shape; a longer escape left its own closing quote MISCOUNTED as
/// still open, which could then pair with a LATER, unrelated `'` and
/// mistake a real brace in between for part of a char literal.
fn escape_len(chars: &[char], idx: usize) -> usize {
    match chars.get(idx + 1) {
        Some('x') => 4,
        Some('u') if chars.get(idx + 2) == Some(&'{') => {
            let mut len = 3; // `\`, `u`, `{`.
            while chars.get(idx + len) != Some(&'}') && idx + len < chars.len() {
                len += 1;
            }
            if chars.get(idx + len) == Some(&'}') {
                len += 1; // include the closing `}`.
            }
            len
        }
        _ => 2,
    }
}

/// The 0-based index of the last line of the item starting at `start_line`,
/// whose first line's relevant TEXT is `first_line_text` (Ruling R18: on a
/// same-line attribute, this is a SUFFIX of `lines[start_line]`, not the
/// whole line), together with whatever of that same line remains AFTER the
/// item's own end (Ruling R24, B1) — `Ok` if it ends in `;` before any `{`
/// (`mod x;`), or on the line carrying its own matching closing `}`; that
/// leftover is production code sharing the item's own end-of-line, and
/// round 2 dropped it silently by always resuming at the START of the
/// NEXT line.
///
/// `Err((line, leftover))` (Ruling R19, N2 — and R24, B2) when the item
/// cannot be safely delimited at all, the leftover meaning the same thing
/// as for `Ok`:
/// - brace depth goes negative before the item's own first `{` or `;` —
///   a gated struct field or enum variant (`probe: u8,`) has no delimiter
///   of its own before the ENCLOSING item's `}`, which this function must
///   never mistake for its own;
/// - a `,` at brace depth 0, outside `()`/`[]`, appears before the item's
///   own first `{` or `;` — the same failure, reached differently: a gated
///   match arm or struct-expression field ALSO ends in `,`, and without
///   this check the scan would otherwise adopt its next braced SIBLING's
///   `{`/`}` as if they were its own, silently swallowing that sibling
///   whole (`Cmd::Probe => probe(),` followed by `Cmd::Save => { .. }` —
///   the `Save` arm, path literals included, vanishing with no offender at
///   all). A depth-0 comma inside a generic parameter list (`fn f<A, B>()`)
///   is not exempted here (only `()`/`[]` are) — an accepted, and safe,
///   false "cannot delimit" in that shape, per Ruling R24's own text;
/// - the scan reaches end of file with a string, a raw string, a block
///   comment or a brace still open — genuinely unclosed, or (in practice)
///   a shape this function's own tracking does not cover.
///
/// All of the above used to be silently swallowed — the caller now knows to
/// report an offender instead of trusting a wrong or absent answer.
fn skip_gated_item(
    lines: &[&str],
    start_line: usize,
    first_line_text: &str,
) -> Result<(usize, String), (usize, String)> {
    let mut depth: i32 = 0;
    let mut paren_depth: usize = 0;
    let mut bracket_depth: usize = 0;
    let mut found_brace = false;
    let mut mode = Mode::Normal;
    let mut line_no = start_line;
    let mut chars: Vec<char> = first_line_text.chars().collect();
    let mut k = 0usize;
    let mut next_line = start_line + 1;
    loop {
        if k >= chars.len() {
            if next_line >= lines.len() {
                return Err((line_no, String::new())); // EOF: whatever state we were in never closed.
            }
            line_no = next_line;
            chars = lines[next_line].chars().collect();
            k = 0;
            next_line += 1;
            continue;
        }
        let c = chars[k];
        match &mut mode {
            Mode::InString => {
                if c == '\\' {
                    k += 2;
                } else if c == '"' {
                    mode = Mode::Normal;
                    k += 1;
                } else {
                    k += 1;
                }
            }
            Mode::InRawString { hashes } => {
                let h = *hashes;
                if c == '"' && closes_raw_string(&chars, k, h) {
                    k += 1 + h;
                    mode = Mode::Normal;
                } else {
                    k += 1;
                }
            }
            Mode::InBlockComment { depth: cdepth } => {
                if c == '/' && chars.get(k + 1) == Some(&'*') {
                    *cdepth += 1;
                    k += 2;
                } else if c == '*' && chars.get(k + 1) == Some(&'/') {
                    if *cdepth == 0 {
                        mode = Mode::Normal;
                    } else {
                        *cdepth -= 1;
                    }
                    k += 2;
                } else {
                    k += 1;
                }
            }
            Mode::Normal => {
                if c == '/' && chars.get(k + 1) == Some(&'/') {
                    k = chars.len(); // line comment: rest of line ignored.
                } else if c == '/' && chars.get(k + 1) == Some(&'*') {
                    mode = Mode::InBlockComment { depth: 0 };
                    k += 2;
                } else if c == 'r' {
                    match raw_string_hashes_at(&chars, k) {
                        Some(hashes) => {
                            mode = Mode::InRawString { hashes };
                            k += 2 + hashes; // `r`, the hashes, and the opening `"`.
                        }
                        None => k += 1,
                    }
                } else if c == '"' {
                    mode = Mode::InString;
                    k += 1;
                } else if c == '\'' {
                    // A char literal closes right after its own body — one
                    // ordinary character, or one escape sequence of
                    // whatever length `escape_len` measures; a lifetime or
                    // label (`'a`, `'outer:`) never closes that soon —
                    // treated as a bare token when it does not.
                    let body_len =
                        if chars.get(k + 1) == Some(&'\\') { escape_len(&chars, k + 1) } else { 1 };
                    let closes_at = k + 1 + body_len;
                    if chars.get(closes_at) == Some(&'\'') {
                        k = closes_at + 1;
                    } else {
                        k += 1;
                    }
                } else if c == '(' {
                    paren_depth += 1;
                    k += 1;
                } else if c == ')' {
                    paren_depth = paren_depth.saturating_sub(1);
                    k += 1;
                } else if c == '[' {
                    bracket_depth += 1;
                    k += 1;
                } else if c == ']' {
                    bracket_depth = bracket_depth.saturating_sub(1);
                    k += 1;
                } else if c == '{' {
                    depth += 1;
                    found_brace = true;
                    k += 1;
                } else if c == '}' {
                    depth -= 1;
                    k += 1;
                    if found_brace && depth <= 0 {
                        return Ok((line_no, chars[k..].iter().collect()));
                    }
                    if depth < 0 {
                        // A closing brace we do not own: the item never had
                        // one of its own to begin with (a gated struct
                        // field or enum variant, ending in `,` rather than
                        // `;`) — this `}` belongs to whatever encloses it.
                        return Err((line_no, chars[k..].iter().collect()));
                    }
                } else if c == ';' && !found_brace && depth == 0 {
                    k += 1;
                    return Ok((line_no, chars[k..].iter().collect()));
                } else if c == ',' && !found_brace && depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                    // A gated match arm or struct-expression field, ending
                    // in `,` rather than `;` or a `{..}` of its own — the
                    // same "no delimiter of its own" failure as the
                    // negative-depth case above, reported before the scan
                    // ever reaches (and silently adopts) a braced SIBLING.
                    k += 1;
                    return Err((line_no, chars[k..].iter().collect()));
                } else {
                    k += 1;
                }
            }
        }
    }
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

/// Whether a `mod name;` declared in `file` resolves BESIDE `file` itself
/// (Ruling R20, N4) — true only for the four shapes `rustc` itself resolves
/// that way: `main.rs`, `lib.rs`, `mod.rs`, or any file directly under a
/// `src/bin/` directory (every plugin's own binary entry point, and the
/// `files` plugin's `media-mount.rs` helper). For anything else — a plain
/// module file `foo.rs` — a child module resolves to `foo/name.rs` (or
/// `foo/name/mod.rs`), never beside `foo.rs` itself; getting this wrong
/// would make the guard exclude the WRONG file outright — a production
/// `foo/name.rs` staying invisible to it, rather than a merely absent
/// exclusion.
fn resolves_beside_itself(file: &std::path::Path) -> bool {
    match file.file_name().and_then(|n| n.to_str()) {
        Some("main.rs") | Some("lib.rs") | Some("mod.rs") => return true,
        _ => {}
    }
    file.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) == Some("bin")
}

/// The `{`/`}` nesting depth reached after scanning every line strictly
/// before `upto` — string, char, raw-string and comment aware, the same way
/// `skip_gated_item` is (kept in sync with it by hand; there is no shared
/// helper, since one runs bounded by an item's own end and the other over a
/// file's whole, unbounded prefix). Used only to tell whether a
/// `#[cfg(test)]` attribute at `upto` sits at the file's own top level or
/// inside an inline `mod m { .. }` (Ruling R24, B4): a gated `mod x;` found
/// at depth 0 resolves beside the file (or its stem directory), exactly as
/// `resolves_beside_itself` already decides — but ONE found at depth > 0
/// resolves, in `rustc`, beside `m` ITSELF (`m/x.rs`), a directory this
/// guard does not know the name of without separately parsing `m`'s own
/// declaration. Reported as an offender instead of guessed at wrongly.
fn brace_depth_before(lines: &[&str], upto: usize) -> i32 {
    let mut depth: i32 = 0;
    let mut mode = Mode::Normal;
    for line in &lines[..upto] {
        let chars: Vec<char> = line.chars().collect();
        let mut k = 0usize;
        while k < chars.len() {
            let c = chars[k];
            match &mut mode {
                Mode::InString => {
                    if c == '\\' {
                        k += 2;
                    } else if c == '"' {
                        mode = Mode::Normal;
                        k += 1;
                    } else {
                        k += 1;
                    }
                }
                Mode::InRawString { hashes } => {
                    let h = *hashes;
                    if c == '"' && closes_raw_string(&chars, k, h) {
                        k += 1 + h;
                        mode = Mode::Normal;
                    } else {
                        k += 1;
                    }
                }
                Mode::InBlockComment { depth: cdepth } => {
                    if c == '/' && chars.get(k + 1) == Some(&'*') {
                        *cdepth += 1;
                        k += 2;
                    } else if c == '*' && chars.get(k + 1) == Some(&'/') {
                        if *cdepth == 0 {
                            mode = Mode::Normal;
                        } else {
                            *cdepth -= 1;
                        }
                        k += 2;
                    } else {
                        k += 1;
                    }
                }
                Mode::Normal => {
                    if c == '/' && chars.get(k + 1) == Some(&'/') {
                        k = chars.len();
                    } else if c == '/' && chars.get(k + 1) == Some(&'*') {
                        mode = Mode::InBlockComment { depth: 0 };
                        k += 2;
                    } else if c == 'r' {
                        match raw_string_hashes_at(&chars, k) {
                            Some(hashes) => {
                                mode = Mode::InRawString { hashes };
                                k += 2 + hashes;
                            }
                            None => k += 1,
                        }
                    } else if c == '"' {
                        mode = Mode::InString;
                        k += 1;
                    } else if c == '\'' {
                        let body_len =
                            if chars.get(k + 1) == Some(&'\\') { escape_len(&chars, k + 1) } else { 1 };
                        let closes_at = k + 1 + body_len;
                        if chars.get(closes_at) == Some(&'\'') {
                            k = closes_at + 1;
                        } else {
                            k += 1;
                        }
                    } else if c == '{' {
                        depth += 1;
                        k += 1;
                    } else if c == '}' {
                        depth -= 1;
                        k += 1;
                    } else {
                        k += 1;
                    }
                }
            }
        }
    }
    depth
}

/// Every file, among `files`, that is test-only as a WHOLE: under a `tests/`
/// directory, or named by a `#[cfg(test)] mod <name>;` declaration found
/// anywhere in the crate — resolved beside the declaring file only when
/// `resolves_beside_itself` says so, otherwise into its own `<stem>/`
/// subdirectory, both as `rustc` itself would (Ruling R20, N4). Alongside
/// the exclusion set, every `#[cfg(test)] #[path = "..."] mod name;` found
/// along the way is returned as an offender instead of guessed at (Ruling
/// R20): this guard does not read `#[path]`'s value, so it cannot know
/// which file that declaration really names, and guessing wrong would
/// silently exclude an unrelated production file.
///
/// `production_lines` only ever excludes an item WITHIN the file it is
/// given; a file that is entirely test-only — compiled at all only under
/// `cfg(test)`, e.g. a plugin's `placeholder.rs`, rendered only by its own
/// test module — must never be handed to it, or counted as scanned, in the
/// first place: every line of such a file is fixture-adjacent by
/// construction, whatever it happens to contain.
fn test_only_files(files: &[std::path::PathBuf]) -> (std::collections::HashSet<std::path::PathBuf>, Vec<String>) {
    let mut excluded = std::collections::HashSet::new();
    let mut offenders = Vec::new();
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
            match locate_gated_item(&lines, idx) {
                GatedItem::Dangling => {}
                GatedItem::UnresolvablePath(path_line) => {
                    offenders.push(format!(
                        "cannot resolve #[path] for a gated module at {}:{}",
                        file.display(),
                        path_line + 1
                    ));
                }
                GatedItem::Here(_, item_text) => {
                    let Some(name) = declared_test_only_module(item_text) else { continue };
                    // Ruling R24, B4: a gated `mod name;` found INSIDE an
                    // inline `mod m { .. }` resolves, in rustc, beside `m`
                    // itself — a directory this guard cannot name without
                    // parsing `m`'s own declaration separately. Checked only
                    // once we know this really is a `mod name;` (not a
                    // gated fn/struct, which this depth says nothing useful
                    // about): every plugin's own `#[cfg(test)] mod tests {
                    // .. }` body is full of gated `#[test] fn`s one level
                    // deep, and none of those may turn into a false
                    // offender here.
                    if brace_depth_before(&lines, idx) > 0 {
                        offenders.push(format!(
                            "cannot resolve a gated module declared inside an inline module at {}:{}",
                            file.display(),
                            idx + 1
                        ));
                        continue;
                    }
                    let Some(parent) = file.parent() else { continue };
                    let (flat, nested) = if resolves_beside_itself(file) {
                        (parent.join(format!("{name}.rs")), parent.join(name).join("mod.rs"))
                    } else {
                        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                        let dir = parent.join(stem);
                        (dir.join(format!("{name}.rs")), dir.join(name).join("mod.rs"))
                    };
                    if flat.is_file() {
                        excluded.insert(flat);
                    } else if nested.is_file() {
                        excluded.insert(nested);
                    }
                }
            }
        }
    }
    (excluded, offenders)
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
        let (excluded, path_offenders) = test_only_files(&files);
        offenders.extend(path_offenders);
        for file in &files {
            if excluded.contains(file) {
                continue;
            }
            let text = std::fs::read_to_string(file).unwrap();
            scanned += 1;
            let normalised = file.to_string_lossy().replace('\\', "/");
            let (kept, undelimited) = production_lines(&text);
            for bad_line in undelimited {
                offenders.push(format!(
                    "cannot delimit the #[cfg(test)] item at {}:{}",
                    file.display(),
                    bad_line + 1
                ));
            }
            for (line_no, line) in kept {
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
        "the data-directory guard found a problem (a spelled-out data path, or an item it could \
         not safely delimit):\n{}",
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
    use super::*;

    fn kept_lines(src: &str) -> Vec<String> {
        production_lines(src).0.into_iter().map(|(_, l)| l).collect()
    }

    /// `mod x;` on its own line, gated by an attribute on the line before —
    /// is skipped alone; what comes after stays.
    #[test]
    fn a_two_line_cfg_test_mod_is_skipped_but_what_follows_stays() {
        let src = "fn before() {}\n#[cfg(test)]\nmod placeholder;\nfn after() {}\n";
        assert_eq!(kept_lines(src), vec!["fn before() {}", "fn after() {}"]);
    }

    /// A `#[cfg(test)] fn` is skipped through its own matching closing brace,
    /// not just its own line.
    #[test]
    fn a_cfg_test_fn_is_skipped_through_its_closing_brace() {
        let src = "fn before() {}\n#[cfg(test)]\nfn helper() {\n    let x = 1;\n}\nfn after() {}\n";
        assert_eq!(kept_lines(src), vec!["fn before() {}", "fn after() {}"]);
    }

    /// A `}` inside a string literal must not be mistaken for the item's own
    /// closing brace — this is exactly the "balance while ignoring braces
    /// inside string literals" clause this function's own doc promises.
    #[test]
    fn a_closing_brace_inside_a_string_does_not_end_the_item_early() {
        let src = "fn before() {}\n#[cfg(test)]\nmod tests {\n    fn odd() { let s = \"}\"; }\n}\nfn after() {}\n";
        assert_eq!(kept_lines(src), vec!["fn before() {}", "fn after() {}"]);
    }

    /// A doc comment merely mentioning the attribute is not the attribute:
    /// its trimmed text starts with `//`, never `#[`, so it triggers no skip
    /// and the line after it is production code like any other.
    #[test]
    fn a_doc_comment_mentioning_the_attribute_triggers_no_skip() {
        let src = "/// discussed elsewhere: #[cfg(test)] mod tests\nfn after() {}\n";
        assert_eq!(kept_lines(src), vec!["/// discussed elsewhere: #[cfg(test)] mod tests", "fn after() {}"]);
    }

    /// The line numbers returned are the ORIGINAL ones, not positions in the
    /// filtered output — an offender must be reportable at its real place in
    /// the file.
    #[test]
    fn line_numbers_survive_the_filtering() {
        let src = "fn a() {}\n#[cfg(test)]\nmod placeholder;\nfn b() {}\n";
        let numbers: Vec<usize> = production_lines(src).0.into_iter().map(|(n, _)| n).collect();
        assert_eq!(numbers, vec![1, 4]);
    }

    /// A lifetime (`'a`) inside a gated item's body must not be mistaken for
    /// the start of an unterminated char literal — which would otherwise
    /// consume the rest of the item hunting for a closing quote and miscount
    /// the braces along the way.
    #[test]
    fn a_lifetime_inside_a_gated_item_does_not_confuse_the_brace_count() {
        let src = "fn before() {}\n#[cfg(test)]\nfn helper<'a>(x: &'a str) -> &'a str {\n    x\n}\nfn after() {}\n";
        assert_eq!(kept_lines(src), vec!["fn before() {}", "fn after() {}"]);
    }

    // --- N1: the attribute and the item on the SAME physical line ---------

    /// The exact shape that escaped round 1: `#[cfg(test)] mod x;`, both on
    /// one line, followed by real production code that must not be lost.
    #[test]
    fn a_one_line_attribute_and_mod_declaration_keeps_the_following_production_fn() {
        let src = "fn before() {}\n#[cfg(test)] mod placeholder;\nfn after() {}\n";
        assert_eq!(kept_lines(src), vec!["fn before() {}", "fn after() {}"]);
    }

    /// The same shape, with a gated `fn` instead of a `mod`.
    #[test]
    fn a_one_line_attribute_and_fn_keeps_the_following_production_fn() {
        let src = "fn before() {}\n#[cfg(test)] fn t() {}\nfn after() {}\n";
        assert_eq!(kept_lines(src), vec!["fn before() {}", "fn after() {}"]);
    }

    /// `test_only_files` must resolve the module the ONE-LINE attribute
    /// actually gates, not the next line down — which is exactly the
    /// regression the finding describes: radio's own shape is
    /// `#[cfg(test)] mod placeholder;` immediately followed by an ordinary,
    /// ungated `mod state;` — round 1's `test_only_files` always looked at
    /// the line AFTER the attribute, so it would have grabbed `state` (and
    /// excluded `state.rs`, a real production file, entirely) instead of
    /// `placeholder`.
    #[test]
    fn a_one_line_attribute_mod_declaration_excludes_only_the_gated_module() {
        let dir = tempfile::tempdir().unwrap();
        let main_rs = dir.path().join("main.rs");
        std::fs::write(&main_rs, "#[cfg(test)] mod placeholder;\nmod state;\nfn main() {}\n").unwrap();
        std::fs::write(dir.path().join("placeholder.rs"), "pub fn render() {}\n").unwrap();
        std::fs::write(dir.path().join("state.rs"), "pub fn load() {}\n").unwrap();
        let files = vec![main_rs.clone(), dir.path().join("placeholder.rs"), dir.path().join("state.rs")];
        let (excluded, offenders) = test_only_files(&files);
        assert!(offenders.is_empty(), "{offenders:?}");
        assert!(excluded.contains(&dir.path().join("placeholder.rs")));
        assert!(
            !excluded.contains(&dir.path().join("state.rs")),
            "state.rs is a real production file and must not be excluded"
        );
    }

    // --- N2: shapes that must fail loud, or now be genuinely delimited -----

    /// A gated struct field: there is no `;` or `{` of the FIELD's own
    /// before the struct's own closing `}` — a silent skip would run past
    /// it and swallow whatever comes after. Must be reported, and the
    /// production code after it must still be kept (not swallowed twice
    /// over).
    #[test]
    fn a_gated_struct_field_is_reported_rather_than_silently_skipped() {
        let src = "struct S {\n#[cfg(test)]\nprobe: u8,\n}\nfn prod() {}\n";
        let (kept, undelimited) = production_lines(src);
        assert_eq!(undelimited, vec![1], "the attribute is on line 2 (index 1)");
        let kept_lines: Vec<&str> = kept.iter().map(|(_, l)| l.as_str()).collect();
        assert!(kept_lines.contains(&"fn prod() {}"), "{kept_lines:?}");
    }

    /// The same shape for an enum variant, which ends in `,` rather than
    /// `;` — proving the failure is about the missing delimiter, not about
    /// struct syntax specifically.
    #[test]
    fn a_gated_enum_variant_is_reported_rather_than_silently_skipped() {
        let src = "enum E {\n#[cfg(test)]\nProbe,\n}\nfn prod() {}\n";
        let (kept, undelimited) = production_lines(src);
        assert!(!undelimited.is_empty());
        let kept_lines: Vec<&str> = kept.iter().map(|(_, l)| l.as_str()).collect();
        assert!(kept_lines.contains(&"fn prod() {}"), "{kept_lines:?}");
    }

    /// A raw string holding brace characters must not confuse the brace
    /// count: this is the exact fixture the finding gives, and it must now
    /// be delimited correctly (no offender), with the production code after
    /// it kept.
    #[test]
    fn a_raw_string_with_brace_content_is_delimited_correctly() {
        let src = "fn before() {}\n#[cfg(test)]\nfn fixture() -> &'static str { r#\"{\"a\":\"{\"}\"# }\nfn prod() {}\n";
        let (kept, undelimited) = production_lines(src);
        assert!(undelimited.is_empty(), "{undelimited:?}");
        let kept_lines: Vec<&str> = kept.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(kept_lines, vec!["fn before() {}", "fn prod() {}"]);
    }

    /// A raw string spanning several physical lines, closed several lines
    /// down — proving the tracking survives a line boundary, not just a
    /// single embedded brace.
    #[test]
    fn a_multi_line_raw_string_is_delimited_correctly() {
        let src = "fn before() {}\n#[cfg(test)]\nfn fixture() -> &'static str {\n    r#\"{\n        \"a\": \"b\"\n    }\"#\n}\nfn prod() {}\n";
        let (kept, undelimited) = production_lines(src);
        assert!(undelimited.is_empty(), "{undelimited:?}");
        let kept_lines: Vec<&str> = kept.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(kept_lines, vec!["fn before() {}", "fn prod() {}"]);
    }

    /// A block comment holding a brace character (the finding's own
    /// fixture): must not confuse the count either, nested or not.
    #[test]
    fn a_block_comment_with_a_brace_is_delimited_correctly() {
        let src = "fn before() {}\n#[cfg(test)]\nmod tests {\n    /* { */\n}\nfn prod() {}\n";
        let (kept, undelimited) = production_lines(src);
        assert!(undelimited.is_empty(), "{undelimited:?}");
        let kept_lines: Vec<&str> = kept.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(kept_lines, vec!["fn before() {}", "fn prod() {}"]);
    }

    /// A NESTED block comment — Rust nests these, unlike C — must close at
    /// the OUTER `*/`, not the inner one.
    #[test]
    fn a_nested_block_comment_closes_at_its_own_outer_delimiter() {
        let src = "fn before() {}\n#[cfg(test)]\nmod tests {\n    /* outer /* inner */ still-comment */\n}\nfn prod() {}\n";
        let (kept, undelimited) = production_lines(src);
        assert!(undelimited.is_empty(), "{undelimited:?}");
        let kept_lines: Vec<&str> = kept.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(kept_lines, vec!["fn before() {}", "fn prod() {}"]);
    }

    /// An item that never closes before EOF (missing its final brace): the
    /// other half of Ruling R19's "fail loud" requirement. Nothing follows
    /// it in this fixture (there is nothing TO keep past a genuine EOF), so
    /// this only asserts the failure is reported, at the attribute's line.
    #[test]
    fn an_item_left_unclosed_to_eof_is_reported_rather_than_silently_dropped() {
        let src = "fn before() {}\n#[cfg(test)]\nfn never_closes() {\n    let x = 1;\n";
        let (kept, undelimited) = production_lines(src);
        assert_eq!(undelimited, vec![1]);
        let kept_lines: Vec<&str> = kept.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(kept_lines, vec!["fn before() {}"]);
    }

    // --- N3: the three real files that used to end their skip early -------

    /// Fix round 1's stated limitation ("no gated item has a multi-line
    /// string today") was false: these three modules hold raw JSON fixtures
    /// that used to cut the skip short, leaking the REST of their own test
    /// functions into "production" (harmless in practice — it only ever
    /// widens what gets scanned — but not what the guard's doc claimed).
    /// Now that raw strings are tracked across lines, each module is
    /// skipped WHOLE: nothing from its `#[cfg(test)]` line to the file's own
    /// last line survives into `production_lines`' kept output.
    #[test]
    fn the_three_known_raw_string_modules_are_now_skipped_whole() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates");
        let cases = [
            ("ritornello-plugin-musicbrainz/src/musicbrainz.rs", 829usize),
            ("ritornello-plugin-radio/src/directory.rs", 444),
            ("ritornello-plugin-radiofrance-metas/src/live.rs", 428),
        ];
        for (rel, attr_line) in cases {
            let path = crates.join(rel);
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {rel}: {e}"));
            let total_lines = text.lines().count();
            let (kept, undelimited) = production_lines(&text);
            assert!(undelimited.is_empty(), "{rel}: {undelimited:?}");
            let leaked: Vec<usize> =
                kept.into_iter().map(|(n, _)| n).filter(|&n| n >= attr_line && n <= total_lines).collect();
            assert!(
                leaked.is_empty(),
                "{rel}: lines {leaked:?} of the #[cfg(test)] module (starting at {attr_line}) leaked \
                 into production — the module was not skipped whole"
            );
        }
    }

    // --- N4: `mod name;` resolved by rustc's own rule, not always beside ---

    /// A NON-root declaring file (`state.rs`, not `main.rs`/`lib.rs`/
    /// `mod.rs`/a `src/bin/` entry) resolves a child module beside ITSELF —
    /// `state/util.rs` — never as `util.rs` next to `state.rs`, which is
    /// where round 1's resolver would have looked (and found nothing, or
    /// worse, a same-named but unrelated production file).
    #[test]
    fn a_gated_module_declared_by_a_non_root_file_resolves_beside_the_declaring_files_own_stem() {
        let dir = tempfile::tempdir().unwrap();
        let state_rs = dir.path().join("state.rs");
        std::fs::write(&state_rs, "#[cfg(test)]\nmod util;\n").unwrap();
        // The WRONG (round 1) location: a production file that must stay
        // visible to the guard, proving the fix does not merely happen to
        // find nothing at the old path.
        std::fs::write(dir.path().join("util.rs"), "pub fn wrong_location() {}\n").unwrap();
        // The RIGHT (rustc) location.
        std::fs::create_dir(dir.path().join("state")).unwrap();
        std::fs::write(dir.path().join("state").join("util.rs"), "pub fn right_location() {}\n").unwrap();
        let files = vec![state_rs, dir.path().join("util.rs"), dir.path().join("state").join("util.rs")];
        let (excluded, offenders) = test_only_files(&files);
        assert!(offenders.is_empty(), "{offenders:?}");
        assert!(excluded.contains(&dir.path().join("state").join("util.rs")));
        assert!(
            !excluded.contains(&dir.path().join("util.rs")),
            "round 1's beside-the-declaring-file rule must not apply to a non-root file"
        );
    }

    /// `#[path = "..."]` is not guessed at: the guard does not read the
    /// attribute's value, so it cannot know which file a `mod name;` next
    /// to it really names — reported as an offender instead.
    #[test]
    fn a_path_attribute_on_a_gated_module_is_reported_rather_than_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let state_rs = dir.path().join("state.rs");
        std::fs::write(
            &state_rs,
            "#[cfg(test)]\n#[path = \"elsewhere.rs\"]\nmod util;\n",
        )
        .unwrap();
        let files = vec![state_rs];
        let (excluded, offenders) = test_only_files(&files);
        assert!(excluded.is_empty(), "nothing should be guessed at: {excluded:?}");
        assert_eq!(offenders.len(), 1, "{offenders:?}");
        assert!(offenders[0].contains("#[path]"), "{offenders:?}");
    }

    // --- B1: production code sharing the gated item's own end-of-line -----

    /// The finding's own first example: a one-line gated `mod`, immediately
    /// followed — on that SAME physical line — by real production code
    /// carrying a path. Round 2 dropped it entirely (kept = `[]`); it must
    /// now be scanned.
    #[test]
    fn production_code_sharing_a_one_line_gated_mods_own_end_of_line_is_kept() {
        let src = "#[cfg(test)] mod placeholder; fn main() { let p = \"/etc/ritornello/x.toml\"; }\n";
        let kept_lines = kept_lines(src);
        assert!(
            kept_lines.iter().any(|l| l.contains("/etc/ritornello/x.toml")),
            "{kept_lines:?}"
        );
    }

    /// The finding's second example: a gated `fn`'s own closing `}`,
    /// immediately followed by more production code on that SAME line.
    #[test]
    fn production_code_sharing_a_gated_fns_own_closing_brace_line_is_kept() {
        let src = "#[cfg(test)]\nfn t() {\n} const P: &str = \"/etc/ritornello/x.toml\";\n";
        let kept_lines = kept_lines(src);
        assert!(
            kept_lines.iter().any(|l| l.contains("/etc/ritornello/x.toml")),
            "{kept_lines:?}"
        );
    }

    // --- B2: a gated match arm or struct-expression field ------------------

    /// The finding's match-arm fixture: a gated arm ending in `,`, with no
    /// braces of its own, followed by a braced sibling arm carrying a path.
    /// Round 2 adopted the sibling's own braces as if they belonged to the
    /// gated arm, silently swallowing it whole, path included. Must now be
    /// an offender, and the sibling arm must still be kept.
    #[test]
    fn a_gated_match_arm_does_not_swallow_its_braced_sibling() {
        let src = "match x {\n#[cfg(test)]\nCmd::Probe => probe(),\nCmd::Save => { std::fs::write(\"/etc/ritornello/x.toml\", b\"\")?; }\n}\n";
        let (kept, undelimited) = production_lines(src);
        assert!(!undelimited.is_empty(), "must be reported, not silently adopted");
        let kept_lines: Vec<&str> = kept.iter().map(|(_, l)| l.as_str()).collect();
        assert!(
            kept_lines.iter().any(|l| l.contains("/etc/ritornello/x.toml")),
            "the Save arm must not vanish with the Probe arm: {kept_lines:?}"
        );
    }

    /// The finding's struct-expression fixture: same mechanism, a gated
    /// field ending in `,` followed by an ordinary field whose VALUE is
    /// itself braced.
    #[test]
    fn a_gated_struct_expression_field_does_not_swallow_its_braced_sibling() {
        let src = "let s = S {\n#[cfg(test)]\nprobe: 0,\npath: { \"/etc/ritornello/x.toml\".into() },\n};\n";
        let (kept, undelimited) = production_lines(src);
        assert!(!undelimited.is_empty(), "must be reported, not silently adopted");
        let kept_lines: Vec<&str> = kept.iter().map(|(_, l)| l.as_str()).collect();
        assert!(
            kept_lines.iter().any(|l| l.contains("/etc/ritornello/x.toml")),
            "the path field must not vanish with the gated probe field: {kept_lines:?}"
        );
    }

    // --- B3: a char literal with a multi-character escape ------------------

    /// `'\x41'` is FOUR characters wide, not one — round 2's heuristic only
    /// ever measured one-character escapes, so it left `'\x41'` looking
    /// unclosed, which then paired with the unrelated `'{'` right after it
    /// and miscounted a real brace. Inside an `impl`, that silently dropped
    /// every method declared after the gated one.
    #[test]
    fn a_char_literal_with_a_long_escape_does_not_confuse_the_brace_count() {
        let src = "impl S {\n#[cfg(test)]\nfn t() { let a = ['\\x41','{']; }\nfn prod() { \"/etc/ritornello/x.toml\" }\n}\n";
        let (kept, undelimited) = production_lines(src);
        assert!(undelimited.is_empty(), "{undelimited:?}");
        let kept_lines: Vec<&str> = kept.iter().map(|(_, l)| l.as_str()).collect();
        assert!(
            kept_lines.iter().any(|l| l.contains("/etc/ritornello/x.toml")),
            "fn prod must survive the gated fn before it: {kept_lines:?}"
        );
    }

    // --- B4: a gated module inside an inline module, and a one-line #[path] -

    /// A `#[cfg(test)] mod fixtures;` declared INSIDE an inline `mod tests {
    /// .. }` resolves, in rustc, to `tests/fixtures.rs` — never beside the
    /// declaring file itself, which is where round 2 would have looked
    /// (finding nothing, or a same-named unrelated file). Reported instead.
    #[test]
    fn a_gated_module_declared_inside_an_inline_module_is_reported_rather_than_resolved_beside_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let main_rs = dir.path().join("main.rs");
        std::fs::write(
            &main_rs,
            "mod tests {\n#[cfg(test)]\nmod fixtures;\n}\n",
        )
        .unwrap();
        // A production file that must stay visible to the guard: proves the
        // fix does not merely happen to find nothing beside `main.rs`.
        std::fs::write(dir.path().join("fixtures.rs"), "pub fn real_fixtures() {}\n").unwrap();
        let files = vec![main_rs, dir.path().join("fixtures.rs")];
        let (excluded, offenders) = test_only_files(&files);
        assert!(
            !excluded.contains(&dir.path().join("fixtures.rs")),
            "a production fixtures.rs beside main.rs must not be excluded for an inline module's own submodule"
        );
        assert_eq!(offenders.len(), 1, "{offenders:?}");
        assert!(offenders[0].contains("inline module"), "{offenders:?}");
    }

    /// A one-line `#[cfg(test)] #[path = ".."] mod m;`: round 2's same-line
    /// handling treated the WHOLE stacked-attribute-plus-item text as "the
    /// item" itself, which matches no pattern `declared_test_only_module`
    /// knows — silently ignored rather than resolved OR reported. The
    /// `#[path]` attribute must now be found and reported, exactly as it
    /// already is when written on its own line.
    #[test]
    fn a_one_line_stacked_path_attribute_is_reported_the_same_as_a_multi_line_one() {
        let dir = tempfile::tempdir().unwrap();
        let state_rs = dir.path().join("state.rs");
        std::fs::write(&state_rs, "#[cfg(test)] #[path = \"elsewhere.rs\"] mod util;\n").unwrap();
        let files = vec![state_rs];
        let (excluded, offenders) = test_only_files(&files);
        assert!(excluded.is_empty(), "nothing should be guessed at: {excluded:?}");
        assert_eq!(offenders.len(), 1, "{offenders:?}");
        assert!(offenders[0].contains("#[path]"), "{offenders:?}");
    }
}
