//! The operations HUD: a prioritized ticket stack, compact control hints, and
//! floating rack badges with leader lines.
//!
//! The HUD owns no gameplay state. Every frame it reads [`TicketQueue`],
//! [`RackOperations`], [`RackRoster`], [`MovementLock`], [`LastInteraction`],
//! and the real camera, and writes only presentation components plus one
//! observable [`HudReport`]. There is no second ticket model.
//!
//! Badges are fixed-size screen-space UI nodes, not world-space sprites, so
//! they never rotate, shear, or change size with the camera. They are anchored
//! every frame from a stable rack world point through the real
//! [`Camera::world_to_viewport`], which is why they follow all four headings
//! and every tween sample for free:
//!
//! ```text
//! RackEntry.center (stable, authored)
//!            |
//!            | + RACK_BADGE_ANCHOR_HEIGHT on Y
//!            v
//!    anchor world point
//!            |
//!            | Camera::world_to_viewport, using this frame's camera
//!            | transform rather than last frame's propagated one
//!            v
//!    +-- Err ------------> BadgeVisibility::ProjectionFailed (+ HudError)
//!    +-- outside viewport -> BadgeVisibility::OffScreen
//!            v
//!    anchor viewport point
//!            |
//!            | lift BADGE_LIFT px, then clamp the whole badge box inside the
//!            | viewport so a visible anchor always has a visible badge
//!            v
//!    badge centre  --- thin leader line back to the anchor ---> anchor
//! ```
//!
//! Fixed panels are pinned to two corners so they never cover the middle of
//! the screen: the queue stack is narrower than a quarter of the smallest
//! supported viewport, and the control strip is shorter than a quarter of it,
//! so neither can intersect the central 50% x 50% play rectangle.

use std::time::Duration;

use bevy::{
    camera::ViewportConversionError,
    ecs::{relationship::RelatedSpawnerCommands, system::SystemParam},
    prelude::*,
};

use crate::{
    CellShiftSet,
    camera::CellShiftCamera,
    design::{MAX_ACTIVE_TICKETS, PaletteRole, REPAIR_INTERACTION_RANGE},
    operations::{
        InteractionOutcome, LastInteraction, MovementLock, RackEntry, RackOperations, RackRoster,
        RackState, TicketId, TicketQueue, TicketSeverity, operations_are_ready, rack_distance,
    },
    player::Technician,
};

// ---------------------------------------------------------------------------
// Fixed layout constants
// ---------------------------------------------------------------------------

/// Distance every fixed panel keeps from every viewport edge, in logical px.
pub const HUD_MARGIN: f32 = 16.0;

/// Width of the top-left queue stack, in logical px.
///
/// The verification viewport is 960 px wide, so a quarter of it is 240 px. A
/// 216 px panel pinned 16 px from the left edge ends at 232 px, which is left
/// of the central play rectangle at both supported sizes.
pub const QUEUE_PANEL_WIDTH: f32 = 216.0;

/// Padding inside every fixed panel, in logical px.
pub const HUD_PANEL_PADDING: f32 = 8.0;

/// Height of one queue row, in logical px.
pub const QUEUE_ROW_HEIGHT: f32 = 18.0;

/// Vertical gap between queue rows, in logical px.
pub const QUEUE_ROW_GAP: f32 = 3.0;

/// Edge of the square severity chip that leads every queue row, in logical px.
pub const QUEUE_CHIP_SIZE: f32 = 12.0;

/// Height of the thin repair-progress bar under the queue rows, in logical px.
pub const QUEUE_PROGRESS_HEIGHT: f32 = 3.0;

/// Font size of every queue row label, in logical px.
pub const QUEUE_LABEL_FONT_SIZE: f32 = 12.0;

/// Font size of the panel headers and control hints, in logical px.
pub const HUD_SMALL_FONT_SIZE: f32 = 11.0;

/// Height of the bottom-right control strip, in logical px.
///
/// The verification viewport is 540 px tall, so a quarter of it is 135 px. A
/// 40 px strip pinned 16 px from the bottom starts at 484 px, which is below
/// the central play rectangle at both supported sizes.
pub const CONTROLS_PANEL_HEIGHT: f32 = 40.0;

/// Width of one badge, in logical px. Badges never scale with the camera.
pub const BADGE_WIDTH: f32 = 34.0;

/// Height of one badge, in logical px.
pub const BADGE_HEIGHT: f32 = 22.0;

/// How far above its projected anchor a badge floats, in logical px.
pub const BADGE_LIFT: f32 = 54.0;

/// How far a badge stays clear of the viewport edge when it is clamped.
pub const BADGE_EDGE_MARGIN: f32 = 6.0;

/// Thickness of a leader line, in logical px.
pub const LEADER_WIDTH: f32 = 2.0;

/// Gap between the badge edge and the start of its leader line, in logical px.
pub const LEADER_GAP: f32 = 2.0;

/// The top of the authored rack cabinets, in metres: the `cabinet-top` module
/// is centred at 2.16 m with a 0.03 m half height.
pub const AUTHORED_RACK_TOP_HEIGHT: f32 = 2.19;

/// Height above the rack's ground centre that badges are anchored to, in
/// metres, chosen to float the anchor just clear of the rack itself.
pub const RACK_BADGE_ANCHOR_HEIGHT: f32 = 2.4;

/// Opacity of the ink backing behind every fixed panel and badge.
pub const HUD_PANEL_ALPHA: f32 = 0.86;

/// Half extents of one badge, in logical px.
pub fn badge_half_extents() -> Vec2 {
    Vec2::new(BADGE_WIDTH, BADGE_HEIGHT) * 0.5
}

/// One typed palette colour, opaque.
pub fn hud_color(role: PaletteRole) -> Color {
    Color::from(role.color())
}

/// The ink backing every panel and badge is drawn on.
pub fn hud_panel_color() -> Color {
    Color::from(PaletteRole::Ink.color().with_alpha(HUD_PANEL_ALPHA))
}

// ---------------------------------------------------------------------------
// Typed presentation state
// ---------------------------------------------------------------------------

/// Which diegetic badge one rack shows.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BadgeKind {
    /// An open fault: red, sharp cornered, shouting.
    Fault,
    /// A running repair: blue, rounded, the technician is on it.
    Repairing,
    /// A completed repair, shown briefly: healthy green.
    Resolved,
}

impl BadgeKind {
    /// Every badge, in state-machine order.
    pub const ALL: [Self; 3] = [Self::Fault, Self::Repairing, Self::Resolved];

    /// The badge a rack in this state shows, if any. Healthy and cooling racks
    /// show nothing at all.
    pub const fn for_state(state: RackState) -> Option<Self> {
        match state {
            RackState::Faulted => Some(Self::Fault),
            RackState::Repairing => Some(Self::Repairing),
            RackState::Resolved => Some(Self::Resolved),
            RackState::Healthy | RackState::Cooldown => None,
        }
    }

    /// The typed palette role this badge is filled with.
    pub const fn role(self) -> PaletteRole {
        match self {
            Self::Fault => PaletteRole::FaultRed,
            Self::Repairing => PaletteRole::WorkerHardHat,
            Self::Resolved => PaletteRole::HealthyGreen,
        }
    }

    /// The typed palette role this badge's glyph is drawn in.
    pub const fn text_role(self) -> PaletteRole {
        match self {
            Self::Fault | Self::Repairing => PaletteRole::RackWhite,
            Self::Resolved => PaletteRole::Ink,
        }
    }

    /// The short glyph inside the badge.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fault => "!",
            Self::Repairing => "FIX",
            Self::Resolved => "OK",
        }
    }

    /// Corner radius, in logical px, so the three badges differ by shape as
    /// well as by colour.
    pub const fn corner_radius(self) -> f32 {
        match self {
            Self::Fault => 0.0,
            Self::Repairing => 6.0,
            Self::Resolved => BADGE_HEIGHT * 0.5,
        }
    }
}

/// The one-line status the queue stack ends with.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum HudStatus {
    /// Nothing is open and nothing was rejected.
    #[default]
    AllHealthy,
    /// At least one ticket is open and waiting.
    TicketsOpen,
    /// A repair is running and movement is locked.
    Repairing,
    /// The last `Space` press was rejected for range.
    MoveCloser,
    /// The last `Space` press found no open ticket at all.
    NoOpenTickets,
}

impl HudStatus {
    /// Every status, in declaration order.
    pub const ALL: [Self; 5] = [
        Self::AllHealthy,
        Self::TicketsOpen,
        Self::Repairing,
        Self::MoveCloser,
        Self::NoOpenTickets,
    ];

