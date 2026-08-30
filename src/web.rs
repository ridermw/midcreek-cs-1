//! Browser packaging support for the production game.
//!
//! The readiness model is pure and compiled on every target so it can be
//! tested natively. Only the Bevy plugin and the DOM handshake are compiled
//! for `wasm32`.

use bevy::prelude::*;

use crate::{assets::AssetLoadState, hud::HudError, player::PlayerRigState, world::HallState};

/// How many settled `PostUpdate` frames the browser handshake waits for after
/// every production subsystem reports ready. Two frames prove the game
/// survived a full simulate-and-present cycle rather than a single lucky one.
pub const READY_SETTLE_FRAMES: u32 = 2;

/// Longest sanitized failure detail the browser is ever shown, in bytes.
pub const MAX_ERROR_DETAIL: usize = 200;

/// The named production subsystems the handshake waits on, in report order.
pub const SUBSYSTEM_NAMES: [&str; 5] = ["assets", "hall", "player", "operations", "hud"];

/// What the browser bootstrap is told about the running game.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebSignal {
    /// The game is instantiating and has not finished initializing.
    Loading,
    /// Every subsystem is ready and the game survived the settle frames.
    Ready,
    /// Initialization failed; the payload is a sanitized detail.
    Error(String),
}

impl WebSignal {
    /// The `data-game-state` value this signal publishes.
    pub const fn state(&self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Error(_) => "error",
        }
    }
}

/// Readiness of one production subsystem.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubsystemStatus {
    /// Still initializing.
    Pending,
    /// Initialized and healthy.
    Ready,
    /// Initialization failed and cannot recover.
    Failed,
}

/// Every production subsystem the browser handshake waits on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WebSubsystems {
    /// Generated glTF documents and module scenes.
    pub assets: SubsystemStatus,
    /// The spawned data hall.
    pub hall: SubsystemStatus,
    /// The bound technician rig.
    pub player: SubsystemStatus,
    /// The attached rack roster.
    pub operations: SubsystemStatus,
    /// The drawn HUD and diegetic badges.
    pub hud: SubsystemStatus,
}

impl WebSubsystems {
    /// Every status, in the same order as [`SUBSYSTEM_NAMES`].
    pub const fn statuses(&self) -> [SubsystemStatus; 5] {
        [
            self.assets,
            self.hall,
            self.player,
            self.operations,
            self.hud,
        ]
    }

    /// Whether every subsystem reported ready this frame.
    pub fn all_ready(&self) -> bool {
        self.statuses()
            .iter()
            .all(|status| *status == SubsystemStatus::Ready)
    }

    /// The names of every failed subsystem, in declaration order.
    pub fn failures(&self) -> Vec<&'static str> {
        self.statuses()
            .into_iter()
            .zip(SUBSYSTEM_NAMES)
            .filter(|(status, _)| *status == SubsystemStatus::Failed)
            .map(|(_, name)| name)
            .collect()
    }
}

/// The latched readiness handshake. It signals `Ready` or `Error` exactly once
/// so a late frame can never downgrade a browser that already reported ready.
#[derive(Resource, Clone, Debug, Default, Eq, PartialEq)]
pub struct WebReadiness {
    settled: u32,
    signalled: Option<WebSignal>,
}

impl WebReadiness {
    /// Whether a terminal signal was already published.
    pub const fn is_settled(&self) -> bool {
        self.signalled.is_some()
    }

    /// Consecutive settled frames observed so far.
    pub const fn settled_frames(&self) -> u32 {
        self.settled
    }

    /// Records one frame of subsystem readiness and returns the signal the
    /// browser must be told about, if it changed this frame.
    pub fn observe(&mut self, subsystems: WebSubsystems, detail: &str) -> Option<WebSignal> {
        if self.signalled.is_some() {
            return None;
        }

        let failures = subsystems.failures();
        if !failures.is_empty() {
            let names = failures.join(", ");
            let detail = sanitize_detail(detail);
            let message = if detail.is_empty() {
                names
            } else {
                format!("{names}: {detail}")
            };
            let signal = WebSignal::Error(message);
            self.signalled = Some(signal.clone());
            return Some(signal);
        }

        if !subsystems.all_ready() {
            self.settled = 0;
            return None;
        }

        self.settled += 1;
        if self.settled < READY_SETTLE_FRAMES {
            return None;
        }
        self.signalled = Some(WebSignal::Ready);
        Some(WebSignal::Ready)
    }
}

/// Reduces a raw failure message to something safe to publish in a browser:
/// no control characters, no absolute paths, and a bounded length.
pub fn sanitize_detail(detail: &str) -> String {
    let replaced = detail
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let sanitized = replaced
        .split_whitespace()
        .map(strip_absolute_path)
        .collect::<Vec<_>>()
        .join(" ");
    truncate_detail(&sanitized)
}

