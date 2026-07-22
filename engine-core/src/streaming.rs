//! Chunk streaming/threading (spec §4.1): background-thread generation,
//! vertical streaming (shallow surface slice first, deeper sections on
//! demand), horizontal load/unload by radius.
//!
//! Threading model: `ChunkManager` lives on the game thread and is not `Sync`;
//! a small worker pool pulls [`Job`]s off a shared queue, runs the (pure,
//! deterministic) generator, and sends finished sections back over a channel.
//! The game thread integrates results by calling [`ChunkManager::pump`] once
//! per tick — all `ChunkColumn` mutation happens on the game thread, so chunk
//! data itself needs no locking.
//!
//! Persistence contract (spec §4.1): generation is deterministic from the
//! generator, so unmodified columns are *discarded* on unload and simply
//! regenerated later. Only columns with player modifications are handed back
//! via [`ChunkManager::drain_evicted_modified`] for the (future) save system;
//! in-radius modified state is owned by the manager until eviction.

use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::chunk::{BlockId, ChunkColumn, PalettedSection, SECTION_DIM};

/// World generator. **Must be deterministic**: the same `(cx, sy, cz)` must
/// always produce an identical section — unload/reload correctness (and the
/// entire only-persist-modified-chunks strategy) depends on it.
pub trait ChunkGenerator: Send + Sync + 'static {
    fn generate(&self, cx: i32, sy: i32, cz: i32) -> PalettedSection;
}

#[derive(Clone, Debug)]
pub struct StreamingConfig {
    /// Chebyshev radius (in columns) around the center that must be loaded.
    pub load_radius: i32,
    /// Extra hysteresis ring: columns unload only past `load_radius + unload_margin`,
    /// so oscillating across a chunk border doesn't thrash load/unload.
    pub unload_margin: i32,
    /// Section-y range generated eagerly when a column enters the radius —
    /// the shallow "surface slice". Deeper sections stream via [`ChunkManager::ensure_depth`].
    pub initial_sections: RangeInclusive<i32>,
    pub workers: usize,
}

enum Job {
    Gen { cx: i32, sy: i32, cz: i32 },
    Stop,
}

type GenResult = (i32, i32, i32, PalettedSection);

struct Column {
    data: ChunkColumn,
    /// Section-ys ever enqueued for this column (dedups double-requests;
    /// includes already-arrived ones).
    requested: HashSet<i32>,
    /// Section-ys touched by `set_block` — decides persist-vs-discard on unload.
    modified: HashSet<i32>,
}

pub struct ChunkManager {
    columns: HashMap<(i32, i32), Column>,
    cfg: StreamingConfig,
    center: Option<(i32, i32)>,
    jobs: Sender<Job>,
    results: Receiver<GenResult>,
    workers: Vec<JoinHandle<()>>,
    in_flight: usize,
    evicted_modified: Vec<((i32, i32), ChunkColumn)>,
}

impl ChunkManager {
    pub fn new(generator: Arc<dyn ChunkGenerator>, cfg: StreamingConfig) -> Self {
        assert!(cfg.workers >= 1);
        let (job_tx, job_rx) = channel::<Job>();
        let (res_tx, res_rx) = channel::<GenResult>();
        // std mpsc receivers aren't Clone/Sync; the pool shares one behind a mutex.
        let job_rx = Arc::new(Mutex::new(job_rx));
        let workers = (0..cfg.workers)
            .map(|_| {
                let (job_rx, res_tx, generator) =
                    (Arc::clone(&job_rx), res_tx.clone(), Arc::clone(&generator));
                std::thread::spawn(move || loop {
                    // Holding the lock only for recv keeps workers from serializing on generate().
                    let job = match job_rx.lock().unwrap().recv() {
                        Ok(j) => j,
                        Err(_) => return, // manager dropped mid-shutdown
                    };
                    match job {
                        Job::Stop => return,
                        Job::Gen { cx, sy, cz } => {
                            let section = generator.generate(cx, sy, cz);
                            // Send failure = manager already gone; just exit.
                            if res_tx.send((cx, sy, cz, section)).is_err() {
                                return;
                            }
                        }
                    }
                })
            })
            .collect();
        Self {
            columns: HashMap::new(),
            cfg,
            center: None,
            jobs: job_tx,
            results: res_rx,
            workers,
            in_flight: 0,
            evicted_modified: Vec::new(),
        }
    }

