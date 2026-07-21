//! Voxel pathfinding for mobs, aware of partially-loaded chunks (spec §6,
//! Opus tier). A* over standable cells with Minecraft-style step-up / fall
//! navigation.
//!
//! The defining constraint: the world oracle is **three-state** — `Solid`,
//! `Open`, or `Unknown` (not yet generated/loaded). A cell is only traversable
//! when every block the path depends on there (feet, head, the ground under
//! it, step-up headroom) is *known*. `Unknown` is never treated as walkable or
//! as a wall to squeeze past — it simply blocks, so a mob never paths into or
//! across ungenerated terrain and then falls through it when it streams in.
//! Callers recompute as more chunks load.

use glam::IVec3;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cell {
    /// Occupied by a solid block (can stand on top, can't move through).
    Solid,
    /// Empty/passable and known to be so.
    Open,
    /// Not generated/loaded — treated as impassable, never guessed.
    Unknown,
}

#[derive(Clone, Copy, Debug)]
pub struct NavConfig {
    /// Max blocks a mob will drop in a single step.
    pub max_fall: i32,
    /// A* expansion budget — bounds cost when the goal is unreachable (e.g.
    /// walled off by `Unknown`), returning `None` instead of running forever
    /// over an effectively infinite open world.
    pub max_nodes: usize,
}

impl Default for NavConfig {
    fn default() -> Self {
        Self { max_fall: 3, max_nodes: 4000 }
    }
}

/// A cell `(x,y,z)` is standable if a mob can occupy it: feet and head clear,
/// solid ground directly beneath. All three must be *known* — any `Unknown`
/// fails the `== Open`/`== Solid` checks, so partially-loaded space is
/// automatically non-standable.
fn standable(x: i32, y: i32, z: i32, oracle: &impl Fn(i32, i32, i32) -> Cell) -> bool {
    oracle(x, y, z) == Cell::Open
        && oracle(x, y + 1, z) == Cell::Open
        && oracle(x, y - 1, z) == Cell::Solid
}

/// Walkable neighbours of `p`: for each of the 4 horizontal directions, the
/// highest reachable standable cell — step up one (with headroom), stay level,
/// or fall up to `max_fall`.
fn neighbors(p: IVec3, cfg: &NavConfig, oracle: &impl Fn(i32, i32, i32) -> Cell) -> Vec<IVec3> {
    let mut out = Vec::with_capacity(4);
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let (nx, nz) = (p.x + dx, p.z + dz);
        // Step up: needs clear headroom above the mob's current head.
        if oracle(p.x, p.y + 2, p.z) == Cell::Open && standable(nx, p.y + 1, nz, oracle) {
            out.push(IVec3::new(nx, p.y + 1, nz));
            continue;
        }
        if standable(nx, p.y, nz, oracle) {
            out.push(IVec3::new(nx, p.y, nz));
            continue;
        }
        // Fall: first standable cell scanning down from just below.
        for ny in (p.y - cfg.max_fall..=p.y - 1).rev() {
            if standable(nx, ny, nz, oracle) {
                out.push(IVec3::new(nx, ny, nz));
                break;
            }
        }
    }
    out
}