fn strip_absolute_path(token: &str) -> &str {
    let absolute = token.starts_with('/')
        || token.contains("://")
        || token.starts_with("file:")
        || is_windows_drive_path(token);
    if !absolute {
        return token;
    }
    token
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(token)
}

fn is_windows_drive_path(token: &str) -> bool {
    let mut characters = token.chars();
    let drive = characters
        .next()
        .is_some_and(|value| value.is_ascii_alphabetic());
    let colon = characters.next() == Some(':');
    let separator = matches!(characters.next(), Some('\\') | Some('/'));
    drive && colon && separator
}

fn truncate_detail(detail: &str) -> String {
    if detail.len() <= MAX_ERROR_DETAIL {
        return detail.to_owned();
    }
    let mut end = MAX_ERROR_DETAIL - 3;
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &detail[..end])
}

/// Maps the generated-asset lifecycle onto a handshake status.
pub const fn asset_status(state: AssetLoadState) -> SubsystemStatus {
    match state {
        AssetLoadState::Loading => SubsystemStatus::Pending,
        AssetLoadState::Ready => SubsystemStatus::Ready,
        AssetLoadState::Failed => SubsystemStatus::Failed,
    }
}

/// Maps the hall lifecycle onto a handshake status.
pub const fn hall_status(state: HallState) -> SubsystemStatus {
    match state {
        HallState::Unbuilt => SubsystemStatus::Pending,
        HallState::Ready => SubsystemStatus::Ready,
        HallState::Invalid => SubsystemStatus::Failed,
    }
}

/// Maps the technician rig lifecycle onto a handshake status.
pub const fn player_status(state: PlayerRigState) -> SubsystemStatus {
    match state {
        PlayerRigState::Pending => SubsystemStatus::Pending,
        PlayerRigState::Ready => SubsystemStatus::Ready,
        PlayerRigState::Failed => SubsystemStatus::Failed,
    }
}

/// Operations are ready once the rack roster is attached. An empty roster is
/// an unfinished spawn, never a failure.
pub const fn operations_status(racks: usize) -> SubsystemStatus {
    if racks == 0 {
        SubsystemStatus::Pending
    } else {
        SubsystemStatus::Ready
    }
}

/// Maps the HUD report onto a handshake status. A missing camera or viewport
/// is an unfinished startup frame, so it stays pending; anything else the HUD
/// could not present is a real failure.
pub fn hud_status(errors: &[HudError], viewport: Vec2) -> SubsystemStatus {
    if errors
        .iter()
        .any(|error| !matches!(error, HudError::NoCamera | HudError::NoViewport))
    {
        return SubsystemStatus::Failed;
    }
    if errors.is_empty() && viewport.x > 0.0 && viewport.y > 0.0 {
        SubsystemStatus::Ready
    } else {
        SubsystemStatus::Pending
    }
}

#[cfg(target_arch = "wasm32")]
pub use platform::{WebReadyPlugin, publish_signal};

#[cfg(target_arch = "wasm32")]
mod platform {
    use super::{
        WebReadiness, WebSignal, WebSubsystems, asset_status, hall_status, hud_status,
        operations_status, player_status, sanitize_detail,
    };
    use crate::{
        CellShiftSet,
        assets::{AssetLoadReport, AssetLoadState},
        hud::HudReport,
        operations::RackRoster,
        player::{PlayerRigReport, PlayerRigState},
        world::{HallErrors, HallState},
    };
    use bevy::prelude::*;

    /// Publishes the browser handshake for the production game. Compiled only
    /// for `wasm32`, where a real DOM exists to receive it.
    pub struct WebReadyPlugin;

    impl Plugin for WebReadyPlugin {
        fn build(&self, app: &mut App) {
            app.init_resource::<WebReadiness>()
                .add_systems(Startup, announce_loading)
                .add_systems(
                    PostUpdate,
                    publish_readiness.after(CellShiftSet::VerificationProbe),
                );
        }
    }