    /// The short label this status shows.
    pub const fn label(self) -> &'static str {
        match self {
            Self::AllHealthy => "All racks healthy",
            Self::TicketsOpen => "Tickets waiting",
            Self::Repairing => "Repair running",
            Self::MoveCloser => "Move closer",
            Self::NoOpenTickets => "No open tickets",
        }
    }

    /// The typed palette role this status is drawn in.
    pub const fn role(self) -> PaletteRole {
        match self {
            Self::AllHealthy => PaletteRole::HealthyGreen,
            Self::TicketsOpen => PaletteRole::SignatureYellow,
            Self::Repairing => PaletteRole::WorkerHardHat,
            Self::MoveCloser => PaletteRole::SignatureYellow,
            Self::NoOpenTickets => PaletteRole::RackWhite,
        }
    }

    /// Derives the status from live operations state alone.
    ///
    /// A running repair outranks everything, because it is the only state that
    /// takes the controls away. Otherwise the most recent real rejection is
    /// shown while it is still true, and the queue itself is the fallback.
    ///
    /// "Still true" is checked against the one rack the rejection was about,
    /// not against the queue as a whole: see [`move_closer_still_stands`].
    pub fn derive(
        lock: &MovementLock,
        last: &LastInteraction,
        queue: &TicketQueue,
        roster: &RackRoster,
        racks: &[RackPresentation],
        technician: Option<Vec2>,
    ) -> Self {
        if lock.is_locked() {
            return Self::Repairing;
        }
        match last.outcome {
            InteractionOutcome::OutOfRange {
                nearest_rack: Some(rack),
                ..
            } if move_closer_still_stands(rack, queue, roster.get(rack), racks, technician) => {
                Self::MoveCloser
            }
            InteractionOutcome::NoOpenTickets if queue.is_empty() => Self::NoOpenTickets,
            _ if queue.is_empty() => Self::AllHealthy,
            _ => Self::TicketsOpen,
        }
    }
}

/// Whether the last out-of-range rejection is still true right now.
///
/// A rejection is always about one specific rack, the `nearest_rack` the real
/// Space press measured, so it must survive exactly as long as that rack is
/// still unreachable and still needs a repair, and no longer. It stops holding
/// the moment any of these becomes true:
///
/// - the rack's ticket left the live queue (repaired, resolved, removed);
/// - the rack is no longer `Faulted`, so there is nothing to walk up to;
/// - the rack is not on the roster, or the technician is not in the world;
/// - the technician walked inside [`REPAIR_INTERACTION_RANGE`] of it.
///
/// Every input is read live. Nothing here is cached between frames.
pub fn move_closer_still_stands(
    nearest_rack: usize,
    queue: &TicketQueue,
    entry: Option<&RackEntry>,
    racks: &[RackPresentation],
    technician: Option<Vec2>,
) -> bool {
    if queue.for_rack(nearest_rack).is_none() {
        return false;
    }
    let Some(rack) = racks.iter().find(|rack| rack.rack == nearest_rack) else {
        return false;
    };
    if rack.state != RackState::Faulted {
        return false;
    }
    let (Some(entry), Some(position)) = (entry, technician) else {
        return false;
    };
    rack_distance(position, entry.center, entry.half_extents) > REPAIR_INTERACTION_RANGE
}

/// The typed palette role one severity is drawn in.
pub const fn severity_role(severity: TicketSeverity) -> PaletteRole {
    match severity {
        TicketSeverity::Critical => PaletteRole::FaultRed,
        TicketSeverity::Warning => PaletteRole::SignatureYellow,
    }
}

/// The typed palette role one rack state is drawn in inside the queue stack.
pub const fn state_role(state: RackState) -> PaletteRole {
    match state {
        RackState::Faulted => PaletteRole::FaultRed,
        RackState::Repairing => PaletteRole::WorkerHardHat,
        RackState::Resolved => PaletteRole::HealthyGreen,
        RackState::Healthy | RackState::Cooldown => PaletteRole::RackShadow,
    }
}

/// Which control one bottom-right hint describes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HudControl {
    /// Arrow keys.
    Move,
    /// `Q`.
    TurnLeft,
    /// `E`.
    TurnRight,
    /// `Space`.
    Repair,
}

impl HudControl {
    /// Every hint, left to right.
    pub const ALL: [Self; 4] = [Self::Move, Self::TurnLeft, Self::TurnRight, Self::Repair];

    /// The key cap text.
    pub const fn keys(self) -> &'static str {
        match self {
            Self::Move => "Arrows",
            Self::TurnLeft => "Q",
            Self::TurnRight => "E",
            Self::Repair => "Space",
        }
    }

    /// The short action label beside the key cap.
    pub const fn action(self) -> &'static str {
        match self {
            Self::Move => "Move",
            Self::TurnLeft => "Turn L",
            Self::TurnRight => "Turn R",
            Self::Repair => "Repair",
        }
    }

    /// The typed palette role this key cap is filled with while a repair is or
    /// is not running. Movement is disabled during a repair and `Space` is
    /// what is running, so both say so.
    pub const fn cap_role(self, locked: bool) -> PaletteRole {
        match (self, locked) {
            (Self::Move, true) => PaletteRole::RackShadow,
            (Self::Repair, true) => PaletteRole::WorkerHardHat,
            _ => PaletteRole::RackWhite,
        }
    }

    /// The typed palette role this key cap's text is drawn in, chosen so it
    /// always reads against [`HudControl::cap_role`].
    pub const fn cap_text_role(self, locked: bool) -> PaletteRole {
        match (self, locked) {
            (Self::Repair, true) => PaletteRole::RackWhite,
            _ => PaletteRole::Ink,
        }
    }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// One rendered queue row, in priority order.
#[derive(Clone, Debug, PartialEq)]
pub struct HudRow {
    /// Which of the [`MAX_ACTIVE_TICKETS`] slots this row occupies.
    pub slot: usize,
    /// The ticket this row shows.
    pub ticket: TicketId,
    /// The rack that ticket belongs to.
    pub rack: usize,
    /// How urgent it is.
    pub severity: TicketSeverity,
    /// What that rack is doing right now.
    pub state: RackState,
    /// How far through its current timed state that rack is, in `0.0..=1.0`.
    /// Untimed states report zero.
    pub progress: f32,
    /// The short label the row renders.
    pub label: String,
}

/// Why one rack badge is or is not on screen.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BadgeVisibility {
    /// The badge is drawn.
    Shown,
    /// The rack has no badge state at all.
    NoTicket,
    /// The rack anchor projected outside the viewport.
    OffScreen,
    /// The real projection refused the anchor.
    ProjectionFailed,
    /// There is no usable game camera to project through.
    NoCamera,
    /// A game camera exists but has no viewport size yet, so nothing can be
    /// placed against it.
    NoViewport,
    /// The rack entity lost its operational state.
    MissingRack,
    /// The rack has no spawned badge node to write to.
    MissingBadgeNode,
}

/// One rack's badge presentation this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HudBadge {
    /// Stable rack index.
    pub rack: usize,
    /// Which badge it shows, if any.
    pub kind: Option<BadgeKind>,
    /// Why it is or is not drawn.
    pub visibility: BadgeVisibility,
    /// The stable world point it is anchored to.
    pub anchor_world: Vec3,
    /// Where that point landed on the viewport, when it projected.
    pub anchor: Option<Vec2>,
    /// Where the badge centre ended up, when it is drawn.
    pub center: Option<Vec2>,
}

/// A presentation failure the HUD refused to hide.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HudError {
    /// No game camera exists, so nothing can be projected.
    NoCamera,
    /// The camera has no viewport size yet.
    NoViewport,
    /// A rack on the roster lost its [`RackOperations`].
    MissingRackState {
        /// The offending rack index.
        rack: usize,
    },
    /// A rack on the roster has no spawned badge node.
    MissingBadgeNode {
        /// The offending rack index.
        rack: usize,
    },
    /// A queue slot has no spawned row node.
    MissingRow {
        /// The offending slot.
        slot: usize,
    },
    /// The real projection refused a rack anchor.
    ProjectionFailed {
        /// The offending rack index.
        rack: usize,
        /// Exactly why the projection refused it.
        reason: ProjectionFailure,
    },
    /// An active ticket names a rack the HUD cannot find.
    TicketWithoutRack {
        /// The ticket.
        ticket: TicketId,
        /// The rack it names.
        rack: usize,
    },
}

/// Why [`Camera::world_to_viewport`] refused an anchor, as a typed value the
/// report can carry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProjectionFailure {
    /// The camera had no viewport size.
    NoViewportSize,
    /// The anchor was behind the near plane.
    PastNearPlane,
    /// The anchor was beyond the far plane.
    PastFarPlane,
    /// The projection matrix could not be inverted.
    InvalidData,
}

impl From<ViewportConversionError> for ProjectionFailure {
    fn from(error: ViewportConversionError) -> Self {
        match error {
            ViewportConversionError::NoViewportSize => Self::NoViewportSize,
            ViewportConversionError::PastNearPlane => Self::PastNearPlane,
            ViewportConversionError::PastFarPlane => Self::PastFarPlane,
            ViewportConversionError::InvalidData => Self::InvalidData,
        }
    }
}

/// Everything the HUD drew this frame, and everything it could not draw.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct HudReport {
    /// The viewport the HUD was laid out against.
    pub viewport: Vec2,
    /// The rendered queue rows, in priority order.
    pub rows: Vec<HudRow>,
    /// Every rack's badge presentation, in stable rack order.
    pub badges: Vec<HudBadge>,
    /// The status line.
    pub status: HudStatus,
    /// Whether movement is locked by a running repair.
    pub movement_locked: bool,
    /// Every presentation failure, in detection order.
    pub errors: Vec<HudError>,
}

