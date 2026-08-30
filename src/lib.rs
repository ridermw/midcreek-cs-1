pub mod assetgen;
pub mod assets;
pub mod design;
pub mod player;
pub mod sitegen;
pub mod world;

use bevy::prelude::*;

use design::{DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, FLOOR_LIGHT};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, SystemSet)]
pub enum CellShiftSet {
    AssetReady,
    SpawnWorld,
    ReadInput,
    UpdateOrbitIntent,
    UpdateOperations,
    MovePlayer,
    UpdateAnimation,
    FollowCamera,
    UpdateHudAndBadges,
    VerificationProbe,
}

pub struct CellShiftPlugin;

impl Plugin for CellShiftPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(FLOOR_LIGHT.into()))
            .add_plugins((
                assets::AssetPlugin,
                world::HallPlugin,
                player::TechnicianPlugin,
            ))
            .configure_sets(
                Update,
                (
                    CellShiftSet::AssetReady,
                    CellShiftSet::SpawnWorld,
                    CellShiftSet::ReadInput,
                    CellShiftSet::UpdateOrbitIntent,
                    CellShiftSet::UpdateOperations,
                    CellShiftSet::MovePlayer,
                    CellShiftSet::UpdateAnimation,
                    CellShiftSet::FollowCamera,
                    CellShiftSet::UpdateHudAndBadges,
                    CellShiftSet::VerificationProbe,
                )
                    .chain(),
            );
    }
}

pub fn run() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Cell Shift Data Center POC".into(),
                resolution: (DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(CellShiftPlugin)
        .run();
}
