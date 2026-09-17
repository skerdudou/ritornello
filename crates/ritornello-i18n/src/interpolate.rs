//! Substituting `{name}` tokens into a resolved catalog string.
//!
//! Every producer of user-facing text does the same two steps: resolve a key
//! (`Chain::get` / `Catalog::get`), then fill its `{name}` tokens from a
//! parameter map. This module is the second step, shared so that every
//! caller — the core's own status text, the admin protocol, the updater, and
//! (mirrored in TypeScript) the browser — gets it from one place instead of
//! reimplementing it.
//!
//! The bug this replaces: folding over the parameter map and calling
//! `str::replace` on the accumulated string once per parameter rescans the
//! **whole** string on every pass, including text a previous pass just
//! inserted. A value that happens to contain another parameter's literal
//! `{token}` — a station a user named `Radio {url}`, raw `systemctl` output,
//! a tokio error message — gets rewritten by a later pass, and which
//! parameter is "later" depends on map iteration order. Over a `HashMap`
//! that order is unspecified and can differ between runs of the same build.
//!
//! [`interpolate`] instead makes one left-to-right pass over the template
//! and never rescans what it has already emitted: a token's value is pushed
//! to the output and the scan continues **past** the closing `}`, so nothing
//! written can be read again by a later lookup. Parameter order therefore
//! cannot change the result.

