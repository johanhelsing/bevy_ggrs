use std::hash::{Hash, Hasher};

use bevy::prelude::*;

use crate::{
    ChecksumDiagnostics, ChecksumFlag, ChecksumPart, RollbackId, RollbackOrdered, SaveWorld,
    SaveWorldSystems, checksum_hasher,
};

/// A [`Plugin`] which will track the [`Component`] `C` on [`Rollback Entities`](`Rollback`) and ensure a
/// [`ChecksumPart`] is available and updated. This can be used to generate a [`Checksum`](`crate::Checksum`).
///
/// # Examples
/// ```rust
/// # use bevy::prelude::*;
/// # use bevy_ggrs::{prelude::*, ComponentChecksumPlugin};
/// #
/// # const FPS: usize = 60;
/// #
/// # type MyInputType = u8;
/// #
/// # fn read_local_inputs() {}
/// #
/// # fn start(session: Session<GgrsConfig<MyInputType>>) {
/// # let mut app = App::new();
/// #[derive(Component, Clone, Copy, Hash)]
/// struct Health(u32);
///
/// // To include something in the checksum, it should also be rolled back
/// app.rollback_component_with_clone::<Health>();
///
/// // This will update the checksum every frame to include Health on rollback entities
/// app.add_plugins(ComponentChecksumPlugin::<Health>::default());
/// # }
/// ```
pub struct ComponentChecksumPlugin<C: Component>(pub for<'a> fn(&'a C) -> u64);

fn default_hasher<C: Component + Hash>(component: &C) -> u64 {
    let mut hasher = checksum_hasher();
    component.hash(&mut hasher);
    hasher.finish()
}

impl<C> Default for ComponentChecksumPlugin<C>
where
    C: Component + Hash,
{
    fn default() -> Self {
        Self(default_hasher::<C>)
    }
}

impl<C> Plugin for ComponentChecksumPlugin<C>
where
    C: Component,
{
    fn build(&self, app: &mut App) {
        let custom_hasher = self.0;

        let update = move |mut commands: Commands,
                           rollback_ordered: Res<RollbackOrdered>,
                           components: Query<
            (&RollbackId, &C),
            (With<RollbackId>, Without<ChecksumFlag<C>>),
        >,
                           mut checksum: Query<
            &mut ChecksumPart,
            (Without<RollbackId>, With<ChecksumFlag<C>>),
        >,
                           diagnostics: Option<Res<ChecksumDiagnostics>>| {
            let mut hasher = checksum_hasher();

            let mut result = 0;
            let type_name = std::any::type_name::<C>();

            for (&rollback, component) in components.iter() {
                let mut hasher = hasher;

                // Hashing the rollback index ensures this hash is unique and stable
                let order = rollback_ordered.order(rollback);
                order.hash(&mut hasher);
                custom_hasher(component).hash(&mut hasher);

                // XOR chosen over addition or multiplication as it is closed on u64 and commutative
                let entity_hash = hasher.finish();
                result ^= entity_hash;

                if let Some(ref diag) = diagnostics {
                    diag.record_entity_hash(type_name, order, entity_hash);
                }
            }

            // Hash the XOR'ed result to break commutativity with other types
            result.hash(&mut hasher);

            let result = ChecksumPart(hasher.finish() as u128);

            trace!(
                "Component {} has checksum {:X}",
                disqualified::ShortName::of::<C>(),
                result.0
            );

            if let Some(ref diag) = diagnostics {
                diag.record_type_checksum(type_name, result.0);
            }

            if let Ok(mut checksum) = checksum.single_mut() {
                *checksum = result;
            } else {
                commands.spawn((result, ChecksumFlag::<C>::default()));
            }
        };

        app.add_systems(SaveWorld, update.in_set(SaveWorldSystems::Checksum));
    }
}