impl HudReport {
    /// Whether the HUD drew everything it was asked to.
    pub fn is_healthy(&self) -> bool {
        self.errors.is_empty()
    }

    /// The badge presentation for one rack.
    pub fn badge(&self, rack: usize) -> Option<&HudBadge> {
        self.badges.iter().find(|badge| badge.rack == rack)
    }

    /// Every drawn badge kind, in stable rack order.
    pub fn shown_badges(&self) -> Vec<(usize, BadgeKind)> {
        self.badges
            .iter()
            .filter(|badge| badge.visibility == BadgeVisibility::Shown)
            .filter_map(|badge| badge.kind.map(|kind| (badge.rack, kind)))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Pure model
// ---------------------------------------------------------------------------

/// What the HUD needs to know about one rack, read straight off its live
/// [`RackOperations`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RackPresentation {
    /// Stable rack index.
    pub rack: usize,
    /// Current state.
    pub state: RackState,
    /// How far through the current timed state the rack is, in `0.0..=1.0`.
    pub progress: f32,
}

impl RackPresentation {
    /// Reads one rack's presentation from its live operational state.
    pub fn read(operations: &RackOperations) -> Self {
        Self {
            rack: operations.rack,
            state: operations.state(),
            progress: dwell_progress(operations.state(), operations.elapsed()),
        }
    }
}

/// How far through its dwell time a rack in this state is. Untimed states are
/// always zero, and the value never leaves `0.0..=1.0`.
pub fn dwell_progress(state: RackState, elapsed: Duration) -> f32 {
    let Some(dwell) = state.dwell() else {
        return 0.0;
    };
    if dwell.is_zero() {
        return 1.0;
    }
    (elapsed.as_secs_f32() / dwell.as_secs_f32()).clamp(0.0, 1.0)
}

/// The label one queue row renders. Racks are named by their authored
/// `rack-row-NN` number, which is the stable rack index plus one.
pub fn row_label(ticket: TicketId, rack: usize, severity: TicketSeverity) -> String {
    format!("{ticket} R{:02} {}", rack + 1, severity.label())
}

/// Builds the ordered queue rows from the live queue and the live rack states.
///
/// The queue is already in global priority order and is capped at
/// [`MAX_ACTIVE_TICKETS`], so this never re-sorts and never copies ticket
/// state; it only joins each ticket to the rack that owns it. A ticket whose
/// rack the HUD cannot find is reported rather than silently drawn wrong.
pub fn queue_rows(queue: &TicketQueue, racks: &[RackPresentation]) -> (Vec<HudRow>, Vec<HudError>) {
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for ticket in queue.ordered().iter().take(MAX_ACTIVE_TICKETS) {
        let Some(rack) = racks.iter().find(|rack| rack.rack == ticket.rack) else {
            errors.push(HudError::TicketWithoutRack {
                ticket: ticket.id,
                rack: ticket.rack,
            });
            continue;
        };
        rows.push(HudRow {
            slot: rows.len(),
            ticket: ticket.id,
            rack: ticket.rack,
            severity: ticket.severity,
            state: rack.state,
            progress: rack.progress,
            label: row_label(ticket.id, ticket.rack, ticket.severity),
        });
    }
    (rows, errors)
}

/// Where one badge and its leader line sit on the viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BadgePlacement {
    /// The projected rack anchor, in logical px.
    pub anchor: Vec2,
    /// The badge centre, in logical px.
    pub center: Vec2,
    /// The leader line's centre, when one is drawn.
    pub leader_center: Option<Vec2>,
    /// The leader line's length in logical px, when one is drawn.
    pub leader_length: f32,
    /// The clockwise screen rotation that turns the leader line's local `+Y`
    /// axis onto the direction of the anchor.
    pub leader_rotation: Rot2,
}

/// Distance from the centre of an axis-aligned rectangle to its edge along a
/// unit direction. A degenerate direction returns the smaller half extent.
pub fn rect_exit_distance(half: Vec2, direction: Vec2) -> f32 {
    let horizontal = if direction.x.abs() > f32::EPSILON {
        half.x / direction.x.abs()
    } else {
        f32::INFINITY
    };
    let vertical = if direction.y.abs() > f32::EPSILON {
        half.y / direction.y.abs()
    } else {
        f32::INFINITY
    };
    let exit = horizontal.min(vertical);
    if exit.is_finite() {
        exit
    } else {
        half.min_element()
    }
}

/// Places one badge from its projected anchor.
///
/// Returns `None` when the anchor itself is off screen, which is the only
/// reason a projected badge is ever hidden. Whenever the anchor is visible the
/// badge box is clamped fully inside the viewport, so a visible rack always has
/// a visible badge however close to the edge it is.
pub fn place_badge(anchor: Vec2, viewport: Vec2) -> Option<BadgePlacement> {
    if !(0.0..=viewport.x).contains(&anchor.x) || !(0.0..=viewport.y).contains(&anchor.y) {
        return None;
    }
    let half = badge_half_extents();
    let limit = half + Vec2::splat(BADGE_EDGE_MARGIN);
    let center = Vec2::new(
        anchor.x.clamp(limit.x, (viewport.x - limit.x).max(limit.x)),
        (anchor.y - BADGE_LIFT).clamp(limit.y, (viewport.y - limit.y).max(limit.y)),
    );

    let delta = anchor - center;
    let distance = delta.length();
    if distance <= f32::EPSILON {
        return Some(BadgePlacement {
            anchor,
            center,
            leader_center: None,
            leader_length: 0.0,
            leader_rotation: Rot2::IDENTITY,
        });
    }
    let direction = delta / distance;
    let start = rect_exit_distance(half, direction) + LEADER_GAP;
    let length = distance - start;
    let rotation = Rot2::from_sin_cos(-direction.x, direction.y);
    if length <= 0.0 {
        return Some(BadgePlacement {
            anchor,
            center,
            leader_center: None,
            leader_length: 0.0,
            leader_rotation: rotation,
        });
    }
    Some(BadgePlacement {
        anchor,
        center,
        leader_center: Some(center + direction * (start + length * 0.5)),
        leader_length: length,
        leader_rotation: rotation,
    })
}

/// The stable world point one rack's badge is anchored to.
pub fn badge_anchor_world(center: Vec2) -> Vec3 {
    Vec3::new(center.x, RACK_BADGE_ANCHOR_HEIGHT, center.y)
}

// ---------------------------------------------------------------------------
// Presentation components
// ---------------------------------------------------------------------------

/// The full-viewport node every HUD element hangs from.
#[derive(Component, Clone, Copy, Debug)]
pub struct HudRoot;

/// The top-left prioritized queue and status stack.
#[derive(Component, Clone, Copy, Debug)]
pub struct TicketQueuePanel;

/// The queue stack's header line.
#[derive(Component, Clone, Copy, Debug)]
pub struct QueueHeaderLabel;

/// One of the [`MAX_ACTIVE_TICKETS`] queue rows, by slot.
#[derive(Component, Clone, Copy, Debug)]
pub struct QueueRowNode {
    /// Which slot this row occupies, top to bottom.
    pub slot: usize,
}

/// The severity chip that leads one queue row.
#[derive(Component, Clone, Copy, Debug)]
pub struct QueueRowSeverityChip {
    /// Which slot this chip belongs to.
    pub slot: usize,
}

/// The rack-state chip inside one queue row.
#[derive(Component, Clone, Copy, Debug)]
pub struct QueueRowStateChip {
    /// Which slot this chip belongs to.
    pub slot: usize,
}

/// The label inside one queue row.
#[derive(Component, Clone, Copy, Debug)]
pub struct QueueRowLabel {
    /// Which slot this label belongs to.
    pub slot: usize,
}

/// The timed-state progress bar under one queue row.
#[derive(Component, Clone, Copy, Debug)]
pub struct QueueRowProgress {
    /// Which slot this bar belongs to.
    pub slot: usize,
}

/// The status chip at the bottom of the queue stack.
#[derive(Component, Clone, Copy, Debug)]
pub struct HudStatusChip;

/// The status label at the bottom of the queue stack.
#[derive(Component, Clone, Copy, Debug)]
pub struct HudStatusLabel;

/// The compact bottom-right control strip.
#[derive(Component, Clone, Copy, Debug)]
pub struct ControlsPanel;

/// One key cap in the control strip.
#[derive(Component, Clone, Copy, Debug)]
pub struct ControlHintCap {
    /// Which control this cap describes.
    pub control: HudControl,
}

/// The text inside one key cap.
#[derive(Component, Clone, Copy, Debug)]
pub struct ControlHintCapLabel {
    /// Which control this label describes.
    pub control: HudControl,
}

/// One rack's floating badge.
#[derive(Component, Clone, Copy, Debug)]
pub struct RackBadgeNode {
    /// Stable rack index.
    pub rack: usize,
}

/// The glyph inside one rack badge.
#[derive(Component, Clone, Copy, Debug)]
pub struct RackBadgeLabel {
    /// Stable rack index.
    pub rack: usize,
}

