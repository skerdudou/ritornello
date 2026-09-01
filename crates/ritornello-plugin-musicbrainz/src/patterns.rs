//! The store of learned patterns, one per station: which split to verify
//! once, and remember. The ICY format is a property of the **station**, not of
//! the track, so the unit of memorization is the stream URL, probed once then
//! replayed without network.
//!
//! Two enumerations, not one: [`Pattern`] says **what it is** — split on such
//! separator in such order, or do not split — and [`Origin`] says **how we
//! learned it** — standard confirmed, learned deviation, or manual. Merging
//! them would put "do not split" among the origins, and would make a "do not
//! split" set by hand indistinguishable from a learned one. The rule whereby
//! relearning **never** overwrites a manual pattern needs precisely this
//! distinction: without it, the first track after a user's correction would
//! silently undo it.

use crate::icy::Candidate;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// **What**: how to split the string announced by a station — or not split
/// it at all.
///
/// `DoNotSplit` belongs to the *what*, not to the *how*: it is a form of split
/// in its own right (the absence of a split), just like `Split`. Confusing it
/// with an origin would prevent setting "do not split" by hand and telling it
/// apart from a "do not split" that was suffered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    Split {
        separator: String,
        artist_first: bool,
        /// The title is the **middle** field (`Artiste - Titre - Album`), the
        /// rest being ignored.
        ///
        /// `serde(default)`: a state file written before this field still
        /// reads back, and absence means "no", which is the common form.
        ///
        /// **When it is true, `artist_first` has no effect anymore**: the
        /// three-field form is always "artist, then title, then the rest",
        /// and it is the only one `icy::candidates` produces. The reverse
        /// combination is therefore representable without being meaningful —
        /// it is not made impossible by the type, because a third enum
        /// variant would make the page's JSON contract pay for a form it does
        /// not offer.
        ///
        /// This field exists because `icy::candidates` produces a middle
        /// candidate that the pattern had to be able to **replay**. Without
        /// it, that candidate validated then was recorded in a form that glued
        /// the album back onto the title: validation failed on every track,
        /// three failures triggered a reprobe, the same candidate won again —
        /// an endless loop, found by the test that compares `apply` to
        /// `candidates`.
        #[serde(default)]
        title_in_middle: bool,
    },
    DoNotSplit,
}

impl Pattern {
    /// The pattern described by this validated candidate.
    ///
    /// The inverse of [`crate::icy::candidates`]: that one derives the
    /// plausible splits from a string, `from_candidate` remembers which one
    /// validated, to replay it without network next time.
    pub fn from_candidate(c: &Candidate) -> Pattern {
        Pattern::Split {
            separator: c.separator.to_string(),
            artist_first: c.artist_first,
            title_in_middle: c.title_in_middle,
        }
    }
}

/// **How we learned it**: where the pattern retained for a station comes from.
///
/// Never set freely next to an arbitrary pattern: see
/// [`Origin::from_pattern`], which carries the invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The standard separator (`" - "`), artist first: the de facto
    /// convention of broadcast automation systems, confirmed by one request.
    StandardConfirmed,
    /// Everything else a probe has learned: another separator, the reverse
    /// order, or the absence of a split validated by elimination.
    LearnedDeviation,
    /// Set from the admin page. Nothing ever relearns it.
    Manual,
}

impl Origin {
    /// Derives the origin this pattern can carry.
    ///
    /// The store's invariant: `StandardConfirmed` only pairs with the exact
    /// standard. Leaving both fields free would allow a "standard confirmed"
    /// that does not split, or that splits in the reverse order — which
    /// nothing would catch afterwards, since `learn` trusts the origin already
    /// set to know whether it may rewrite.
    pub fn from_pattern(pattern: &Pattern) -> Origin {
        match pattern {
            // `title_in_middle: false` is part of the definition of the
            // standard: `Artiste - Titre - Album` is a deviation, even though
            // its separator and order are those of the standard.
            Pattern::Split { separator, artist_first: true, title_in_middle: false }
                if separator == " - " =>
            {
                Origin::StandardConfirmed
            }
            _ => Origin::LearnedDeviation,
        }
    }
}

/// What the store remembers for a station.
///
/// An entry exists as soon as the station has been probed, even if the result
/// matches the standard: absence would confuse "never probed" and
/// "verified", two states the caller must be able to tell apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub url: String,
    pub pattern: Pattern,
    pub origin: Origin,
    /// ISO-8601 UTC, not a date type: this repository has no date crate, the
    /// value only serves for sorting and display, and producing it from
    /// `SystemTime` avoids a dependency.
    #[serde(default)]
    pub last_used: Option<String>,
    #[serde(default)]
    pub split_titles: u64,
}

/// The store, indexed by stream URL and persisted as JSON.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    stations: Vec<Entry>,
}

