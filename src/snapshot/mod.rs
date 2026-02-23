//! Core snapshot infrastructure for bevy_ggrs.
//!
//! This module exposes the three fundamental schedules that drive the rollback loop —
//! [`SaveWorld`], [`LoadWorld`], and [`AdvanceWorld`] — together with the snapshot storage
//! types ([`GgrsSnapshots`], [`GgrsComponentSnapshot`]) and the top-level
//! [`SnapshotPlugin`] that wires them all together.
//!
//! Most users interact with this module indirectly through [`RollbackApp`] and
//! [`GgrsPlugin`](`crate::GgrsPlugin`), but the types here are public so that
//! advanced users can build custom snapshot behaviour.

use crate::{DEFAULT_FPS, MaxPredictionWindow};
use bevy::{
    ecs::reflect::AppTypeRegistry, ecs::schedule::ScheduleLabel, platform::collections::HashMap,
    prelude::*,
};
use seahash::SeaHasher;
use std::marker::PhantomData;

mod checksum;
mod childof_snapshot;
mod component_checksum;
mod component_map;
mod component_snapshot;
mod despawn;
mod entity;
mod entity_checksum;
mod resource_checksum;
mod resource_map;
mod resource_snapshot;
mod rollback;
mod rollback_app;
mod rollback_entity;
mod rollback_entity_map;
mod set;
mod strategy;

pub use checksum::*;
pub use childof_snapshot::*;
pub use component_checksum::*;
pub use component_map::*;
pub use component_snapshot::*;
pub use despawn::*;
pub use entity::*;
pub use entity_checksum::*;
pub use resource_checksum::*;
pub use resource_map::*;
pub use resource_snapshot::*;
pub use rollback::*;
pub use rollback_app::*;
pub use rollback_entity::*;
pub use rollback_entity_map::*;
pub use set::*;
pub use strategy::*;

pub mod prelude {
    pub use super::despawn::{RollbackDespawnCommandExtension, RollbackDespawned};
    pub use super::{Checksum, LoadWorldSystems, SaveWorldSystems};
}

/// Label for the schedule which loads and overwrites a snapshot of the world.
#[derive(ScheduleLabel, Debug, Hash, PartialEq, Eq, Clone)]
pub struct LoadWorld;

/// Label for the schedule which saves a snapshot of the current world.
#[derive(ScheduleLabel, Debug, Hash, PartialEq, Eq, Clone)]
pub struct SaveWorld;

/// Label for the schedule which advances the current world to the next frame.
#[derive(ScheduleLabel, Debug, Hash, PartialEq, Eq, Clone)]
pub struct AdvanceWorld;

/// Keeps track of the current frame the rollback simulation is in
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RollbackFrameCount(pub i32);

impl From<RollbackFrameCount> for i32 {
    fn from(value: RollbackFrameCount) -> i32 {
        value.0
    }
}

/// The most recently confirmed frame. Any information for frames stored before this point can be safely discarded.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConfirmedFrameCount(pub i32);

impl From<ConfirmedFrameCount> for i32 {
    fn from(value: ConfirmedFrameCount) -> i32 {
        value.0
    }
}

/// Typical [`Resource`] used to store snapshots for a [`Resource`] `R` as the type `As`.
/// For most types, the default `As = R` will suffice.
pub type GgrsResourceSnapshots<R, As = R> = GgrsSnapshots<R, Option<As>>;

/// Typical [`Resource`] used to store snapshots for a [`Component`] `C` as the type `As`.
/// For most types, the default `As = C` will suffice.
pub type GgrsComponentSnapshots<C, As = C> = GgrsSnapshots<C, GgrsComponentSnapshot<C, As>>;

/// Collection of snapshots for a type `For`, stored as `As`
#[derive(Resource)]
pub struct GgrsSnapshots<For, As = For> {
    snapshots: HashMap<i32, As>,
    /// Maximum number of snapshots to store. `None` means unbounded.
    depth: Option<usize>,
    /// The frame selected by `rollback()`, used by `get()`.
    current_frame: Option<i32>,
    _phantom: PhantomData<For>,
}