/// One rack badge's thin leader line.
#[derive(Component, Clone, Copy, Debug)]
pub struct RackLeaderLine {
    /// Stable rack index.
    pub rack: usize,
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Draws the operations HUD from live operations state, and nothing else.
pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HudReport>()
            .add_systems(Startup, spawn_hud)
            .add_systems(
                Update,
                spawn_rack_badges
                    .in_set(CellShiftSet::SpawnWorld)
                    .run_if(operations_are_ready),
            )
            .add_systems(Update, update_hud.in_set(CellShiftSet::UpdateHudAndBadges));
    }
}

fn panel_node(padding: f32) -> Node {
    Node {
        position_type: PositionType::Absolute,
        flex_direction: FlexDirection::Column,
        padding: UiRect::all(Val::Px(padding)),
        row_gap: Val::Px(QUEUE_ROW_GAP),
        ..default()
    }
}

fn label_font(size: f32) -> TextFont {
    TextFont::from_font_size(size)
}

fn spawn_hud(mut commands: Commands) {
    commands
        .spawn((
            HudRoot,
            Name::new("hud-root"),
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
        ))
        .with_children(|root| {
            spawn_queue_panel(root);
            spawn_controls_panel(root);
        });
}

fn spawn_queue_panel(root: &mut RelatedSpawnerCommands<'_, ChildOf>) {
    root.spawn((
        TicketQueuePanel,
        Name::new("hud-queue-panel"),
        Node {
            left: Val::Px(HUD_MARGIN),
            top: Val::Px(HUD_MARGIN),
            width: Val::Px(QUEUE_PANEL_WIDTH),
            border_radius: BorderRadius::all(Val::Px(4.0)),
            ..panel_node(HUD_PANEL_PADDING)
        },
        BackgroundColor(hud_panel_color()),
    ))
    .with_children(|panel| {
        panel.spawn((
            QueueHeaderLabel,
            Name::new("hud-queue-header"),
            Text::new(queue_header_label(0)),
            label_font(HUD_SMALL_FONT_SIZE),
            TextColor(hud_color(PaletteRole::RackShadow)),
        ));
        for slot in 0..MAX_ACTIVE_TICKETS {
            spawn_queue_row(panel, slot);
        }
        panel
            .spawn((
                Name::new("hud-status-line"),
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Px(QUEUE_ROW_HEIGHT),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(6.0),
                    ..default()
                },
            ))
            .with_children(|status| {
                status.spawn((
                    HudStatusChip,
                    Name::new("hud-status-chip"),
                    Node {
                        width: Val::Px(QUEUE_CHIP_SIZE),
                        height: Val::Px(QUEUE_CHIP_SIZE),
                        border_radius: BorderRadius::all(Val::Px(QUEUE_CHIP_SIZE * 0.5)),
                        ..default()
                    },
                    BackgroundColor(hud_color(HudStatus::default().role())),
                ));
                status.spawn((
                    HudStatusLabel,
                    Name::new("hud-status-label"),
                    Text::new(HudStatus::default().label()),
                    label_font(HUD_SMALL_FONT_SIZE),
                    TextColor(hud_color(HudStatus::default().role())),
                ));
            });
    });
}

fn spawn_queue_row(panel: &mut RelatedSpawnerCommands<'_, ChildOf>, slot: usize) {
    panel
        .spawn((
            QueueRowNode { slot },
            Name::new(format!("hud-queue-row-{slot}")),
            Node {
                display: Display::None,
                width: Val::Percent(100.0),
                height: Val::Px(QUEUE_ROW_HEIGHT),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.0),
                ..default()
            },
        ))
        .with_children(|row| {
            row.spawn((
                QueueRowSeverityChip { slot },
                Name::new(format!("hud-queue-severity-{slot}")),
                Node {
                    width: Val::Px(QUEUE_CHIP_SIZE),
                    height: Val::Px(QUEUE_CHIP_SIZE),
                    border_radius: BorderRadius::all(Val::Px(0.0)),
                    ..default()
                },
                BackgroundColor(hud_color(severity_role(TicketSeverity::Critical))),
            ));
            row.spawn((
                QueueRowStateChip { slot },
                Name::new(format!("hud-queue-state-{slot}")),
                Node {
                    width: Val::Px(4.0),
                    height: Val::Px(QUEUE_CHIP_SIZE),
                    ..default()
                },
                BackgroundColor(hud_color(state_role(RackState::Faulted))),
            ));
            row.spawn((
                QueueRowLabel { slot },
                Name::new(format!("hud-queue-label-{slot}")),
                Text::new(String::new()),
                label_font(QUEUE_LABEL_FONT_SIZE),
                TextColor(hud_color(PaletteRole::RackWhite)),
            ));
            row.spawn((
                QueueRowProgress { slot },
                Name::new(format!("hud-queue-progress-{slot}")),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    bottom: Val::Px(0.0),
                    width: Val::Percent(0.0),
                    height: Val::Px(QUEUE_PROGRESS_HEIGHT),
                    ..default()
                },
                BackgroundColor(hud_color(state_role(RackState::Repairing))),
            ));
        });
}

fn spawn_controls_panel(root: &mut RelatedSpawnerCommands<'_, ChildOf>) {
    root.spawn((
        ControlsPanel,
        Name::new("hud-controls-panel"),
        Node {
            right: Val::Px(HUD_MARGIN),
            bottom: Val::Px(HUD_MARGIN),
            height: Val::Px(CONTROLS_PANEL_HEIGHT),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(10.0),
            border_radius: BorderRadius::all(Val::Px(4.0)),
            ..panel_node(HUD_PANEL_PADDING)
        },
        BackgroundColor(hud_panel_color()),
    ))
    .with_children(|panel| {
        for control in HudControl::ALL {
            panel
                .spawn((
                    Name::new(format!("hud-control-{}", control.action())),
                    Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(5.0),
                        ..default()
                    },
                ))
                .with_children(|hint| {
                    hint.spawn((
                        ControlHintCap { control },
                        Name::new(format!("hud-control-cap-{}", control.keys())),
                        Node {
                            padding: UiRect::axes(Val::Px(4.0), Val::Px(2.0)),
                            border: UiRect::all(Val::Px(1.0)),
                            border_radius: BorderRadius::all(Val::Px(3.0)),
                            ..default()
                        },
                        BackgroundColor(hud_color(control.cap_role(false))),
                        BorderColor::all(hud_color(PaletteRole::Ink)),
                    ))
                    .with_child((
                        ControlHintCapLabel { control },
                        Text::new(control.keys()),
                        label_font(HUD_SMALL_FONT_SIZE),
                        TextColor(hud_color(control.cap_text_role(false))),
                    ));
                    hint.spawn((
                        Text::new(control.action()),
                        label_font(HUD_SMALL_FONT_SIZE),
                        TextColor(hud_color(PaletteRole::RackWhite)),
                    ));
                });
        }
    });
}

/// The queue header, which always names how many of the reviewed maximum are
/// active.
pub fn queue_header_label(active: usize) -> String {
    format!("OPS QUEUE {active}/{MAX_ACTIVE_TICKETS}")
}

/// Spawns one badge and one leader line per authored rack, once, as soon as
/// the operations roster exists.
fn spawn_rack_badges(
    mut commands: Commands,
    roster: Res<RackRoster>,
    roots: Query<Entity, With<HudRoot>>,
    badges: Query<&RackBadgeNode>,
) {
    if !badges.is_empty() {
        return;
    }
    let Ok(root) = roots.single() else {
        return;
    };
    commands.entity(root).with_children(|parent| {
        for entry in roster.all() {
            let rack = entry.rack;
            parent.spawn((
                RackLeaderLine { rack },
                Name::new(format!("hud-rack-leader-{rack}")),
                Node {
                    position_type: PositionType::Absolute,
                    display: Display::None,
                    width: Val::Px(LEADER_WIDTH),
                    height: Val::Px(0.0),
                    ..default()
                },
                BackgroundColor(hud_color(PaletteRole::Ink)),
                ZIndex(0),
            ));
            parent
                .spawn((
                    RackBadgeNode { rack },
                    Name::new(format!("hud-rack-badge-{rack}")),
                    Node {
                        position_type: PositionType::Absolute,
                        display: Display::None,
                        width: Val::Px(BADGE_WIDTH),
                        height: Val::Px(BADGE_HEIGHT),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        border: UiRect::all(Val::Px(2.0)),
                        border_radius: BorderRadius::all(Val::Px(BadgeKind::Fault.corner_radius())),
                        ..default()
                    },
                    BackgroundColor(hud_color(BadgeKind::Fault.role())),
                    BorderColor::all(hud_color(PaletteRole::Ink)),
                    ZIndex(1),
                ))
                .with_child((
                    RackBadgeLabel { rack },
                    Text::new(BadgeKind::Fault.label()),
                    label_font(HUD_SMALL_FONT_SIZE),
                    TextColor(hud_color(BadgeKind::Fault.text_role())),
                ));
        }
    });
}

/// Everything the badge pass needs from the one game camera.
struct BadgeCamera {
    camera: Camera,
    transform: GlobalTransform,
    viewport: Vec2,
}

/// What the badge pass could get from the one game camera this frame.
enum BadgeView {
    /// A usable, unparented camera with a real viewport size.
    ///
    /// [`Camera`] is a large component and this enum is built every frame, so
    /// the usable case is boxed and the two failure cases stay pointer sized.
    Ready(Box<BadgeCamera>),
    /// No usable game camera exists: either none is spawned, or the one that
    /// is has been given a parent and so no longer satisfies the current-frame
    /// projection invariant.
    NoCamera,
    /// A usable camera exists but has no viewport size yet.
    NoViewport,
}

