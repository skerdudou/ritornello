//! The armed preset, kept across restarts in `state.json` of the plugin's own
//! data directory (the radio does the same): after a reboot, Play resumes the
//! last preset rather than the first.

use crate::entries::write_atomically;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct State {
    preset: u8,
}

/// The remembered preset; `None` when nothing (valid) was remembered.
pub fn load(data_dir: &Path) -> Option<u8> {
    let text = std::fs::read_to_string(data_dir.join("state.json")).ok()?;
    let state: State = serde_json::from_str(&text).ok()?;
    (1..=99).contains(&state.preset).then_some(state.preset)
}

pub fn save(data_dir: &Path, preset: u8) -> Result<()> {
    write_atomically(&data_dir.join("state.json"), serde_json::to_string(&State { preset })?.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preset_round_trips_and_garbage_is_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), None);
        save(dir.path(), 7).unwrap();
        assert_eq!(load(dir.path()), Some(7));
        std::fs::write(dir.path().join("state.json"), "{\"preset\":0}").unwrap();
        assert_eq!(load(dir.path()), None);
        std::fs::write(dir.path().join("state.json"), "nope").unwrap();
        assert_eq!(load(dir.path()), None);
    }
}