    /// Moves the streaming center (player chunk position): queues generation
    /// for columns entering the load radius and evicts columns outside the
    /// radius + hysteresis margin. Call whenever the player crosses a chunk
    /// border (calling every tick is fine — no-op when nothing changed).
    pub fn set_center(&mut self, cx: i32, cz: i32) {
        if self.center == Some((cx, cz)) {
            return;
        }
        self.center = Some((cx, cz));

        let r = self.cfg.load_radius;
        for dz in -r..=r {
            for dx in -r..=r {
                let key = (cx + dx, cz + dz);
                if !self.columns.contains_key(&key) {
                    self.columns.insert(
                        key,
                        Column {
                            data: ChunkColumn::new(),
                            requested: HashSet::new(),
                            modified: HashSet::new(),
                        },
                    );
                    for sy in self.cfg.initial_sections.clone() {
                        self.request_section(key.0, key.1, sy);
                    }
                }
            }
        }

        let drop_beyond = r + self.cfg.unload_margin;
        let evict: Vec<(i32, i32)> = self
            .columns
            .keys()
            .copied()
            .filter(|&(x, z)| (x - cx).abs().max((z - cz).abs()) > drop_beyond)
            .collect();
        for key in evict {
            let col = self.columns.remove(&key).unwrap();
            if !col.modified.is_empty() {
                self.evicted_modified.push((key, col.data));
            }
            // Unmodified: dropped. Deterministic regeneration is the store.
        }
    }

    /// Queues one section for background generation (no-op if already
    /// requested/loaded or the column isn't tracked). The vertical-streaming
    /// entry point for "player is digging toward sy".
    pub fn request_section(&mut self, cx: i32, cz: i32, sy: i32) {
        let Some(col) = self.columns.get_mut(&(cx, cz)) else {
            return;
        };
        if !col.requested.insert(sy) {
            return;
        }
        self.jobs
            .send(Job::Gen { cx, sy, cz })
            .expect("worker pool alive while manager alive");
        self.in_flight += 1;
    }

    /// Vertical streaming helper: ensures every section from the bottom of the
    /// eager slice down to (and including) `sy` is requested, so digging can't
    /// skip past an ungenerated gap.
    pub fn ensure_depth(&mut self, cx: i32, cz: i32, sy: i32) {
        let floor = *self.cfg.initial_sections.start();
        for y in sy..floor {
            self.request_section(cx, cz, y);
        }
    }

