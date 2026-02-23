use std::marker::PhantomData;

use bevy::prelude::*;

use super::{RollbackId, RollbackOrdered};

/// A [`Plugin`] which registers an observer on [`Add`] for component `T`, automatically
/// creating a [`RollbackId`] and registering in [`RollbackOrdered`] when `T` is added
/// to an entity.
///
/// This is an alternative to adding the [`Rollback`](`super::Rollback`) marker component
/// manually. Useful for types you control directly.
///
/// # Examples
/// ```rust,ignore
/// // Any entity that gets a Player component is automatically tracked for rollback
/// app.rollback_entities_with::<Player>();
/// ```
pub struct RollbackEntitiesPlugin<T: Component> {
    _phantom: PhantomData<T>,
}

impl<T: Component> Default for RollbackEntitiesPlugin<T> {
    fn default() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

fn rollback_entity_observer<T: Component>(
    trigger: On<Add, T>,
    mut commands: Commands,
    rollback_id_query: Query<&RollbackId>,
    mut ordered: ResMut<RollbackOrdered>,
) {
    let entity = trigger.entity;

    // Respawn path: RollbackId already present (e.g. during rollback restore)
    if rollback_id_query.get(entity).is_ok() {
        return;
    }

    let rollback_id = RollbackId::new(entity);
    commands.entity(entity).insert(rollback_id);
    ordered.push(rollback_id);
}

impl<T: Component> Plugin for RollbackEntitiesPlugin<T> {
    fn build(&self, app: &mut App) {
        app.add_observer(rollback_entity_observer::<T>);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{
        AdvanceWorld, RollbackApp, RollbackOrdered, SnapshotPlugin,
        tests::{advance_frame, load_world, save_world},
    };

    #[derive(Component, Clone, Copy)]
    struct Marker;

    #[derive(Component, Clone, Copy, Default)]
    struct Health(u32);

    /// Test that rollback_entities_with creates RollbackId automatically
    #[test]
    fn observer_creates_rollback_id() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(SnapshotPlugin);
        app.rollback_entities_with::<Marker>();
        app.update();

        // Spawn an entity with only Marker — no Rollback component
        let entity = app.world_mut().spawn(Marker).id();
        app.update(); // flush commands from observer

        // RollbackId should have been created by the observer
        assert!(
            app.world().get::<RollbackId>(entity).is_some(),
            "RollbackId should be created by observer"
        );

        // RollbackOrdered should have one entry
        let ordered = app.world().resource::<RollbackOrdered>();
        assert_eq!(ordered.len(), 1);
    }

    /// Test that entities with RollbackId are properly saved/restored during rollback
    #[test]
    fn observer_entity_rollback() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(SnapshotPlugin);
        app.rollback_entities_with::<Marker>();
        app.rollback_component_with_copy::<Health>();
        app.add_systems(
            AdvanceWorld,
            |mut query: Query<&mut Health, With<Marker>>| {
                for mut health in &mut query {
                    health.0 += 1;
                }
            },
        );
        app.update();

        // Spawn entity with Marker
        app.world_mut().spawn((Marker, Health(0)));
        app.update(); // flush observer commands

        save_world(app.world_mut()); // save frame 0
        advance_frame(app.world_mut()); // frame 1: health becomes 1

        {
            let health = app
                .world_mut()
                .query::<&Health>()
                .single(app.world())
                .unwrap();
            assert_eq!(health.0, 1);
        }

        // Roll back to frame 0
        load_world(app.world_mut(), 0);

        {
            let health = app
                .world_mut()
                .query::<&Health>()
                .single(app.world())
                .unwrap();
            assert_eq!(health.0, 0);
        }
    }

    /// Test idempotent push when entity has multiple registered component types
    #[test]
    fn multiple_observers_idempotent() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(SnapshotPlugin);
        app.rollback_entities_with::<Marker>();
        app.rollback_entities_with::<Health>();
        app.update();

        // Spawn entity with both components
        app.world_mut().spawn((Marker, Health(10)));
        app.update(); // flush observer commands

        // Should only have one entry in RollbackOrdered despite two observers
        let ordered = app.world().resource::<RollbackOrdered>();
        assert_eq!(ordered.len(), 1);
    }
}