    fn announce_loading() {
        publish_signal(&WebSignal::Loading);
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the handshake deliberately reads every production subsystem it waits on"
    )]
    fn publish_readiness(
        mut readiness: ResMut<WebReadiness>,
        assets: Res<State<AssetLoadState>>,
        asset_report: Res<AssetLoadReport>,
        hall: Res<State<HallState>>,
        hall_errors: Res<HallErrors>,
        rig: Res<State<PlayerRigState>>,
        rig_report: Res<PlayerRigReport>,
        roster: Option<Res<RackRoster>>,
        hud: Res<HudReport>,
    ) {
        if readiness.is_settled() {
            return;
        }

        let subsystems = WebSubsystems {
            assets: asset_status(*assets.get()),
            hall: hall_status(*hall.get()),
            player: player_status(*rig.get()),
            operations: operations_status(roster.map_or(0, |roster| roster.len())),
            hud: hud_status(&hud.errors, hud.viewport),
        };

        let detail = if subsystems.failures().is_empty() {
            String::new()
        } else {
            failure_detail(&asset_report, &hall_errors, &rig_report, &hud)
        };

        if let Some(signal) = readiness.observe(subsystems, &detail) {
            publish_signal(&signal);
        }
    }

    fn failure_detail(
        assets: &AssetLoadReport,
        hall: &HallErrors,
        rig: &PlayerRigReport,
        hud: &HudReport,
    ) -> String {
        let mut parts = assets.failures().to_vec();
        parts.extend(hall.errors().iter().map(|error| format!("{error:?}")));
        parts.extend(rig.errors().iter().map(|error| format!("{error:?}")));
        parts.extend(
            hud.errors
                .iter()
                .filter(|error| {
                    !matches!(
                        error,
                        crate::hud::HudError::NoCamera | crate::hud::HudError::NoViewport
                    )
                })
                .map(|error| format!("{error:?}")),
        );
        parts.join("; ")
    }

    /// Writes one handshake signal into the document the bootstrap owns.
    pub fn publish_signal(signal: &WebSignal) {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        if let Some(body) = document.body() {
            let _ = body.set_attribute("data-game-state", signal.state());
        }
        let WebSignal::Error(message) = signal else {
            return;
        };
        let Some(sink) = document.get_element_by_id("browser-errors") else {
            return;
        };
        let existing = sink.text_content().unwrap_or_default();
        let detail = sanitize_detail(message);
        let joined = if existing.trim().is_empty() {
            detail
        } else {
            format!("{existing}\n{detail}")
        };
        sink.set_text_content(Some(&joined));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(status: SubsystemStatus) -> WebSubsystems {
        WebSubsystems {
            assets: status,
            hall: status,
            player: status,
            operations: status,
            hud: status,
        }
    }

    #[test]
    fn ready_requires_two_settled_frames_after_every_subsystem_is_ready() {
        let mut readiness = WebReadiness::default();

        assert_eq!(readiness.observe(all(SubsystemStatus::Ready), ""), None);
        assert_eq!(
            readiness.observe(all(SubsystemStatus::Ready), ""),
            Some(WebSignal::Ready)
        );
    }

    #[test]
    fn a_pending_subsystem_restarts_the_settled_frame_count() {
        let mut readiness = WebReadiness::default();
        let mut subsystems = all(SubsystemStatus::Ready);

        assert_eq!(readiness.observe(subsystems, ""), None);
        subsystems.hud = SubsystemStatus::Pending;
        assert_eq!(readiness.observe(subsystems, ""), None);
        subsystems.hud = SubsystemStatus::Ready;
        assert_eq!(readiness.observe(subsystems, ""), None);
        assert_eq!(readiness.observe(subsystems, ""), Some(WebSignal::Ready));
    }

    #[test]
    fn a_failed_subsystem_signals_an_error_that_names_it() {
        let mut readiness = WebReadiness::default();
        let mut subsystems = all(SubsystemStatus::Ready);
        subsystems.assets = SubsystemStatus::Failed;

        let signal = readiness.observe(subsystems, "generated/rack.glb failed to load");

        assert_eq!(
            signal,
            Some(WebSignal::Error(
                "assets: generated/rack.glb failed to load".to_owned()
            ))
        );
    }

    #[test]
    fn every_failed_subsystem_is_named_in_declaration_order() {
        let mut readiness = WebReadiness::default();
        let mut subsystems = all(SubsystemStatus::Ready);
        subsystems.hud = SubsystemStatus::Failed;
        subsystems.hall = SubsystemStatus::Failed;

        assert_eq!(
            readiness.observe(subsystems, ""),
            Some(WebSignal::Error("hall, hud".to_owned()))
        );
    }

    #[test]
    fn an_error_is_signalled_once_and_latches() {
        let mut readiness = WebReadiness::default();
        let mut subsystems = all(SubsystemStatus::Ready);
        subsystems.player = SubsystemStatus::Failed;

        assert!(readiness.observe(subsystems, "stale part").is_some());
        assert_eq!(readiness.observe(subsystems, "stale part"), None);
        assert_eq!(readiness.observe(all(SubsystemStatus::Ready), ""), None);
        assert_eq!(readiness.observe(all(SubsystemStatus::Ready), ""), None);
    }

    #[test]
    fn ready_is_signalled_once_and_is_not_replaced_by_a_later_failure() {
        let mut readiness = WebReadiness::default();
        readiness.observe(all(SubsystemStatus::Ready), "");

        assert_eq!(
            readiness.observe(all(SubsystemStatus::Ready), ""),
            Some(WebSignal::Ready)
        );
        assert_eq!(readiness.observe(all(SubsystemStatus::Ready), ""), None);
        assert_eq!(
            readiness.observe(all(SubsystemStatus::Failed), "late"),
            None
        );
    }

    #[test]
    fn sanitize_detail_replaces_absolute_paths_with_file_names() {
        assert_eq!(
            sanitize_detail("/Users/someone/secret/assets/generated/rack.glb failed to load"),
            "rack.glb failed to load"
        );
        assert_eq!(
            sanitize_detail("loading file:///C:/build/out/hall.glb"),
            "loading hall.glb"
        );
        assert_eq!(
            sanitize_detail(r"C:\Users\someone\rack.glb is missing"),
            "rack.glb is missing"
        );
    }

    #[test]
    fn sanitize_detail_collapses_control_characters_and_whitespace() {
        assert_eq!(
            sanitize_detail("first\n\tsecond\u{0}   third  "),
            "first second third"
        );
    }

    #[test]
    fn sanitize_detail_truncates_long_messages() {
        let detail = "e".repeat(MAX_ERROR_DETAIL * 2);

        let sanitized = sanitize_detail(&detail);

        assert_eq!(sanitized.len(), MAX_ERROR_DETAIL);
        assert!(sanitized.ends_with("..."));
    }

    #[test]
    fn a_failure_detail_is_sanitized_before_it_reaches_the_browser() {
        let mut readiness = WebReadiness::default();
        let mut subsystems = all(SubsystemStatus::Ready);
        subsystems.assets = SubsystemStatus::Failed;

        assert_eq!(
            readiness.observe(subsystems, "/Users/someone/assets/rack.glb\nis missing"),
            Some(WebSignal::Error("assets: rack.glb is missing".to_owned()))
        );
    }

    #[test]
    fn subsystem_status_follows_the_production_asset_lifecycle() {
        assert_eq!(
            asset_status(AssetLoadState::Loading),
            SubsystemStatus::Pending
        );
        assert_eq!(asset_status(AssetLoadState::Ready), SubsystemStatus::Ready);
        assert_eq!(
            asset_status(AssetLoadState::Failed),
            SubsystemStatus::Failed
        );
    }

    #[test]
    fn subsystem_status_follows_the_production_hall_lifecycle() {
        assert_eq!(hall_status(HallState::Unbuilt), SubsystemStatus::Pending);
        assert_eq!(hall_status(HallState::Ready), SubsystemStatus::Ready);
        assert_eq!(hall_status(HallState::Invalid), SubsystemStatus::Failed);
    }

    #[test]
    fn subsystem_status_follows_the_production_rig_lifecycle() {
        assert_eq!(
            player_status(PlayerRigState::Pending),
            SubsystemStatus::Pending
        );
        assert_eq!(player_status(PlayerRigState::Ready), SubsystemStatus::Ready);
        assert_eq!(
            player_status(PlayerRigState::Failed),
            SubsystemStatus::Failed
        );
    }

    #[test]
    fn operations_are_pending_until_the_rack_roster_is_attached() {
        assert_eq!(operations_status(0), SubsystemStatus::Pending);
        assert_eq!(operations_status(16), SubsystemStatus::Ready);
    }

    #[test]
    fn a_hud_without_a_camera_or_viewport_is_still_initialising() {
        assert_eq!(
            hud_status(&[HudError::NoCamera], Vec2::ZERO),
            SubsystemStatus::Pending
        );
        assert_eq!(
            hud_status(&[HudError::NoViewport], Vec2::new(1280.0, 720.0)),
            SubsystemStatus::Pending
        );
        assert_eq!(hud_status(&[], Vec2::ZERO), SubsystemStatus::Pending);
    }

    #[test]
    fn a_hud_that_cannot_present_a_rack_is_a_failure() {
        assert_eq!(
            hud_status(
                &[HudError::MissingBadgeNode { rack: 3 }],
                Vec2::new(1280.0, 720.0)
            ),
            SubsystemStatus::Failed
        );
    }

    #[test]
    fn a_healthy_hud_with_a_real_viewport_is_ready() {
        assert_eq!(
            hud_status(&[], Vec2::new(1280.0, 720.0)),
            SubsystemStatus::Ready
        );
    }
}
