use crate::DEFAULT_FPS;
use bevy::{
    ecs::reflect::AppTypeRegistry,
    ecs::schedule::ScheduleLabel,
    platform::collections::{HashMap, HashSet},
    prelude::*,
};
use seahash::SeaHasher;
use std::marker::PhantomData;

mod checksum;
mod checksum_diagnostics;
mod childof_snapshot;
mod component_checksum;
mod component_map;
mod component_snapshot;
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
pub use checksum_diagnostics::*;
pub use childof_snapshot::*;
pub use component_checksum::*;
pub use component_map::*;
pub use component_snapshot::*;
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
pub struct ConfirmedFrameCount(pub(crate) i32);

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

/// Collection of snapshots for a type `For`, stored as `As`.
///
/// Snapshot depth (how many frames to retain) is controlled globally via
/// [`SnapshotDepth`], not per-storage. Eviction happens in
/// [`discard_old_snapshots`](Self::discard_old_snapshots).
#[derive(Resource)]
pub struct GgrsSnapshots<For, As = For> {
    snapshots: HashMap<i32, As>,
    /// The frame selected by `rollback()`, used by `get()`.
    current_frame: Option<i32>,
    _phantom: PhantomData<For>,
}

impl<For, As> Default for GgrsSnapshots<For, As> {
    fn default() -> Self {
        Self {
            snapshots: HashMap::default(),
            current_frame: None,
            _phantom: PhantomData,
        }
    }
}

impl<For, As> GgrsSnapshots<For, As> {
    /// Store a snapshot for the provided frame, replacing any existing snapshot.
    pub fn push(&mut self, frame: i32, snapshot: As) -> &mut Self {
        self.snapshots.insert(frame, snapshot);
        self
    }

    /// Evict snapshots beyond the depth limit, keeping only the most recent `depth` unpinned frames.
    ///
    /// Pinned frames are never evicted regardless of the depth limit.
    pub fn evict(&mut self, depth: usize, pinned: &HashSet<i32>) {
        let is_unpinned = |f: &&i32| !pinned.contains(*f);
        while self.snapshots.keys().filter(is_unpinned).count() > depth {
            if let Some(&oldest) = self.snapshots.keys().filter(is_unpinned).min() {
                self.snapshots.remove(&oldest);
            } else {
                break;
            }
        }
    }

    /// Discards unpinned snapshots from before `confirmed_frame` as no longer required.
    pub fn confirm(&mut self, confirmed_frame: i32, pinned: &HashSet<i32>) -> &mut Self {
        self.snapshots
            .retain(|&frame, _| frame >= confirmed_frame || pinned.contains(&frame));
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

    /// A system which evicts old snapshots based on [`SnapshotDepth`] and
    /// confirms the [`ConfirmedFrameCount`]. Pinned frames (via [`PinnedFrames`])
    /// are never evicted or confirmed away.
    pub fn discard_old_snapshots(
        mut snapshots: ResMut<Self>,
        depth: Res<SnapshotDepth>,
        pinned: Res<PinnedFrames>,
        confirmed_frame: Option<Res<ConfirmedFrameCount>>,
    ) where
        For: Send + Sync + 'static,
        As: Send + Sync + 'static,
    {
        if let Some(max) = depth.0 {
            snapshots.evict(max, &pinned);
        }

        if let Some(confirmed_frame) = confirmed_frame {
            snapshots.confirm(confirmed_frame.0, &pinned);
        }
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

/// Global snapshot depth limit. `None` means unbounded.
///
/// Defaults to `Some(60)` (~1 second at 60 fps). Insert this resource
/// before adding [`SnapshotPlugin`] to override the default, or mutate it
/// at runtime.
///
/// Read by [`discard_old_snapshots`](GgrsSnapshots::discard_old_snapshots).
#[derive(Resource)]
pub struct SnapshotDepth(pub Option<usize>);

/// Set of frame numbers that should never be evicted by
/// [`discard_old_snapshots`](GgrsSnapshots::discard_old_snapshots).
///
/// Use this to implement sparse checkpoint systems: pin a frame every N ticks
/// so that seeking backward can load a nearby checkpoint and resimulate forward.
///
/// The dense rolling window ([`SnapshotDepth`]) and pinned frames are independent:
/// `evict()` only counts unpinned frames against the depth limit.
#[derive(Resource, Default, Debug, Clone, Deref, DerefMut, Reflect)]
#[reflect(Resource)]
pub struct PinnedFrames(pub HashSet<i32>);

impl Default for SnapshotDepth {
    fn default() -> Self {
        Self(Some(DEFAULT_FPS))
    }
}

/// This plugin sets up the [`LoadWorld`], [`SaveWorld`], and [`AdvanceWorld`]
/// schedules and adds the required systems and resources for basic rollback
/// functionality.
///
/// Snapshot depth is controlled via the [`SnapshotDepth`] resource.
/// Insert it before adding this plugin to override the default (60 frames).
///
/// This is independent of the GGRS plugin and can be used with any Bevy app,
/// including tests and benchmarks.
pub struct SnapshotPlugin;

impl Plugin for SnapshotPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SnapshotDepth>();
        app.init_resource::<PinnedFrames>();
        app.register_type::<PinnedFrames>();
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

    use super::{AdvanceWorld, LoadWorld, RollbackFrameCount, SaveWorld};

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