impl<For, As> Default for GgrsSnapshots<For, As> {
    fn default() -> Self {
        Self {
            snapshots: HashMap::with_capacity(DEFAULT_FPS),
            depth: Some(DEFAULT_FPS),
            current_frame: None,
            _phantom: default(),
        }
    }
}

impl<For, As> GgrsSnapshots<For, As> {
    /// Updates the maximum number of snapshots to store, pre-allocating capacity.
    pub fn set_depth(&mut self, depth: usize) -> &mut Self {
        self.depth = Some(depth);

        // Greedy allocation to avoid allocating at a more sensitive time.
        if self.snapshots.capacity() < depth {
            let additional = depth - self.snapshots.capacity();
            self.snapshots.reserve(additional);
        }

        self
    }

    /// Removes the snapshot depth limit, allowing unbounded storage.
    pub fn set_unbounded(&mut self) -> &mut Self {
        self.depth = None;
        self
    }

    /// Get the current depth limit of this snapshot storage, or `None` if unbounded.
    pub const fn depth(&self) -> Option<usize> {
        self.depth
    }

    /// Store a snapshot for the provided frame, replacing any existing snapshot at that frame.
    /// If the number of stored snapshots exceeds `depth`, the oldest frame is evicted.
    pub fn push(&mut self, frame: i32, snapshot: As) -> &mut Self {
        self.snapshots.insert(frame, snapshot);

        if let Some(depth) = self.depth {
            while self.snapshots.len() > depth {
                if let Some(&oldest) = self.snapshots.keys().min() {
                    self.snapshots.remove(&oldest);
                } else {
                    break;
                }
            }
        }

        self
    }

    /// Discards snapshots from before `confirmed_frame` as no longer required.
    pub fn confirm(&mut self, confirmed_frame: i32) -> &mut Self {
        self.snapshots.retain(|&frame, _| frame >= confirmed_frame);
        self
    }

    /// Selects a frame to rollback to. Use `get()` to retrieve the snapshot.
    pub fn rollback(&mut self, frame: i32) -> &mut Self {
        assert!(
            self.snapshots.contains_key(&frame),
            "Could not rollback to {frame}: no snapshot at that moment could be found."
        );
        self.current_frame = Some(frame);
        self
    }

    /// Get the snapshot for the frame selected by `rollback()`.
    pub fn get(&self) -> &As {
        let frame = self
            .current_frame
            .expect("No frame selected. Call rollback() before get().");
        self.snapshots
            .get(&frame)
            .expect("Snapshot missing for selected frame")
    }

    /// Get a snapshot for a specific frame, if it exists.
    pub fn peek(&self, frame: i32) -> Option<&As> {
        self.snapshots.get(&frame)
    }

    /// A system which automatically confirms the [`ConfirmedFrameCount`], discarding older snapshots.
    pub fn discard_old_snapshots(
        mut snapshots: ResMut<Self>,
        confirmed_frame: Option<Res<ConfirmedFrameCount>>,
    ) where
        For: Send + Sync + 'static,
        As: Send + Sync + 'static,
    {
        let Some(confirmed_frame) = confirmed_frame else {
            return;
        };

        snapshots.confirm(confirmed_frame.0);
    }

    /// A system which syncs the snapshot depth to [`MaxPredictionWindow`].
    /// Runs before each save to ensure snapshots are never evicted prematurely
    /// when the prediction window exceeds the default depth.
    pub fn sync_depth(mut snapshots: ResMut<Self>, max_prediction: Option<Res<MaxPredictionWindow>>)
    where
        For: Send + Sync + 'static,
        As: Send + Sync + 'static,
    {
        let Some(max_prediction) = max_prediction else {
            return;
        };

        snapshots.set_depth(max_prediction.0);
    }
}

/// A storage type suitable for per-[`Entity`] snapshots, such as [`Component`] types.
pub struct GgrsComponentSnapshot<For, As = For> {
    snapshot: HashMap<RollbackId, As>,
    _phantom: PhantomData<For>,
}

