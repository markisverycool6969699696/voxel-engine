//! Data-driven block/item definitions (spec §4.5): blocks and items are JSON
//! data, not hand-coded per-item types. A handful of generic systems (mining,
//! stacking, placement, crafting — built elsewhere, on top of this module)
//! read shared fields here; special behaviors (fluids, crops, powered
//! circuits, …) are expressed as [`BehaviorTag`]s rather than one Rust type
//! per block. This is what lets the item count scale into the thousands
//! without proportional code growth.

use std::collections::HashMap;

use anyhow::{bail, Result};
use serde::Deserialize;

/// Shared vocabulary of special behaviors a block/item can opt into. A
/// gameplay system (crop growth, fluid flow, circuit propagation, …) queries
/// for blocks carrying its tag rather than matching on identity. Adding a new
/// block that reuses an existing behavior costs a data entry, not new code;
/// adding a genuinely new behavior costs one tag + one system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorTag {
    Fluid,
    Crop,
    Gravity,
    Flammable,
    Interactable,
    /// Participates in a redstone-like signal network (source or conductor;
    /// systems distinguish by other fields, not by splitting this tag).
    Powerable,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BlockDef {
    pub id: u16,
    pub key: String,
    pub hardness: f32,
    pub solid: bool,
    #[serde(default)]
    pub tags: Vec<BehaviorTag>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ItemDef {
    pub id: u16,
    pub key: String,
    pub stack_size: u32,
    #[serde(default)]
    pub tags: Vec<BehaviorTag>,
    /// Block `key` this item places, if any (the placement system's link
    /// between an inventory item and world block — not every item has one).
    #[serde(default)]
    pub places_block: Option<String>,
}

/// Implemented by [`BlockDef`]/[`ItemDef`] so [`Registry`] can index either
/// without duplicating the load/lookup logic per type.
pub trait Definition {
    fn id(&self) -> u16;
    fn key(&self) -> &str;
}

impl Definition for BlockDef {
    fn id(&self) -> u16 {
        self.id
    }
    fn key(&self) -> &str {
        &self.key
    }
}

impl Definition for ItemDef {
    fn id(&self) -> u16 {
        self.id
    }
    fn key(&self) -> &str {
        &self.key
    }
}

/// Loaded definitions of one kind (all blocks, or all items), indexed by both
/// stable numeric id (used in chunk storage / save data) and string key
/// (used by data files and tooling).
#[derive(Debug)]
pub struct Registry<T: Definition> {
    by_key: HashMap<String, usize>,
    by_id: HashMap<u16, usize>,
    entries: Vec<T>,
}

impl<T: Definition + HasTags + for<'de> Deserialize<'de>> Registry<T> {
    /// Parses a JSON array of definitions. Rejects duplicate `id`s or `key`s
    /// outright — a collision here is a data bug that must be fixed at the
    /// source, not silently resolved by last-write-wins.
    pub fn load_from_str(json: &str) -> Result<Self> {
        let entries: Vec<T> = serde_json::from_str(json)?;
        let mut by_key = HashMap::with_capacity(entries.len());
        let mut by_id = HashMap::with_capacity(entries.len());
        for (idx, def) in entries.iter().enumerate() {
            if by_key.insert(def.key().to_owned(), idx).is_some() {
                bail!("duplicate definition key {:?}", def.key());
            }
            if by_id.insert(def.id(), idx).is_some() {
                bail!("duplicate definition id {} (key {:?})", def.id(), def.key());
            }
        }
        Ok(Self { by_key, by_id, entries })
    }

    pub fn get(&self, key: &str) -> Option<&T> {
        self.by_key.get(key).map(|&i| &self.entries[i])
    }

    pub fn get_by_id(&self, id: u16) -> Option<&T> {
        self.by_id.get(&id).map(|&i| &self.entries[i])
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.entries.iter()
    }

    /// All definitions carrying `tag` — the query generic systems use instead
    /// of a per-block match arm (e.g. the fluid-flow system iterates
    /// `blocks.with_tag(BehaviorTag::Fluid)` rather than naming every liquid).
    pub fn with_tag(&self, tag: BehaviorTag) -> impl Iterator<Item = &T> {
        self.entries.iter().filter(move |d| d.tags_contain(tag))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// Kept separate from `Definition` (tags aren't part of identity) so
// `with_tag` can stay generic without forcing every future `Definition`
// implementor to carry a `tags` field. `pub` (not `pub(crate)`): it appears
// in `Registry`'s public impl bound, so it must be at least as visible.
pub trait HasTags {
    fn tags_contain(&self, tag: BehaviorTag) -> bool;
}

impl HasTags for BlockDef {
    fn tags_contain(&self, tag: BehaviorTag) -> bool {
        self.tags.contains(&tag)
    }
}

impl HasTags for ItemDef {
    fn tags_contain(&self, tag: BehaviorTag) -> bool {
        self.tags.contains(&tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCKS_JSON: &str = r#"[
        { "id": 0, "key": "air", "hardness": 0.0, "solid": false },
        { "id": 1, "key": "stone", "hardness": 1.5, "solid": true },
        { "id": 2, "key": "water", "hardness": 0.0, "solid": false, "tags": ["fluid"] },
        { "id": 3, "key": "wheat_crop", "hardness": 0.0, "solid": false, "tags": ["crop"] }
    ]"#;

    const ITEMS_JSON: &str = r#"[
        { "id": 0, "key": "stone", "stack_size": 64, "places_block": "stone" },
        { "id": 1, "key": "stick", "stack_size": 64 }
    ]"#;

    #[test]
    fn loads_and_looks_up_by_key_and_id() {
        let blocks = Registry::<BlockDef>::load_from_str(BLOCKS_JSON).unwrap();
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks.get("stone").unwrap().hardness, 1.5);
        assert_eq!(blocks.get_by_id(1).unwrap().key, "stone");
        assert!(blocks.get("nonexistent").is_none());
    }

    #[test]
    fn tag_query_finds_only_tagged_entries() {
        let blocks = Registry::<BlockDef>::load_from_str(BLOCKS_JSON).unwrap();
        let fluids: Vec<_> = blocks.with_tag(BehaviorTag::Fluid).map(|b| b.key.as_str()).collect();
        assert_eq!(fluids, vec!["water"]);
        let crops: Vec<_> = blocks.with_tag(BehaviorTag::Crop).map(|b| b.key.as_str()).collect();
        assert_eq!(crops, vec!["wheat_crop"]);
        assert_eq!(blocks.with_tag(BehaviorTag::Powerable).count(), 0);
    }

    #[test]
    fn items_link_to_blocks_by_key() {
        let items = Registry::<ItemDef>::load_from_str(ITEMS_JSON).unwrap();
        assert_eq!(items.get("stone").unwrap().places_block.as_deref(), Some("stone"));
        assert_eq!(items.get("stick").unwrap().places_block, None);
    }

    #[test]
    fn rejects_duplicate_id() {
        let json = r#"[
            { "id": 5, "key": "a", "hardness": 0.0, "solid": false },
            { "id": 5, "key": "b", "hardness": 0.0, "solid": false }
        ]"#;
        let err = Registry::<BlockDef>::load_from_str(json).unwrap_err();
        assert!(err.to_string().contains("duplicate definition id"));
    }

    #[test]
    fn rejects_duplicate_key() {
        let json = r#"[
            { "id": 5, "key": "dup", "hardness": 0.0, "solid": false },
            { "id": 6, "key": "dup", "hardness": 0.0, "solid": false }
        ]"#;
        let err = Registry::<BlockDef>::load_from_str(json).unwrap_err();
        assert!(err.to_string().contains("duplicate definition key"));
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(Registry::<BlockDef>::load_from_str("{ not valid json").is_err());
    }
}