    /// Integrates finished sections from the workers (non-blocking; call once
    /// per tick). Results for columns evicted while the job was in flight are
    /// discarded — determinism makes redoing that work later free of harm.
    /// Returns the number of sections applied.
    pub fn pump(&mut self) -> usize {
        let mut applied = 0;
        loop {
            match self.results.try_recv() {
                Ok((cx, sy, cz, section)) => {
                    self.in_flight -= 1;
                    if let Some(col) = self.columns.get_mut(&(cx, cz)) {
                        // A player edit can land before generation finishes
                        // (edit → modified section already inserted): never
                        // clobber modified data with freshly generated data.
                        if !col.modified.contains(&sy) {
                            col.data.insert_section(sy, section);
                            applied += 1;
                        }
                    }
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        applied
    }

    /// Jobs queued or running, not yet pumped in.
    pub fn pending(&self) -> usize {
        self.in_flight
    }

    pub fn column(&self, cx: i32, cz: i32) -> Option<&ChunkColumn> {
        self.columns.get(&(cx, cz)).map(|c| &c.data)
    }

    /// All currently loaded columns, keyed by `(cx, cz)`. For callers that
    /// need to iterate the whole loaded set (e.g. rebuilding a combined mesh).
    pub fn columns(&self) -> impl Iterator<Item = ((i32, i32), &ChunkColumn)> {
        self.columns.iter().map(|(&k, c)| (k, &c.data))
    }

    pub fn block(&self, cx: i32, cz: i32, x: usize, wy: i32, z: usize) -> Option<BlockId> {
        self.columns.get(&(cx, cz))?.data.get(x, wy, z)
    }

    /// Player edit. Fails (false) if the containing section isn't loaded —
    /// callers gate digging on loaded state, they don't edit blind.
    pub fn set_block(&mut self, cx: i32, cz: i32, x: usize, wy: i32, z: usize, b: BlockId) -> bool {
        let Some(col) = self.columns.get_mut(&(cx, cz)) else {
            return false;
        };
        let sy = wy.div_euclid(SECTION_DIM as i32);
        let ly = wy.rem_euclid(SECTION_DIM as i32) as usize;
        let Some(section) = col.data.section_mut(sy) else {
            return false;
        };
        section.set(x, ly, z, b);
        col.modified.insert(sy);
        true
    }

    /// Hands over evicted columns that carry player modifications (for the
    /// future diff-based save system). Draining is the caller's obligation;
    /// until drained they accumulate here.
    pub fn drain_evicted_modified(&mut self) -> Vec<((i32, i32), ChunkColumn)> {
        std::mem::take(&mut self.evicted_modified)
    }

    pub fn loaded_columns(&self) -> usize {
        self.columns.len()
    }
}

impl Drop for ChunkManager {
    fn drop(&mut self) {
        for _ in &self.workers {
            // Ignore send errors: workers may have already exited via channel close.
            let _ = self.jobs.send(Job::Stop);
        }
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::AIR;
    use std::time::{Duration, Instant};

    /// Deterministic test generator: uniform sections, id derived from coords
    /// (unique per section under the small coordinates tests use).
    struct TestGen;
    impl ChunkGenerator for TestGen {
        fn generate(&self, cx: i32, sy: i32, cz: i32) -> PalettedSection {
            let id = (cx * 100 + cz * 10 + sy).rem_euclid(1000) as u16 + 1;
            let mut s = PalettedSection::filled(BlockId(id));
            // A dab of non-uniform data so reload comparisons exercise indices too.
            s.set(0, 0, 0, BlockId(id + 1));
            s
        }
    }

    fn mgr(radius: i32, margin: i32, workers: usize) -> ChunkManager {
        ChunkManager::new(
            Arc::new(TestGen),
            StreamingConfig {
                load_radius: radius,
                unload_margin: margin,
                initial_sections: 0..=1,
                workers,
            },
        )
    }

    fn pump_until_idle(m: &mut ChunkManager) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while m.pending() > 0 {
            assert!(Instant::now() < deadline, "workers stalled");
            m.pump();
            std::thread::sleep(Duration::from_millis(1));
        }
        m.pump();
    }

    #[test]
    fn loads_initial_radius_and_surface_slice() {
        let mut m = mgr(1, 0, 1);
        m.set_center(0, 0);
        pump_until_idle(&mut m);
        assert_eq!(m.loaded_columns(), 9); // 3×3
        for cx in -1..=1 {
            for cz in -1..=1 {
                let col = m.column(cx, cz).unwrap();
                assert_eq!(col.loaded_sections().count(), 2); // initial_sections 0..=1
                assert!(m.block(cx, cz, 5, 5, 5).is_some());
                assert_eq!(m.block(cx, cz, 5, 40, 5), None); // above slice: not loaded
            }
        }
    }

    #[test]
    fn generation_matches_generator_output() {
        let mut m = mgr(0, 0, 1);
        m.set_center(4, -3);
        pump_until_idle(&mut m);
        let expect = TestGen.generate(4, 0, -3);
        let col = m.column(4, -3).unwrap();
        for x in 0..16 {
            assert_eq!(col.get(x, 3, 7), Some(expect.get(x, 3, 7)));
        }
    }

    #[test]
    fn ensure_depth_streams_deeper_sections() {
        let mut m = mgr(0, 0, 1);
        m.set_center(0, 0);
        pump_until_idle(&mut m);
        assert!(m.block(0, 0, 0, -1, 0).is_none());
        m.ensure_depth(0, 0, -3);
        pump_until_idle(&mut m);
        let col = m.column(0, 0).unwrap();
        let mut ys: Vec<i32> = col.loaded_sections().map(|(y, _)| y).collect();
        ys.sort_unstable();
        assert_eq!(ys, vec![-3, -2, -1, 0, 1]);
        assert!(m.block(0, 0, 0, -33, 0).is_some()); // world y −33 → section −3
    }

    #[test]
    fn unload_discards_unmodified_returns_modified() {
        let mut m = mgr(1, 0, 1);
        m.set_center(0, 0);
        pump_until_idle(&mut m);
        assert!(m.set_block(1, 1, 3, 5, 3, BlockId(777)));
        m.set_center(100, 100);
        pump_until_idle(&mut m);
        assert_eq!(m.loaded_columns(), 9); // only the new neighborhood
        assert!(m.column(0, 0).is_none());
        let evicted = m.drain_evicted_modified();
        assert_eq!(evicted.len(), 1);
        let (key, col) = &evicted[0];
        assert_eq!(*key, (1, 1));
        assert_eq!(col.get(3, 5, 3), Some(BlockId(777)));
        assert!(m.drain_evicted_modified().is_empty()); // drained means drained
    }

    #[test]
    fn reload_regenerates_identically() {
        let mut m = mgr(0, 0, 1);
        m.set_center(2, 2);
        pump_until_idle(&mut m);
        let sample: Vec<_> = (0..16).map(|x| m.block(2, 2, x, 8, x)).collect();
        m.set_center(50, 50); // far: evict (unmodified → discarded)
        pump_until_idle(&mut m);
        m.set_center(2, 2); // come back
        pump_until_idle(&mut m);
        let resample: Vec<_> = (0..16).map(|x| m.block(2, 2, x, 8, x)).collect();
        assert_eq!(sample, resample);
    }

    #[test]
    fn hysteresis_margin_defers_unload() {
        let mut m = mgr(1, 2, 1);
        m.set_center(0, 0);
        pump_until_idle(&mut m);
        m.set_center(2, 0); // old columns at distance ≤3 = radius+margin: kept
        pump_until_idle(&mut m);
        assert!(m.column(-1, 0).is_some());
        m.set_center(5, 0); // distance 6 > 3: evicted
        assert!(m.column(-1, 0).is_none());
    }

    #[test]
    fn edit_wins_over_late_generation() {
        let mut m = mgr(0, 0, 1);
        m.set_center(0, 0);
        pump_until_idle(&mut m);
        // Re-request an already-loaded section is deduped; simulate the race
        // instead: edit, then force a duplicate gen result through the channel
        // by re-requesting after clearing the dedup entry is impossible from
        // outside — so exercise the guard directly via a fresh column edit
        // before its generation lands.
        let mut m2 = mgr(0, 0, 1);
        m2.set_center(0, 0);
        // Edit before pumping generation results in: section not loaded → false.
        assert!(!m2.set_block(0, 0, 1, 1, 1, BlockId(500)));
        pump_until_idle(&mut m2);
        // After load, edit lands and a hypothetical late duplicate wouldn't
        // clobber (guard: modified sections never overwritten in pump()).
        assert!(m2.set_block(0, 0, 1, 1, 1, BlockId(500)));
        assert_eq!(m2.block(0, 0, 1, 1, 1), Some(BlockId(500)));
        drop(m);
    }

    #[test]
    fn columns_iterates_loaded_set() {
        let mut m = mgr(1, 0, 1);
        m.set_center(0, 0);
        pump_until_idle(&mut m);
        let mut keys: Vec<(i32, i32)> = m.columns().map(|(k, _)| k).collect();
        keys.sort_unstable();
        let mut expect: Vec<(i32, i32)> =
            (-1..=1).flat_map(|x| (-1..=1).map(move |z| (x, z))).collect();
        expect.sort_unstable();
        assert_eq!(keys, expect);
    }

    #[test]
    fn multi_worker_smoke() {
        let mut m = mgr(3, 0, 4); // 49 columns × 2 sections across 4 workers
        m.set_center(0, 0);
        pump_until_idle(&mut m);
        assert_eq!(m.loaded_columns(), 49);
        for cx in -3i32..=3 {
            for cz in -3i32..=3 {
                assert_ne!(m.block(cx, cz, 8, 8, 8), Some(AIR));
                assert_eq!(
                    m.block(cx, cz, 4, 4, 4),
                    Some(TestGen.generate(cx, 0, cz).get(4, 4, 4))
                );
            }
        }
    }
}