/// What the badge pass reads off the one game camera.
type GameCamera = (&'static Camera, &'static Transform);

/// The one game camera the badge pass will accept.
///
/// `Without<ChildOf>` is load bearing, not decoration: see [`update_hud`].
type GameCameraFilter = (With<CellShiftCamera>, Without<ChildOf>);

/// Reads the live operations model, writes only presentation components, and
/// records everything it could not draw in [`HudReport`].
#[allow(clippy::too_many_arguments)]
fn update_hud(
    queue: Res<TicketQueue>,
    roster: Res<RackRoster>,
    lock: Res<MovementLock>,
    last: Res<LastInteraction>,
    racks: Query<&RackOperations>,
    players: Query<&Transform, With<Technician>>,
    cameras: Query<GameCamera, GameCameraFilter>,
    mut report: ResMut<HudReport>,
    mut nodes: HudNodes,
) {
    let mut errors = Vec::new();

    // Reading the camera's own `Transform` rather than its propagated
    // `GlobalTransform` is deliberate: propagation runs in `PostUpdate`, so the
    // propagated value is a frame stale and badges would visibly lag the
    // camera through every orbit tween. That substitution is only sound for a
    // camera with no parent, so `Without<ChildOf>` makes it an enforced
    // invariant rather than a comment: a parented camera is refused as
    // unusable and every badge is hidden, instead of being projected through a
    // local transform pretending to be a global one.
    let view = match cameras.iter().next() {
        Some((camera, transform)) => match camera.logical_viewport_size() {
            Some(viewport) => BadgeView::Ready(Box::new(BadgeCamera {
                camera: camera.clone(),
                transform: GlobalTransform::from(*transform),
                viewport,
            })),
            None => {
                errors.push(HudError::NoViewport);
                BadgeView::NoViewport
            }
        },
        None => {
            errors.push(HudError::NoCamera);
            BadgeView::NoCamera
        }
    };

    let presentations = roster
        .all()
        .iter()
        .map(|entry| match racks.get(entry.entity) {
            Ok(operations) => RackPresentation::read(operations),
            Err(_) => {
                errors.push(HudError::MissingRackState { rack: entry.rack });
                RackPresentation {
                    rack: entry.rack,
                    state: RackState::Healthy,
                    progress: 0.0,
                }
            }
        })
        .collect::<Vec<_>>();
    let missing_state = errors
        .iter()
        .filter_map(|error| match error {
            HudError::MissingRackState { rack } => Some(*rack),
            _ => None,
        })
        .collect::<Vec<_>>();

    let technician = players
        .iter()
        .next()
        .map(|transform| Vec2::new(transform.translation.x, transform.translation.z));

    let (rows, row_errors) = queue_rows(&queue, &presentations);
    errors.extend(row_errors);
    let status = HudStatus::derive(&lock, &last, &queue, &roster, &presentations, technician);

    errors.extend(nodes.write_queue(&rows, status, queue.len()));
    nodes.write_controls(lock.is_locked());

    let mut badges = Vec::with_capacity(presentations.len());
    for (entry, presentation) in roster.all().iter().zip(&presentations) {
        let rack = entry.rack;
        let anchor_world = badge_anchor_world(entry.center);
        let kind = if missing_state.contains(&rack) {
            None
        } else {
            BadgeKind::for_state(presentation.state)
        };

        let mut badge = HudBadge {
            rack,
            kind,
            visibility: BadgeVisibility::NoTicket,
            anchor_world,
            anchor: None,
            center: None,
        };
        if missing_state.contains(&rack) {
            badge.visibility = BadgeVisibility::MissingRack;
        } else if let Some(kind) = kind {
            match &view {
                BadgeView::NoCamera => badge.visibility = BadgeVisibility::NoCamera,
                BadgeView::NoViewport => badge.visibility = BadgeVisibility::NoViewport,
                BadgeView::Ready(view) => {
                    match view.camera.world_to_viewport(&view.transform, anchor_world) {
                        Err(error) => {
                            badge.visibility = BadgeVisibility::ProjectionFailed;
                            errors.push(HudError::ProjectionFailed {
                                rack,
                                reason: error.into(),
                            });
                        }
                        Ok(anchor) => {
                            badge.anchor = Some(anchor);
                            match place_badge(anchor, view.viewport) {
                                None => badge.visibility = BadgeVisibility::OffScreen,
                                Some(placement) => {
                                    badge.visibility = BadgeVisibility::Shown;
                                    badge.center = Some(placement.center);
                                    if let Some(error) = nodes.write_badge(rack, kind, &placement) {
                                        errors.push(error);
                                        badge.visibility = BadgeVisibility::MissingBadgeNode;
                                        badge.center = None;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // A rack whose badge node is missing was already reported once by
        // `write_badge`, which also hid its leader line. Running the hide path
        // as well would record the same failure a second time.
        if !matches!(
            badge.visibility,
            BadgeVisibility::Shown | BadgeVisibility::MissingBadgeNode
        ) && let Some(error) = nodes.hide_badge(rack)
        {
            errors.push(error);
        }
        badges.push(badge);
    }

    let updated = HudReport {
        viewport: match &view {
            BadgeView::Ready(view) => view.viewport,
            BadgeView::NoCamera | BadgeView::NoViewport => Vec2::ZERO,
        },
        rows,
        badges,
        status,
        movement_locked: lock.is_locked(),
        errors,
    };
    if *report != updated {
        *report = updated;
    }
}

/// Every presentation node the HUD writes.
///
/// The marker queries read only their own marker component and the style query
/// writes only presentation components, so the two sets are disjoint and Bevy
/// can schedule them in one system without a `ParamSet`.
/// Every presentation component the HUD writes, as one query item.
type HudStyle = (
    &'static mut Node,
    &'static mut BackgroundColor,
    &'static mut UiTransform,
    Option<&'static mut Text>,
    Option<&'static mut TextColor>,
);

#[derive(SystemParam)]
struct HudNodes<'w, 's> {
    header: Query<'w, 's, Entity, With<QueueHeaderLabel>>,
    rows: Query<'w, 's, (Entity, &'static QueueRowNode)>,
    severity_chips: Query<'w, 's, (Entity, &'static QueueRowSeverityChip)>,
    state_chips: Query<'w, 's, (Entity, &'static QueueRowStateChip)>,
    row_labels: Query<'w, 's, (Entity, &'static QueueRowLabel)>,
    row_progress: Query<'w, 's, (Entity, &'static QueueRowProgress)>,
    status_chip: Query<'w, 's, Entity, With<HudStatusChip>>,
    status_label: Query<'w, 's, Entity, With<HudStatusLabel>>,
    caps: Query<'w, 's, (Entity, &'static ControlHintCap)>,
    cap_labels: Query<'w, 's, (Entity, &'static ControlHintCapLabel)>,
    badges: Query<'w, 's, (Entity, &'static RackBadgeNode)>,
    badge_labels: Query<'w, 's, (Entity, &'static RackBadgeLabel)>,
    leaders: Query<'w, 's, (Entity, &'static RackLeaderLine)>,
    style: Query<'w, 's, HudStyle>,
}

impl HudNodes<'_, '_> {
    fn row_entity(&self, slot: usize) -> Option<Entity> {
        self.rows
            .iter()
            .find(|(_, row)| row.slot == slot)
            .map(|(entity, _)| entity)
    }

    fn severity_entity(&self, slot: usize) -> Option<Entity> {
        self.severity_chips
            .iter()
            .find(|(_, chip)| chip.slot == slot)
            .map(|(entity, _)| entity)
    }

    fn state_entity(&self, slot: usize) -> Option<Entity> {
        self.state_chips
            .iter()
            .find(|(_, chip)| chip.slot == slot)
            .map(|(entity, _)| entity)
    }

    fn row_label_entity(&self, slot: usize) -> Option<Entity> {
        self.row_labels
            .iter()
            .find(|(_, label)| label.slot == slot)
            .map(|(entity, _)| entity)
    }

    fn progress_entity(&self, slot: usize) -> Option<Entity> {
        self.row_progress
            .iter()
            .find(|(_, bar)| bar.slot == slot)
            .map(|(entity, _)| entity)
    }

    fn badge_entity(&self, rack: usize) -> Option<Entity> {
        self.badges
            .iter()
            .find(|(_, badge)| badge.rack == rack)
            .map(|(entity, _)| entity)
    }

    fn badge_label_entity(&self, rack: usize) -> Option<Entity> {
        self.badge_labels
            .iter()
            .find(|(_, label)| label.rack == rack)
            .map(|(entity, _)| entity)
    }

    fn leader_entity(&self, rack: usize) -> Option<Entity> {
        self.leaders
            .iter()
            .find(|(_, leader)| leader.rack == rack)
            .map(|(entity, _)| entity)
    }

    fn set_display(&mut self, entity: Entity, display: Display) {
        let Ok((mut node, ..)) = self.style.get_mut(entity) else {
            return;
        };
        if node.display != display {
            node.display = display;
        }
    }

    fn set_background(&mut self, entity: Entity, color: Color) {
        let Ok((_, mut background, ..)) = self.style.get_mut(entity) else {
            return;
        };
        if background.0 != color {
            background.0 = color;
        }
    }

    fn set_corner_radius(&mut self, entity: Entity, radius: f32) {
        let Ok((mut node, ..)) = self.style.get_mut(entity) else {
            return;
        };
        let updated = BorderRadius::all(Val::Px(radius));
        if node.border_radius != updated {
            node.border_radius = updated;
        }
    }

    fn set_width_percent(&mut self, entity: Entity, percent: f32) {
        let Ok((mut node, ..)) = self.style.get_mut(entity) else {
            return;
        };
        let width = Val::Percent(percent);
        if node.width != width {
            node.width = width;
        }
    }

    fn set_label(&mut self, entity: Entity, value: &str, color: Option<Color>) {
        let Ok((_, _, _, text, text_color)) = self.style.get_mut(entity) else {
            return;
        };
        if let Some(mut text) = text
            && text.0 != value
        {
            text.0 = value.to_owned();
        }
        if let (Some(mut current), Some(color)) = (text_color, color) {
            let updated = TextColor(color);
            if *current != updated {
                *current = updated;
            }
        }
    }

    /// Moves one absolutely positioned node so its top-left corner lands on
    /// `top_left`.
    fn place(&mut self, entity: Entity, top_left: Vec2) {
        let Ok((mut node, ..)) = self.style.get_mut(entity) else {
            return;
        };
        let left = Val::Px(top_left.x);
        let top = Val::Px(top_left.y);
        if node.left != left {
            node.left = left;
        }
        if node.top != top {
            node.top = top;
        }
    }

    fn set_leader(&mut self, entity: Entity, center: Vec2, length: f32, rotation: Rot2) {
        let Ok((mut node, _, mut transform, ..)) = self.style.get_mut(entity) else {
            return;
        };
        let height = Val::Px(length);
        if node.height != height {
            node.height = height;
        }
        let top_left = center - Vec2::new(LEADER_WIDTH, length) * 0.5;
        let left = Val::Px(top_left.x);
        let top = Val::Px(top_left.y);
        if node.left != left {
            node.left = left;
        }
        if node.top != top {
            node.top = top;
        }
        if transform.rotation != rotation {
            transform.rotation = rotation;
        }
        if node.display != Display::Flex {
            node.display = Display::Flex;
        }
    }

    /// Writes the queue stack, returning every row slot it could not find.
    fn write_queue(&mut self, rows: &[HudRow], status: HudStatus, active: usize) -> Vec<HudError> {
        let mut errors = Vec::new();
        if let Some(header) = self.header.iter().next() {
            self.set_label(header, &queue_header_label(active), None);
        }

        for slot in 0..MAX_ACTIVE_TICKETS {
            let row = rows.iter().find(|row| row.slot == slot);
            let Some(entity) = self.row_entity(slot) else {
                errors.push(HudError::MissingRow { slot });
                continue;
            };
            self.set_display(
                entity,
                if row.is_some() {
                    Display::Flex
                } else {
                    Display::None
                },
            );
            let Some(row) = row else {
                continue;
            };

            if let Some(chip) = self.severity_entity(slot) {
                self.set_background(chip, hud_color(severity_role(row.severity)));
                let corner = match row.severity {
                    TicketSeverity::Critical => 0.0,
                    TicketSeverity::Warning => QUEUE_CHIP_SIZE * 0.5,
                };
                self.set_corner_radius(chip, corner);
            }
            if let Some(chip) = self.state_entity(slot) {
                self.set_background(chip, hud_color(state_role(row.state)));
            }
            if let Some(label) = self.row_label_entity(slot) {
                self.set_label(label, &row.label, None);
            }
            if let Some(bar) = self.progress_entity(slot) {
                self.set_width_percent(bar, row.progress * 100.0);
                self.set_background(bar, hud_color(state_role(row.state)));
            }
        }

        if let Some(chip) = self.status_chip.iter().next() {
            self.set_background(chip, hud_color(status.role()));
        }
        if let Some(label) = self.status_label.iter().next() {
            self.set_label(label, status.label(), Some(hud_color(status.role())));
        }
        errors
    }

    /// Writes the control strip, which says which keys are live right now.
    fn write_controls(&mut self, locked: bool) {
        let caps = self
            .caps
            .iter()
            .map(|(entity, cap)| (entity, cap.control))
            .collect::<Vec<_>>();
        for (entity, control) in caps {
            self.set_background(entity, hud_color(control.cap_role(locked)));
        }
        let labels = self
            .cap_labels
            .iter()
            .map(|(entity, label)| (entity, label.control))
            .collect::<Vec<_>>();
        for (entity, control) in labels {
            self.set_label(
                entity,
                control.keys(),
                Some(hud_color(control.cap_text_role(locked))),
            );
        }
    }

    /// Places one badge and its leader line, returning an error when the rack
    /// has no spawned badge node.
    ///
    /// A rack with no badge node still gets its leader line hidden here, so
    /// the caller never has to run the hide path, and never reports the same
    /// missing node twice.
    fn write_badge(
        &mut self,
        rack: usize,
        kind: BadgeKind,
        placement: &BadgePlacement,
    ) -> Option<HudError> {
        let Some(entity) = self.badge_entity(rack) else {
            if let Some(leader) = self.leader_entity(rack) {
                self.set_display(leader, Display::None);
            }
            return Some(HudError::MissingBadgeNode { rack });
        };
        self.set_display(entity, Display::Flex);
        self.place(entity, placement.center - badge_half_extents());
        self.set_corner_radius(entity, kind.corner_radius());
        self.set_background(entity, hud_color(kind.role()));

        if let Some(label) = self.badge_label_entity(rack) {
            self.set_label(label, kind.label(), Some(hud_color(kind.text_role())));
        }

        if let Some(leader) = self.leader_entity(rack) {
            match placement.leader_center {
                Some(center) => self.set_leader(
                    leader,
                    center,
                    placement.leader_length,
                    placement.leader_rotation,
                ),
                None => self.set_display(leader, Display::None),
            }
        }
        None
    }

    /// Hides one rack's badge and leader line, returning an error when the
    /// rack has no spawned badge node.
    fn hide_badge(&mut self, rack: usize) -> Option<HudError> {
        let badge = self.badge_entity(rack);
        if let Some(leader) = self.leader_entity(rack) {
            self.set_display(leader, Display::None);
        }
        match badge {
            Some(entity) => {
                self.set_display(entity, Display::None);
                None
            }
            None => Some(HudError::MissingBadgeNode { rack }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        design::PropId,
        operations::{RESOLVED_DISPLAY, Ticket},
    };

    fn ticket(id: u64, rack: usize, severity: TicketSeverity, created_tick: u64) -> Ticket {
        Ticket {
            id: TicketId::new(id),
            rack,
            rack_id: PropId::new(format!("rack-row-{:02}", rack + 1)),
            severity,
            created_tick,
        }
    }

    /// One roster entry whose collider is a metre wide and centred six metres
    /// apart from its neighbours, so a technician's distance to it is easy to
    /// place either side of [`REPAIR_INTERACTION_RANGE`].
    fn entry(rack: usize) -> RackEntry {
        RackEntry {
            rack,
            id: PropId::new(format!("rack-row-{:02}", rack + 1)),
            entity: Entity::PLACEHOLDER,
            center: Vec2::new(rack as f32 * 6.0, 0.0),
            half_extents: Vec2::new(0.5, 8.0),
        }
    }

    fn roster_of(racks: usize) -> RackRoster {
        RackRoster::from_entries((0..racks).map(entry).collect())
    }

    /// A technician standing `distance` metres clear of one rack's collider.
    fn standing(rack: usize, distance: f32) -> Option<Vec2> {
        let entry = entry(rack);
        Some(Vec2::new(
            entry.center.x + entry.half_extents.x + distance,
            0.0,
        ))
    }

    fn rejected(rack: Option<usize>) -> LastInteraction {
        LastInteraction {
            outcome: InteractionOutcome::OutOfRange {
                nearest_rack: rack,
                nearest_distance: 2.2,
            },
            ..LastInteraction::default()
        }
    }

    fn presentation(rack: usize, state: RackState) -> RackPresentation {
        RackPresentation {
            rack,
            state,
            progress: 0.0,
        }
    }

    fn queue_of(tickets: Vec<Ticket>) -> TicketQueue {
        let mut queue = TicketQueue::default();
        for ticket in tickets {
            queue.insert(ticket).expect("the fixture queue accepts it");
        }
        queue
    }

    #[test]
    fn hud_badge_kind_covers_every_rack_state_exactly_once() {
        assert_eq!(BadgeKind::for_state(RackState::Healthy), None);
        assert_eq!(BadgeKind::for_state(RackState::Cooldown), None);
        assert_eq!(
            BadgeKind::for_state(RackState::Faulted),
            Some(BadgeKind::Fault)
        );
        assert_eq!(
            BadgeKind::for_state(RackState::Repairing),
            Some(BadgeKind::Repairing)
        );
        assert_eq!(
            BadgeKind::for_state(RackState::Resolved),
            Some(BadgeKind::Resolved)
        );

        // The badge predicates on the operations state machine and the HUD
        // must never drift apart.
        for state in RackState::ALL {
            assert_eq!(
                BadgeKind::for_state(state) == Some(BadgeKind::Fault),
                state.shows_fault_badge(),
                "{state:?} disagrees about the red fault badge"
            );
            assert_eq!(
                BadgeKind::for_state(state) == Some(BadgeKind::Repairing),
                state.shows_wrench_badge(),
                "{state:?} disagrees about the blue repair badge"
            );
            assert_eq!(
                BadgeKind::for_state(state) == Some(BadgeKind::Resolved),
                state.shows_healthy_badge(),
                "{state:?} disagrees about the healthy badge"
            );
        }
    }

    #[test]
    fn hud_badge_colors_and_shapes_are_typed_and_distinct() {
        assert_eq!(BadgeKind::Fault.role(), PaletteRole::FaultRed);
        assert_eq!(BadgeKind::Repairing.role(), PaletteRole::WorkerHardHat);
        assert_eq!(BadgeKind::Resolved.role(), PaletteRole::HealthyGreen);

        let colors = BadgeKind::ALL.map(|kind| kind.role().color());
        assert_eq!(colors[0], crate::design::FAULT_RED);
        assert_eq!(colors[2], crate::design::HEALTHY_GREEN);
        for (left, right) in [(0, 1), (0, 2), (1, 2)] {
            assert_ne!(
                colors[left], colors[right],
                "badges must not share a colour"
            );
            assert_ne!(
                BadgeKind::ALL[left].corner_radius(),
                BadgeKind::ALL[right].corner_radius(),
                "badges must not share a shape"
            );
            assert_ne!(
                BadgeKind::ALL[left].label(),
                BadgeKind::ALL[right].label(),
                "badges must not share a glyph"
            );
        }
        // The repair badge really is blue: more blue than red or green.
        let blue = BadgeKind::Repairing.role().color();
        assert!(blue.blue > blue.red && blue.blue > blue.green, "{blue:?}");
    }

    #[test]
    fn hud_severity_and_state_roles_are_typed_palette_entries() {
        assert_eq!(
            severity_role(TicketSeverity::Critical),
            PaletteRole::FaultRed
        );
        assert_eq!(
            severity_role(TicketSeverity::Warning),
            PaletteRole::SignatureYellow
        );
        assert_eq!(state_role(RackState::Faulted), PaletteRole::FaultRed);
        assert_eq!(state_role(RackState::Repairing), PaletteRole::WorkerHardHat);
        assert_eq!(state_role(RackState::Resolved), PaletteRole::HealthyGreen);
        assert_eq!(state_role(RackState::Healthy), PaletteRole::RackShadow);
        assert_eq!(state_role(RackState::Cooldown), PaletteRole::RackShadow);
        for role in RackState::ALL.map(state_role) {
            assert!(PaletteRole::ALL.contains(&role));
        }
    }

    #[test]
    fn hud_queue_rows_follow_the_queue_priority_order_and_cap() {
        let queue = queue_of(vec![
            ticket(1, 2, TicketSeverity::Warning, 240),
            ticket(2, 0, TicketSeverity::Critical, 480),
            ticket(3, 3, TicketSeverity::Warning, 120),
        ]);
        let racks = [
            presentation(0, RackState::Repairing),
            presentation(1, RackState::Healthy),
            presentation(2, RackState::Faulted),
            presentation(3, RackState::Faulted),
        ];

        let (rows, errors) = queue_rows(&queue, &racks);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter()
                .map(|row| (row.slot, row.ticket.value(), row.rack))
                .collect::<Vec<_>>(),
            vec![(0, 2, 0), (1, 3, 3), (2, 1, 2)],
            "critical first, then creation tick, then rack"
        );
        assert_eq!(
            rows.iter().map(|row| row.state).collect::<Vec<_>>(),
            vec![RackState::Repairing, RackState::Faulted, RackState::Faulted],
            "each row reads the live rack it belongs to"
        );
        assert_eq!(rows[0].label, "T0002 R01 Critical");
        assert_eq!(rows[1].label, "T0003 R04 Warning");
        assert_eq!(
            rows.iter().map(|row| row.slot).collect::<Vec<_>>(),
            (0..rows.len()).collect::<Vec<_>>(),
            "slots are dense and ordered"
        );
    }

    #[test]
    fn hud_queue_rows_are_empty_when_nothing_is_open() {
        let (rows, errors) = queue_rows(
            &TicketQueue::default(),
            &[presentation(0, RackState::Healthy)],
        );
        assert!(rows.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn hud_queue_rows_report_a_ticket_whose_rack_is_missing() {
        let queue = queue_of(vec![
            ticket(1, 0, TicketSeverity::Critical, 10),
            ticket(2, 7, TicketSeverity::Critical, 20),
        ]);
        let (rows, errors) = queue_rows(&queue, &[presentation(0, RackState::Faulted)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rack, 0);
        assert_eq!(
            errors,
            vec![HudError::TicketWithoutRack {
                ticket: TicketId::new(2),
                rack: 7
            }],
            "a row the HUD cannot draw is reported, never silently dropped"
        );
    }

    #[test]
    fn hud_dwell_progress_is_clamped_and_zero_for_untimed_states() {
        assert_eq!(
            dwell_progress(RackState::Healthy, Duration::from_secs(9)),
            0.0
        );
        assert_eq!(
            dwell_progress(RackState::Faulted, Duration::from_secs(9)),
            0.0
        );
        assert_eq!(dwell_progress(RackState::Resolved, Duration::ZERO), 0.0);
        assert!(
            (dwell_progress(RackState::Resolved, RESOLVED_DISPLAY / 2) - 0.5).abs() < 1e-6,
            "half a resolved display is half the bar"
        );
        assert_eq!(
            dwell_progress(RackState::Resolved, RESOLVED_DISPLAY * 4),
            1.0,
            "a hitch never overfills the bar"
        );
    }

    #[test]
    fn hud_status_prefers_the_running_repair_then_the_real_rejection() {
        let idle = LastInteraction::default();
        let empty = TicketQueue::default();
        let busy = queue_of(vec![ticket(1, 1, TicketSeverity::Critical, 10)]);
        let roster = roster_of(4);
        let faulted = [
            presentation(0, RackState::Healthy),
            presentation(1, RackState::Faulted),
            presentation(2, RackState::Healthy),
            presentation(3, RackState::Healthy),
        ];
        let far = standing(1, 3.0);

        assert_eq!(
            HudStatus::derive(
                &MovementLock::default(),
                &idle,
                &empty,
                &roster,
                &faulted,
                far
            ),
            HudStatus::AllHealthy
        );
        assert_eq!(
            HudStatus::derive(
                &MovementLock::default(),
                &idle,
                &busy,
                &roster,
                &faulted,
                far
            ),
            HudStatus::TicketsOpen
        );
        assert_eq!(
            HudStatus::derive(
                &MovementLock::default(),
                &rejected(Some(1)),
                &busy,
                &roster,
                &faulted,
                far
            ),
            HudStatus::MoveCloser
        );

        let nothing_open = LastInteraction {
            outcome: InteractionOutcome::NoOpenTickets,
            ..LastInteraction::default()
        };
        assert_eq!(
            HudStatus::derive(
                &MovementLock::default(),
                &nothing_open,
                &empty,
                &roster,
                &faulted,
                far
            ),
            HudStatus::NoOpenTickets
        );

        for status in HudStatus::ALL {
            assert!(PaletteRole::ALL.contains(&status.role()));
            assert!(!status.label().is_empty());
        }
    }

    #[test]
    fn hud_move_closer_holds_only_while_that_one_rack_is_open_and_unreachable() {
        let roster = roster_of(4);
        let faulted = [
            presentation(0, RackState::Faulted),
            presentation(1, RackState::Faulted),
            presentation(2, RackState::Healthy),
            presentation(3, RackState::Healthy),
        ];
        let busy = queue_of(vec![
            ticket(1, 0, TicketSeverity::Critical, 10),
            ticket(2, 1, TicketSeverity::Critical, 20),
        ]);

        assert!(
            move_closer_still_stands(1, &busy, roster.get(1), &faulted, standing(1, 3.0)),
            "an open fault the technician is still too far from keeps the rejection true"
        );
        assert!(
            !move_closer_still_stands(
                1,
                &busy,
                roster.get(1),
                &faulted,
                standing(1, REPAIR_INTERACTION_RANGE - 0.01)
            ),
            "walking into range clears the rejection, even with the ticket still open"
        );
        assert!(
            move_closer_still_stands(
                1,
                &busy,
                roster.get(1),
                &faulted,
                standing(1, REPAIR_INTERACTION_RANGE + 0.01)
            ),
            "the boundary is the real repair range, not the queue"
        );
        assert!(
            !move_closer_still_stands(1, &busy, roster.get(1), &faulted, None),
            "no technician in the world means no standing rejection"
        );
        assert!(
            !move_closer_still_stands(1, &busy, None, &faulted, standing(1, 3.0)),
            "a rack that is not on the roster cannot be walked up to"
        );

        // The rack the rejection was about resolves, while another rack's
        // ticket is still open. The old code kept saying "move closer".
        for state in [
            RackState::Repairing,
            RackState::Resolved,
            RackState::Cooldown,
            RackState::Healthy,
        ] {
            let moved_on = [
                presentation(0, RackState::Faulted),
                presentation(1, state),
                presentation(2, RackState::Healthy),
                presentation(3, RackState::Healthy),
            ];
            assert!(
                !move_closer_still_stands(1, &busy, roster.get(1), &moved_on, standing(1, 3.0)),
                "a {state:?} rack is not something to move closer to"
            );
        }

        // The rack's ticket leaves the queue while rack 0's ticket stays.
        let only_other = queue_of(vec![ticket(1, 0, TicketSeverity::Critical, 10)]);
        assert!(
            !move_closer_still_stands(1, &only_other, roster.get(1), &faulted, standing(1, 3.0)),
            "another rack's ticket must never keep a stale rejection alive"
        );
    }

    #[test]
    fn hud_status_clears_a_stale_rejection_through_every_transition() {
        let roster = roster_of(4);
        let unlocked = MovementLock::default();
        let far = standing(1, 3.0);
        let faulted = [
            presentation(0, RackState::Faulted),
            presentation(1, RackState::Faulted),
            presentation(2, RackState::Healthy),
            presentation(3, RackState::Healthy),
        ];
        let two_open = queue_of(vec![
            ticket(1, 0, TicketSeverity::Critical, 10),
            ticket(2, 1, TicketSeverity::Critical, 20),
        ]);
        let derive = |queue: &TicketQueue, racks: &[RackPresentation], at: Option<Vec2>| {
            HudStatus::derive(&unlocked, &rejected(Some(1)), queue, &roster, racks, at)
        };

        assert_eq!(derive(&two_open, &faulted, far), HudStatus::MoveCloser);

        // Rack 1 starts repairing: the rejection is over, and rack 0's ticket
        // is what the queue now reports.
        let repairing = [
            presentation(0, RackState::Faulted),
            presentation(1, RackState::Repairing),
            presentation(2, RackState::Healthy),
            presentation(3, RackState::Healthy),
        ];
        assert_eq!(derive(&two_open, &repairing, far), HudStatus::TicketsOpen);

        // Rack 1's ticket is removed but rack 0's is still open. The status
        // must not fall back to a rejection that is no longer about anything.
        let only_rack_zero = queue_of(vec![ticket(1, 0, TicketSeverity::Critical, 10)]);
        assert_eq!(
            derive(&only_rack_zero, &faulted, far),
            HudStatus::TicketsOpen,
            "a surviving unrelated ticket must not resurrect the rejection"
        );

        // Every ticket is gone.
        assert_eq!(
            derive(
                &TicketQueue::default(),
                &[
                    presentation(0, RackState::Cooldown),
                    presentation(1, RackState::Cooldown),
                    presentation(2, RackState::Healthy),
                    presentation(3, RackState::Healthy),
                ],
                far
            ),
            HudStatus::AllHealthy,
            "a stale rejection never outlives the ticket it was about"
        );

        // The technician walks in without pressing anything.
        assert_eq!(
            derive(&two_open, &faulted, standing(1, 0.5)),
            HudStatus::TicketsOpen,
            "walking into range clears the prompt that told you to"
        );

        // A rejection that never named a rack cannot stand at all.
        assert_eq!(
            HudStatus::derive(
                &unlocked,
                &rejected(None),
                &two_open,
                &roster,
                &faulted,
                far
            ),
            HudStatus::TicketsOpen
        );
    }

    #[test]
    fn hud_control_hints_name_every_reviewed_key() {
        assert_eq!(
            HudControl::ALL.map(HudControl::keys),
            ["Arrows", "Q", "E", "Space"]
        );
        assert_eq!(
            HudControl::ALL.map(HudControl::action),
            ["Move", "Turn L", "Turn R", "Repair"]
        );
        assert_eq!(
            HudControl::Repair.cap_role(true),
            PaletteRole::WorkerHardHat,
            "the running repair is the key that is doing something"
        );
        assert_eq!(
            HudControl::Move.cap_role(true),
            PaletteRole::RackShadow,
            "movement is locked during a repair, and the hint says so"
        );
        for control in HudControl::ALL {
            assert_eq!(control.cap_role(false), PaletteRole::RackWhite);
            assert!(PaletteRole::ALL.contains(&control.cap_role(true)));
        }
    }

    #[test]
    fn hud_rect_exit_distance_matches_the_badge_box() {
        let half = badge_half_extents();
        assert_eq!(rect_exit_distance(half, Vec2::Y), half.y);
        assert_eq!(rect_exit_distance(half, Vec2::NEG_Y), half.y);
        assert_eq!(rect_exit_distance(half, Vec2::X), half.x);
        assert_eq!(rect_exit_distance(half, Vec2::ZERO), half.min_element());
        let diagonal = Vec2::splat(1.0).normalize();
        let exit = rect_exit_distance(half, diagonal);
        let point = diagonal * exit;
        assert!(
            (point.x.abs() - half.x).abs() < 1e-4 || (point.y.abs() - half.y).abs() < 1e-4,
            "the exit point must land on the box edge, got {point:?}"
        );
        assert!(point.x.abs() <= half.x + 1e-4 && point.y.abs() <= half.y + 1e-4);
    }

    #[test]
    fn hud_badge_placement_lifts_the_badge_and_points_the_leader_at_the_anchor() {
        let viewport = Vec2::new(1280.0, 720.0);
        let anchor = Vec2::new(640.0, 400.0);
        let placement = place_badge(anchor, viewport).expect("a centred anchor is on screen");

        assert_eq!(placement.anchor, anchor);
        assert_eq!(placement.center, Vec2::new(640.0, 400.0 - BADGE_LIFT));
        let center = placement
            .leader_center
            .expect("a lifted badge has a leader");
        assert!((center.x - anchor.x).abs() < 1e-4);
        assert!(
            (placement.leader_length - (BADGE_LIFT - BADGE_HEIGHT * 0.5 - LEADER_GAP)).abs() < 1e-4,
            "got {}",
            placement.leader_length
        );
        let tip = center + placement.leader_rotation * Vec2::Y * (placement.leader_length * 0.5);
        assert!(
            tip.distance(anchor) < 1e-3,
            "the leader must end on the anchor, got {tip:?}"
        );
    }

    #[test]
    fn hud_badge_placement_clamps_a_visible_anchor_fully_on_screen() {
        let viewport = Vec2::new(960.0, 540.0);
        let half = badge_half_extents();
        for anchor in [
            Vec2::new(0.0, 0.0),
            Vec2::new(960.0, 0.0),
            Vec2::new(0.0, 540.0),
            Vec2::new(960.0, 540.0),
            Vec2::new(2.0, 6.0),
            Vec2::new(958.0, 534.0),
        ] {
            let placement =
                place_badge(anchor, viewport).unwrap_or_else(|| panic!("{anchor:?} is on screen"));
            let min = placement.center - half;
            let max = placement.center + half;
            assert!(
                min.x >= 0.0 && min.y >= 0.0 && max.x <= viewport.x && max.y <= viewport.y,
                "badge for {anchor:?} left the viewport: {min:?}..{max:?}"
            );
            if let Some(leader) = placement.leader_center {
                let tip =
                    leader + placement.leader_rotation * Vec2::Y * (placement.leader_length * 0.5);
                assert!(
                    tip.distance(anchor) < 1e-3,
                    "the clamped leader must still end on {anchor:?}, got {tip:?}"
                );
            }
        }
    }

    #[test]
    fn hud_badge_placement_hides_an_anchor_that_is_off_screen() {
        let viewport = Vec2::new(1280.0, 720.0);
        for anchor in [
            Vec2::new(-1.0, 360.0),
            Vec2::new(1281.0, 360.0),
            Vec2::new(640.0, -1.0),
            Vec2::new(640.0, 721.0),
        ] {
            assert_eq!(
                place_badge(anchor, viewport),
                None,
                "{anchor:?} is off screen and must hide explicitly"
            );
        }
    }

    #[test]
    fn hud_badge_anchor_is_a_stable_point_above_the_rack() {
        let anchor = badge_anchor_world(Vec2::new(-9.0, 0.0));
        assert_eq!(anchor, Vec3::new(-9.0, RACK_BADGE_ANCHOR_HEIGHT, 0.0));
        // The anchor must clear the authored cabinet tops.
        const { assert!(RACK_BADGE_ANCHOR_HEIGHT > AUTHORED_RACK_TOP_HEIGHT) };
    }

    #[test]
    fn hud_fixed_panels_cannot_reach_the_central_play_rectangle() {
        for viewport in [Vec2::new(1280.0, 720.0), Vec2::new(960.0, 540.0)] {
            assert!(
                HUD_MARGIN + QUEUE_PANEL_WIDTH < viewport.x * 0.25,
                "the queue stack must stay left of the play rectangle at {viewport:?}"
            );
            assert!(
                HUD_MARGIN + CONTROLS_PANEL_HEIGHT < viewport.y * 0.25,
                "the control strip must stay below the play rectangle at {viewport:?}"
            );
        }
    }
}