impl Store {
    /// Loads the store from disk.
    ///
    /// A missing or unreadable file yields an empty store rather than an
    /// error: a discardable state for a mere cache is relearned, it must not
    /// prevent the plugin from starting.
    pub fn load(path: &Path) -> Store {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// Writes the store to disk, atomically.
    ///
    /// Temporary name specific to this process **and** to this call: a shared
    /// `.tmp` would let two simultaneous writes steal the file from under each
    /// other (`rename` in ENOENT). Same pattern as
    /// `ritornello-plugin-radio/src/state.rs`.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = path.with_extension(format!("json.tmp.{}.{unique}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// A station's entry, if it has already been probed.
    pub fn entry(&self, url: &str) -> Option<&Entry> {
        self.stations.iter().find(|e| e.url == url)
    }

    /// All the entries, for the admin page.
    pub fn entries(&self) -> &[Entry] {
        &self.stations
    }

    /// No station probed.
    ///
    /// `is_empty` and not `vide`: it is this repository's convention for a
    /// predicate (`Known::is_empty`, `Track::is_empty`), and above all "vide"
    /// alone is ambiguous in French — adjective or verb. My brief had written
    /// it that way and the implementer understood the predicate where I meant
    /// the action; both now exist, under two names that can no longer be
    /// confused.
    pub fn is_empty(&self) -> bool {
        self.stations.is_empty()
    }

    /// Forgets **all** the stations: this is the admin page's "clear all".
    ///
    /// A gesture apart from `remove`, and not merely a loop over it: it
    /// answers "I no longer trust what the device has learned", whereas
    /// `remove` answers "reprobe this one". The page presents them distinctly
    /// for that reason, and the caller remains in charge of `save` — as for
    /// the other mutations, so that a disk write does not hide behind a name
    /// that does not mention it.
    pub fn clear_all(&mut self) {
        let count = self.stations.len();
        self.stations.clear();
        tracing::info!("forgot the split patterns of {count} stations");
    }

    /// Sets the pattern learned from a probe.
    ///
    /// If the existing entry is `Manual`, does **nothing**: this is the rule
    /// on which trust in the admin page rests. Without it, the first track
    /// after a user's correction would silently undo it.
    pub fn learn(&mut self, url: &str, pattern: Pattern) {
        if let Some(e) = self.stations.iter_mut().find(|e| e.url == url) {
            if e.origin == Origin::Manual {
                tracing::debug!("manual pattern kept for {url}, learning ignored");
                return;
            }
            e.origin = Origin::from_pattern(&pattern);
            e.pattern = pattern;
            return;
        }
        self.stations.push(Entry {
            url: url.to_string(),
            origin: Origin::from_pattern(&pattern),
            pattern,
            last_used: None,
            split_titles: 0,
        });
    }

    /// Sets a pattern by hand, from the admin page: always `Manual`, even
    /// when the pattern set is the standard.
    pub fn set_manual(&mut self, url: &str, pattern: Pattern) {
        if let Some(e) = self.stations.iter_mut().find(|e| e.url == url) {
            e.pattern = pattern;
            e.origin = Origin::Manual;
            return;
        }
        self.stations.push(Entry {
            url: url.to_string(),
            pattern,
            origin: Origin::Manual,
            last_used: None,
            split_titles: 0,
        });
    }

    /// Counts a successfully split title, and dates the entry.
    pub fn record_success(&mut self, url: &str) {
        let Some(e) = self.stations.iter_mut().find(|e| e.url == url) else {
            tracing::debug!("record_success reported for {url}, with no matching entry");
            return;
        };
        e.split_titles += 1;
        e.last_used = Some(now_iso8601());
    }

    /// Removes a station's entry.
    ///
    /// The recovery gesture for a station classified "do not split": nothing
    /// reprobes it automatically, removal is the remedy.
    pub fn remove(&mut self, url: &str) {
        self.stations.retain(|e| e.url != url);
    }
}

/// Current timestamp, ISO-8601 UTC.
///
/// No date crate in this repository: this value only serves for sorting and
/// display, never for an application-level calendar computation. The days →
/// year/month/day conversion is Howard Hinnant's algorithm (`civil_from_days`),
/// the classic that avoids the dependency.
fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, remainder) = (secs / 86_400, secs % 86_400);
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe as i64 + era * 400 + if month <= 2 { 1 } else { 0 };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        remainder / 3600,
        (remainder % 3600) / 60,
        remainder % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(sep: &str, first: bool) -> Pattern {
        Pattern::Split {
            separator: sep.to_string(),
            artist_first: first,
            title_in_middle: false,
        }
    }

    /// The `Artiste - Titre - Album` form, whose pattern must be told apart
    /// from the standard: see `Origin::from_pattern`.
    fn split_middle(sep: &str) -> Pattern {
        Pattern::Split {
            separator: sep.to_string(),
            artist_first: true,
            title_in_middle: true,
        }
    }

    #[test]
    fn the_origin_derives_from_the_pattern_and_cannot_contradict_it() {
        // The invariant: `StandardConfirmed` only pairs with the standard.
        // Leaving both fields free would allow a "standard confirmed" that
        // does not split, which nothing would catch afterwards.
        assert_eq!(Origin::from_pattern(&split(" - ", true)), Origin::StandardConfirmed);
        assert_eq!(Origin::from_pattern(&split(" - ", false)), Origin::LearnedDeviation);
        assert_eq!(Origin::from_pattern(&split(" / ", true)), Origin::LearnedDeviation);
        assert_eq!(Origin::from_pattern(&Pattern::DoNotSplit), Origin::LearnedDeviation);
        assert_eq!(
            Origin::from_pattern(&split_middle(" - ")),
            Origin::LearnedDeviation,
            "\"Artiste - Titre - Album\" is not the standard, even with its separator and its order"
        );
    }

    #[test]
    fn a_pattern_set_by_hand_is_manual_even_if_it_is_standard() {
        let mut m = Store::default();
        m.set_manual("http://f", split(" - ", true));
        assert_eq!(m.entry("http://f").unwrap().origin, Origin::Manual);
    }

    #[test]
    fn learning_never_erases_a_manual_pattern() {
        // The rule on which trust in the page rests: without it, the first
        // track after a user's correction would silently undo it.
        let mut m = Store::default();
        m.set_manual("http://f", split(" / ", false));
        m.learn("http://f", split(" - ", true));
        let e = m.entry("http://f").unwrap();
        assert_eq!(e.origin, Origin::Manual);
        assert_eq!(e.pattern, split(" / ", false), "the manual pattern must survive");
    }

    #[test]
    fn an_entry_exists_as_soon_as_the_station_is_probed_even_when_conforming() {
        // The storage invariant: "conforming" is an entry, not an absence.
        // Absence would confuse "never probed" and "verified".
        let mut m = Store::default();
        m.learn("http://f", split(" - ", true));
        let e = m.entry("http://f").expect("a conforming station must have its entry");
        assert_eq!(e.origin, Origin::StandardConfirmed);
    }

    #[test]
    fn successes_are_counted_and_date_the_entry() {
        let mut m = Store::default();
        m.learn("http://f", split(" - ", true));
        assert_eq!(m.entry("http://f").unwrap().split_titles, 0);
        m.record_success("http://f");
        m.record_success("http://f");
        assert_eq!(m.entry("http://f").unwrap().split_titles, 2);
        assert!(m.entry("http://f").unwrap().last_used.is_some());
    }

    #[test]
    fn an_unreadable_file_yields_an_empty_store_and_not_an_error() {
        // A discardable state: we relearn. Failing the plugin's startup over
        // a cache file would be disproportionate.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.json");
        std::fs::write(&p, "{ this is not json").unwrap();
        assert!(Store::load(&p).entries().is_empty());
        assert!(Store::load(&dir.path().join("absent.json")).entries().is_empty());
    }

    #[test]
    fn a_round_trip_through_disk_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("state.json");
        let mut m = Store::default();
        m.set_manual("http://a", split(" – ", false));
        m.learn("http://b", Pattern::DoNotSplit);
        m.record_success("http://a");
        m.save(&p).unwrap();

        let reread = Store::load(&p);
        assert_eq!(reread.entry("http://a"), m.entry("http://a"));
        assert_eq!(reread.entry("http://b").unwrap().pattern, Pattern::DoNotSplit);
    }

    #[test]
    fn clear_all_takes_even_the_manual_patterns() {
        // The non-obvious point, and it had to be settled: the protection of
        // a `Manual` pattern targets **automatic relearning**, never an
        // explicit gesture of the user. He clicked "clear all"; silently
        // leaving him his past corrections would be answering beside the
        // point, and he could no longer get rid of them at all.
        let mut m = Store::default();
        m.set_manual("http://a", split(" / ", false));
        m.learn("http://b", split(" - ", true));
        assert!(!m.is_empty());

        m.clear_all();
        assert!(m.is_empty(), "no station left");
        assert!(m.entry("http://a").is_none(), "the manual one goes too");
        assert!(m.entry("http://b").is_none());
    }

    #[test]
    fn removing_an_entry_makes_it_probeable_again() {
        // The recovery gesture for a station classified "do not split":
        // nothing reprobes it automatically, removal is the remedy.
        let mut m = Store::default();
        m.learn("http://f", Pattern::DoNotSplit);
        m.remove("http://f");
        assert!(m.entry("http://f").is_none());
    }
}