/// Fills `{name}` tokens in `template` from `params`, in one left-to-right
/// pass.
///
/// Not a template engine: no expressions, no escaping beyond matching a `{`
/// to its next `}`, no nesting. A `{` with no matching `}` (and everything
/// after it) is copied through unchanged. A token with no entry in `params`
/// is left **visible** in the output — `{missing}` stays `{missing}` rather
/// than becoming empty — the same safety net `Chain::get` uses for an
/// unknown key: a visibly wrong text is easier to diagnose than a silently
/// missing one.
///
/// `params` is any collection of `(name, value)` pairs — a slice of tuples
/// at a call site that builds them ad hoc, or a `HashMap`'s `.iter()`
/// adapted to borrowed `&str` pairs.
pub fn interpolate<'a>(template: &str, params: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let params: Vec<(&str, &str)> = params.into_iter().collect();
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        match after_open.find('}') {
            Some(close) => {
                let name = &after_open[..close];
                match params.iter().find(|(n, _)| *n == name) {
                    Some((_, value)) => out.push_str(value),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after_open[close + 1..];
            }
            None => {
                // No matching `}` for this `{`: nothing left to scan for a
                // token, copy the remainder verbatim and stop.
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Names of every `{name}` token in `template`, with the exact same
/// brace-scanning rule [`interpolate`] substitutes with — reused rather
/// than re-implemented as a second, looser scan (a regex, say) that could
/// silently disagree with it on an edge case (an unmatched `{`, a token
/// containing another `{`).
///
/// Built for the generalized key-parity check (task 15): comparing two
/// languages' **key sets** for a module was already an existing test per
/// component, but it stopped at the key — a translation that dropped a
/// `{remaining}` a sibling key kept, or renamed it, shipped green. Comparing
/// the sets this returns for the English value and its translation of the
/// same key closes that gap, key by key.
///
/// Order does not matter to a parity check (it compares two *sets*), hence
/// `BTreeSet` over the token names rather than preserving position — and
/// deduplicated, so a template using `{n}` twice does not count as two
/// parameters to satisfy.
pub fn params_in(template: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after_open = &rest[open + 1..];
        match after_open.find('}') {
            Some(close) => {
                out.insert(after_open[..close].to_string());
                rest = &after_open[close + 1..];
            }
            // No matching `}`: nothing left to scan for a token, exactly
            // like `interpolate`'s own handling of the same shape.
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn substitutes_a_single_token() {
        assert_eq!(interpolate("hello {name}", [("name", "world")]), "hello world");
    }

    #[test]
    fn an_unsupplied_token_stays_visible() {
        // The safety net: a token nobody filled in must not vanish. Matches
        // `Chain::get`'s own contract for an unknown key.
        assert_eq!(interpolate("{n} of {total}", [("n", "3")]), "3 of {total}");
    }

    #[test]
    fn a_template_with_no_tokens_passes_through() {
        assert_eq!(interpolate("no tokens here", [("unused", "x")]), "no tokens here");
    }

    #[test]
    fn an_unmatched_open_brace_is_copied_verbatim() {
        // Not a template engine: no error handling for malformed input,
        // just copy it through rather than panicking or eating text.
        assert_eq!(interpolate("broken {token", [("token", "x")]), "broken {token");
    }

    #[test]
    fn a_value_is_never_rescanned_for_further_tokens() {
        // A value is data, not a second template: if a station is named
        // literally "{url}", that text must reach the output unexamined.
        assert_eq!(interpolate("{name}", [("name", "{url}")]), "{url}");
    }

    #[test]
    fn works_with_a_hashmap_of_owned_strings() {
        // The Rust call sites (`Text::Keyed`) carry `HashMap<String, String>`
        // parameters; this is the shape every one of them actually has to
        // adapt to `interpolate`'s borrowed-pair interface.
        let mut params = HashMap::new();
        params.insert("name".to_string(), "world".to_string());
        let out = interpolate("hi {name}", params.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        assert_eq!(out, "hi world");
    }

    /// The barrier this task exists to build: order-independence proven
    /// without relying on `HashMap` iteration order (which a mutation could
    /// pass or fail depending on the hash seed — not a proof). Both
    /// parameters' values are crafted to collide with the *other*
    /// parameter's own token text, so a chained whole-string `.replace()`
    /// gets a wrong answer **whichever key it visits first**:
    /// - visiting `a` first: `"{a} and {b}"` → `"{b} and {b}"` → (`b` pass
    ///   rewrites both) → `"{a} and {a}"`
    /// - visiting `b` first: `"{a} and {b}"` → `"{a} and {a}"` → (`a` pass
    ///   rewrites both) → `"{b} and {b}"`
    ///
    /// Neither matches the single correct answer, `"{b} and {a}"` — so this
    /// test fails under the chained fold regardless of map order, and passes
    /// only for a resolver that never rescans emitted output.
    ///
    /// **[MUTATION]**: replace this function's body with
    /// `params.into_iter().fold(template.to_string(), |acc, (name, value)| {
    /// acc.replace(&format!("{{{name}}}"), value) })` — this test fails
    /// deterministically (not "sometimes"), because both orders it could
    /// iterate in are covered above.
    #[test]
    fn parameter_order_cannot_change_the_result() {
        let out = interpolate("{a} and {b}", [("a", "{b}"), ("b", "{a}")]);
        assert_eq!(out, "{b} and {a}");
    }

    // --- params_in: the token-set extractor a generalized key-parity check
    // reasons about (task 15) ---

    #[test]
    fn params_in_collects_every_distinct_token() {
        let names: std::collections::BTreeSet<String> =
            ["count", "name"].iter().map(|s| s.to_string()).collect();
        assert_eq!(params_in("{name} has {count} messages"), names);
    }

    #[test]
    fn params_in_of_a_template_with_no_tokens_is_empty() {
        assert!(params_in("no tokens here").is_empty());
    }

    #[test]
    fn params_in_deduplicates_a_token_used_twice() {
        let mut expected = std::collections::BTreeSet::new();
        expected.insert("n".to_string());
        assert_eq!(params_in("{n} of {n}"), expected);
    }

    /// Mirrors `interpolate`'s own handling of an unmatched `{`: nothing left
    /// to scan for a token, so the dangling brace names none.
    #[test]
    fn params_in_ignores_an_unmatched_open_brace() {
        assert!(params_in("broken {token").is_empty());
    }

    /// **[MUTATION]** barrier this extractor exists for, proven directly: a
    /// translation that renamed `{done}` to `{finished}` — same shape, same
    /// English fluency, silently missing the parameter the interpolation
    /// call site actually supplies — must read as two different sets, not
    /// as equal ones. A key-parity check comparing only key sets (the
    /// pre-task-15 shape of every `key_parity_between_the_embedded_en_and_
    /// the_fr_pack` test) cannot see this at all; this is the fact that
    /// check now has to be built on.
    #[test]
    fn params_in_distinguishes_a_renamed_token_from_the_original() {
        assert_ne!(params_in("{done} of {total}"), params_in("{finished} of {total}"));
    }
}
