pub mod assetgen;
pub mod assets;
pub mod camera;
pub mod design;
pub mod hud;
pub mod operations;
pub mod player;
pub mod sitegen;
/// Autonomous verification is a native-only gate; the browser build never
/// carries the harness, the analyzers, or the fixtures.
#[cfg(not(target_arch = "wasm32"))]
pub mod verification;
pub mod web;
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
                camera::CameraPlugin,
                operations::OperationsPlugin,
                hud::HudPlugin,
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
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(primary_window()),
                ..default()
            })
            .set(bevy::asset::AssetPlugin {
                meta_check: bevy::asset::AssetMetaCheck::Never,
                ..default()
            }),
    )
    .add_plugins(CellShiftPlugin);

    #[cfg(target_arch = "wasm32")]
    app.add_plugins(web::WebReadyPlugin);

    app.run();
}

/// Runs the same production game under the scripted verification journey.
///
/// Nothing about the game changes: the plugins, systems, hall, rig, scheduler,
/// and HUD are the ones `run` builds. Only the window presentation and the
/// deterministic driver are added, and the window is asked for exactly the
/// captured resolution with no scale factor so a screenshot is the frame the
/// contract names.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_verification(
    output: verification::VerifyOutput,
    fault: Option<verification::VerificationFault>,
) -> std::process::ExitCode {
    use bevy::window::{PresentMode, WindowResolution};

    let mut window = primary_window();
    window.title = "Cell Shift Verification".into();
    window.resolution = WindowResolution::new(DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT)
        .with_scale_factor_override(1.0);
    window.present_mode = PresentMode::AutoNoVsync;
    window.resizable = true;

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(window),
                ..default()
            })
            .set(bevy::asset::AssetPlugin {
                meta_check: bevy::asset::AssetMetaCheck::Never,
                ..default()
            }),
    )
    .add_plugins(CellShiftPlugin)
    .add_plugins(verification::VerificationPlugin::new(output, fault));

    match app.run() {
        AppExit::Success => std::process::ExitCode::SUCCESS,
        AppExit::Error(code) => std::process::ExitCode::from(code.get()),
    }
}

fn primary_window() -> Window {
    Window {
        title: "Cell Shift Data Center POC".into(),
        resolution: (DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT).into(),
        #[cfg(target_arch = "wasm32")]
        canvas: Some(WEB_CANVAS_SELECTOR.to_owned()),
        #[cfg(target_arch = "wasm32")]
        fit_canvas_to_parent: true,
        ..default()
    }
}

/// The canvas the browser shell owns and the game renders into.
#[cfg(target_arch = "wasm32")]
pub const WEB_CANVAS_SELECTOR: &str = "#game-canvas";