/// A* from `start` to `goal` over standable cells. Returns the full cell path
/// (inclusive of both ends) or `None` if the goal isn't standable or isn't
/// reachable within `cfg.max_nodes` expansions.
pub fn find_path(
    start: IVec3,
    goal: IVec3,
    oracle: impl Fn(i32, i32, i32) -> Cell,
    cfg: &NavConfig,
) -> Option<Vec<IVec3>> {
    if !standable(goal.x, goal.y, goal.z, &oracle) {
        return None;
    }
    if start == goal {
        return Some(vec![start]);
    }
    let heuristic =
        |p: IVec3| (p.x - goal.x).abs() + (p.y - goal.y).abs() + (p.z - goal.z).abs();

    // Heap entries are (f_score, tie_breaker, [x,y,z]). The unique tie-breaker
    // means the position array is never actually compared; storing it as an
    // array (which is `Ord`) sidesteps needing `Ord` on `IVec3`.
    let mut open: BinaryHeap<Reverse<(i32, u32, [i32; 3])>> = BinaryHeap::new();
    let mut g_score: HashMap<IVec3, i32> = HashMap::new();
    let mut came_from: HashMap<IVec3, IVec3> = HashMap::new();
    let mut counter = 0u32;

    open.push(Reverse((heuristic(start), counter, [start.x, start.y, start.z])));
    g_score.insert(start, 0);
    let mut expanded = 0usize;

    while let Some(Reverse((_, _, cur_arr))) = open.pop() {
        let cur = IVec3::new(cur_arr[0], cur_arr[1], cur_arr[2]);
        if cur == goal {
            let mut path = vec![goal];
            let mut c = goal;
            while let Some(&prev) = came_from.get(&c) {
                path.push(prev);
                c = prev;
                if c == start {
                    break;
                }
            }
            path.reverse();
            return Some(path);
        }
        let cur_g = *g_score.get(&cur).unwrap_or(&i32::MAX);
        expanded += 1;
        if expanded > cfg.max_nodes {
            return None;
        }
        for n in neighbors(cur, cfg, &oracle) {
            let tentative = cur_g + 1;
            if tentative < *g_score.get(&n).unwrap_or(&i32::MAX) {
                g_score.insert(n, tentative);
                came_from.insert(n, cur);
                counter += 1;
                open.push(Reverse((tentative + heuristic(n), counter, [n.x, n.y, n.z])));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_path_over_flat_ground() {
        // Ground at y<=0 solid, open above: standable anywhere at y=1.
        let oracle = |_x, y, _z| if y <= 0 { Cell::Solid } else { Cell::Open };
        let path =
            find_path(IVec3::new(0, 1, 0), IVec3::new(5, 1, 0), oracle, &NavConfig::default())
                .expect("reachable");
        assert_eq!(path.first(), Some(&IVec3::new(0, 1, 0)));
        assert_eq!(path.last(), Some(&IVec3::new(5, 1, 0)));
        // Each step is a unit move (adjacency preserved).
        for w in path.windows(2) {
            let d = (w[1] - w[0]).abs();
            assert_eq!(d.x + d.y + d.z, 1);
        }
    }

    #[test]
    fn climbs_a_staircase() {
        // Ground top follows x (a staircase): solid where y <= x.
        let oracle = |x: i32, y: i32, _z| if y <= x.max(0) { Cell::Solid } else { Cell::Open };
        let path =
            find_path(IVec3::new(0, 1, 0), IVec3::new(3, 4, 0), oracle, &NavConfig::default())
                .expect("staircase reachable");
        assert_eq!(path.last(), Some(&IVec3::new(3, 4, 0)));
        // Never steps up more than one block at a time.
        for w in path.windows(2) {
            assert!((w[1].y - w[0].y).abs() <= 1);
        }
    }

    #[test]
    fn unknown_region_is_impassable() {
        // A full plane of Unknown at x==3 walls off the goal; the mob must not
        // path across ungenerated space.
        let oracle = |x: i32, y: i32, _z| {
            if x == 3 {
                Cell::Unknown
            } else if y <= 0 {
                Cell::Solid
            } else {
                Cell::Open
            }
        };
        let path = find_path(
            IVec3::new(0, 1, 0),
            IVec3::new(5, 1, 0),
            oracle,
            &NavConfig { max_fall: 3, max_nodes: 3000 },
        );
        assert!(path.is_none(), "must not cross an Unknown wall");
    }

    #[test]
    fn respects_node_budget() {
        let oracle = |_x, y, _z| if y <= 0 { Cell::Solid } else { Cell::Open };
        let path = find_path(
            IVec3::new(0, 1, 0),
            IVec3::new(100, 1, 0),
            oracle,
            &NavConfig { max_fall: 3, max_nodes: 10 },
        );
        assert!(path.is_none(), "tiny budget can't reach a far goal");
    }

    #[test]
    fn goal_without_ground_is_unreachable() {
        // Goal floats (no solid beneath) → not standable → None.
        let oracle = |_x, y, _z| if y <= 0 { Cell::Solid } else { Cell::Open };
        let path =
            find_path(IVec3::new(0, 1, 0), IVec3::new(2, 5, 0), oracle, &NavConfig::default());
        assert!(path.is_none());
    }

    #[test]
    fn detours_around_a_solid_pillar() {
        // Solid wall at x==2 for z in {-0,0}, but open at z==1 to route around.
        let oracle = |x: i32, y: i32, z: i32| {
            if y <= 0 {
                Cell::Solid
            } else if x == 2 && z == 0 && y == 1 {
                Cell::Solid // obstacle at head/feet level on the direct line
            } else {
                Cell::Open
            }
        };
        let path =
            find_path(IVec3::new(0, 1, 0), IVec3::new(4, 1, 0), oracle, &NavConfig::default())
                .expect("should route around the pillar");
        // The blocked cell must not appear in the path.
        assert!(!path.contains(&IVec3::new(2, 1, 0)));
    }
}