impl<For, As> Default for GgrsComponentSnapshot<For, As> {
    fn default() -> Self {
        Self {
            snapshot: default(),
            _phantom: default(),
        }
    }
}

impl<For, As> GgrsComponentSnapshot<For, As> {
    /// Create a new snapshot from a list of [`Rollback`] flags and stored [`Component`] types.
    pub fn new(components: impl IntoIterator<Item = (RollbackId, As)>) -> Self {
        Self {
            snapshot: components.into_iter().collect(),
            ..default()
        }
    }

    /// Insert a single snapshot for the provided [`Rollback`].
    pub fn insert(&mut self, entity: RollbackId, snapshot: As) -> &mut Self {
        self.snapshot.insert(entity, snapshot);
        self
    }

    /// Get a single snapshot for the provided [`Rollback`].
    pub fn get(&self, entity: &RollbackId) -> Option<&As> {
        self.snapshot.get(entity)
    }

    /// Iterate over all stored snapshots.
    pub fn iter(&self) -> impl Iterator<Item = (&RollbackId, &As)> + '_ {
        self.snapshot.iter()
    }
}

/// Returns a hasher built using the `seahash` library appropriate for creating portable checksums.
pub fn checksum_hasher() -> SeaHasher {
    SeaHasher::new()
}

/// This plugin sets up the [`LoadWorld`], [`SaveWorld`], and [`AdvanceWorld`]
/// schedules and adds the required systems and resources for basic rollback
/// functionality.
///
/// This is independent of the GGRS plugin and can be used with any Bevy app,
/// including tests and benchmarks.
pub struct SnapshotPlugin;

impl Plugin for SnapshotPlugin {
    /// Registers the rollback schedules, frame-count resources, and core snapshot plugins.
    fn build(&self, app: &mut App) {
        app.add_plugins(SnapshotSetPlugin)
            .init_resource::<RollbackOrdered>()
            .init_resource::<RollbackFrameCount>()
            .init_resource::<ConfirmedFrameCount>()
            .init_schedule(LoadWorld)
            .init_schedule(SaveWorld)
            .init_schedule(AdvanceWorld)
            .add_plugins((
                EntitySnapshotPlugin,
                ResourceSnapshotPlugin::<CloneStrategy<RollbackOrdered>>::default(),
                ChildOfSnapshotPlugin,
                RollbackDespawnPlugin,
            ));
    }

