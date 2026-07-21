//! Single-player world save/load. One fixed save slot (`world_save.json`
//! next to the running executable's working directory) — the user's ask was
//! "New World" vs. "Join Yours" (continue your saved world), not a
//! multi-slot save browser or real networked multiplayer.
//!
//! Only the diff is persisted: `engine_core::streaming::ChunkManager`
//! already discards unmodified columns on evict and regenerates them
//! deterministically from the seed (see `streaming.rs`'s module doc), so a
//! save is just the seed plus every column that carries a player edit —
//! `ChunkManager::modified_columns`/`drain_evicted_modified` are exactly
//! that list.

use engine_core::chunk::ChunkColumn;
use serde::{Deserialize, Serialize};

const SAVE_PATH: &str = "world_save.json";

#[derive(Serialize, Deserialize)]
pub struct WorldSave {
    pub seed: u64,
    pub creative: bool,
    pub player_pos: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub columns: Vec<((i32, i32), ChunkColumn)>,
}

pub fn save_exists() -> bool {
    std::path::Path::new(SAVE_PATH).exists()
}

/// Best-effort: a failed save (disk full, permissions, ...) shouldn't stop
/// the game from closing, so this logs and swallows the error rather than
/// panicking or propagating.
pub fn save_world(save: &WorldSave) {
    match serde_json::to_string(save) {
        Ok(json) => {
            if let Err(e) = std::fs::write(SAVE_PATH, json) {
                eprintln!("failed to write {SAVE_PATH}: {e:#}");
            }
        }
        Err(e) => eprintln!("failed to serialize world save: {e:#}"),
    }
}

/// `None` on any failure (missing file, corrupt/old-format JSON) — the
/// caller falls back to starting a fresh world rather than crashing on a
/// bad save file.
pub fn load_world() -> Option<WorldSave> {
    let data = std::fs::read_to_string(SAVE_PATH).ok()?;
    serde_json::from_str(&data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::chunk::{BlockId, PalettedSection};

    /// Round-trips through an actual JSON string (not the filesystem — the
    /// save path is a fixed constant, and a unit test writing to the repo's
    /// working directory would be its own footgun). This is the real risk
    /// worth testing: does `ChunkColumn`/`PalettedSection`/`BlockId` survive
    /// serde_json intact, including a non-uniform (bit-packed) section and
    /// the `(i32, i32)` column-key tuples.
    #[test]
    fn world_save_round_trips_through_json() {
        let mut column = ChunkColumn::new();
        let mut section = PalettedSection::filled(BlockId(3));
        section.set(1, 2, 3, BlockId(9)); // forces non-uniform (bit-packed) storage
        column.insert_section(0, section); // section_y 0 == world y 0..15, keeps the .get() math below simple

        let save = WorldSave {
            seed: 0x5EED_1234,
            creative: false,
            player_pos: [1.5, 70.25, -3.0],
            yaw: 0.7,
            pitch: -0.2,
            columns: vec![((4, -7), column)],
        };

        let json = serde_json::to_string(&save).expect("serializes");
        let restored: WorldSave = serde_json::from_str(&json).expect("deserializes");

        assert_eq!(restored.seed, save.seed);
        assert_eq!(restored.creative, save.creative);
        assert_eq!(restored.player_pos, save.player_pos);
        assert_eq!(restored.columns.len(), 1);
        let (key, col) = &restored.columns[0];
        assert_eq!(*key, (4, -7));
        assert_eq!(col.get(1, 2, 3), Some(BlockId(9)));
        assert_eq!(col.get(0, 0, 0), Some(BlockId(3)));
    }
}