    fn finish(&self, app: &mut App) {
        if app.world().contains_resource::<SkipAutoRegistration>() {
            return;
        }

        let Some(registry) = app.world().get_resource::<AppTypeRegistry>() else {
            return;
        };

        let registry = registry.read();
        let registrations: Vec<fn(&mut App)> = registry
            .iter_with_data::<ReflectRollback>()
            .map(|(_, data)| data.register_fn)
            .collect();
        drop(registry);

        for register_fn in registrations {
            (register_fn)(app);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use bevy::prelude::*;

    use super::{AdvanceWorld, GgrsSnapshots, LoadWorld, RollbackFrameCount, SaveWorld};

    // ---- GgrsSnapshots unit tests ----

    type Snap = GgrsSnapshots<u32, u32>;

    fn snap_with_depth(depth: usize) -> Snap {
        let mut s = Snap::default();
        s.set_depth(depth);
        s
    }

    // --- push ---

    /// When depth is exceeded, the oldest frames are evicted.
    #[test]
    fn push_evicts_oldest_when_depth_exceeded() {
        let mut s = snap_with_depth(3);
        for i in 0..5_i32 {
            s.push(i, i as u32);
        }
        // Only frames 2, 3, 4 should survive
        assert!(s.peek(0).is_none());
        assert!(s.peek(1).is_none());
        assert_eq!(s.peek(2), Some(&2));
        assert_eq!(s.peek(3), Some(&3));
        assert_eq!(s.peek(4), Some(&4));
    }

    /// Pushing the same frame twice replaces the old snapshot.
    #[test]
    fn push_same_frame_replaces() {
        let mut s = snap_with_depth(8);
        s.push(3, 10);
        s.push(3, 20);
        assert_eq!(s.peek(3), Some(&20));
    }

    // --- confirm ---

    /// Confirming a frame prunes all snapshots strictly before it.
    #[test]
    fn confirm_prunes_older_frames() {
        let mut s = snap_with_depth(8);
        for i in 0..6_i32 {
            s.push(i, i as u32);
        }
        s.confirm(3);
        assert!(s.peek(0).is_none());
        assert!(s.peek(1).is_none());
        assert!(s.peek(2).is_none());
        // Frame 3 itself is kept (confirm is exclusive lower bound)
        assert_eq!(s.peek(3), Some(&3));
        assert_eq!(s.peek(4), Some(&4));
        assert_eq!(s.peek(5), Some(&5));
    }

    /// Confirming beyond all stored frames leaves the storage empty.
    #[test]
    fn confirm_beyond_all_frames_empties_storage() {
        let mut s = snap_with_depth(8);
        for i in 0..4_i32 {
            s.push(i, i as u32);
        }
        s.confirm(100);
        for i in 0..4_i32 {
            assert!(s.peek(i).is_none());
        }
    }

    /// Confirming on an empty storage does not panic.
    #[test]
    fn confirm_on_empty_does_not_panic() {
        let mut s: Snap = snap_with_depth(8);
        s.confirm(5); // should not panic
    }

    // --- rollback ---

    /// Rollback to an existing frame succeeds and positions the cursor there.
    #[test]
    fn rollback_to_existing_frame() {
        let mut s = snap_with_depth(8);
        for i in 0..5_i32 {
            s.push(i, i as u32 * 10);
        }
        s.rollback(2);
        assert_eq!(s.get(), &20);
    }

    /// Rollback to a missing frame panics.
    #[test]
    #[should_panic(expected = "Could not rollback to 99")]
    fn rollback_missing_frame_panics() {
        let mut s = snap_with_depth(8);
        s.push(0, 0);
        s.rollback(99);
    }

    // --- i32 wraparound ---

    /// Pushing i32::MIN after i32::MAX is a forward step across the wrap boundary.
    /// History frames near i32::MAX are retained (they're older context), and i32::MIN
    /// is prepended as the newest snapshot.
    #[test]
    fn push_wraps_i32_max_to_min_retains_history() {
        let mut s = snap_with_depth(8);
        s.push(i32::MAX - 2, 1);
        s.push(i32::MAX - 1, 2);
        s.push(i32::MAX, 3);
        // i32::MIN is "after" i32::MAX in GGRS frame counting (forward wrap).
        // The old frames are history and must be retained.
        s.push(i32::MIN, 4);
        assert_eq!(s.peek(i32::MAX - 2), Some(&1));
        assert_eq!(s.peek(i32::MAX - 1), Some(&2));
        assert_eq!(s.peek(i32::MAX), Some(&3));
        assert_eq!(s.peek(i32::MIN), Some(&4));
    }

    /// Saves the world by running the [`SaveWorld`] schedule.
    pub(crate) fn save_world(world: &mut World) {
        world.run_schedule(SaveWorld);
    }

    /// Advances the world by one frame, running the [`AdvanceWorld`] schedule.
    ///
    /// assumes input has already been updated
    pub(crate) fn advance_frame(world: &mut World) -> i32 {
        let mut frame_count = world
            .get_resource_mut::<RollbackFrameCount>()
            .expect("Unable to find GGRS RollbackFrameCount. Did you remove it?");
        frame_count.0 += 1;
        let frame = frame_count.0;
        world.run_schedule(AdvanceWorld);
        frame
    }

    /// Loads the world from the provided frame, by running the [`LoadWorld`] schedule.
    pub(crate) fn load_world(world: &mut World, frame: i32) {
        world
            .get_resource_mut::<RollbackFrameCount>()
            .expect("Unable to find GGRS RollbackFrameCount. Did you remove it?")
            .0 = frame;
        world.run_schedule(LoadWorld);
    }
}
