//! Recurring seeded faults, prioritized tickets, and the repair interaction.
//!
//! The rack state machine, exactly as the reviewed plan documents it:
//!
//! ```text
//! Healthy
//!    |
//!    | seeded scheduler; capacity available
//!    v
//! Faulted + TicketOpen
//!    |
//!    | player in range + Space just_pressed
//!    v
//! Repairing (3 s, movement locked, blue wrench badge)
//!    |
//!    | timer complete
//!    v
//! Resolved (2 s, healthy indicator)
//!    |
//!    | display timer complete
//!    v
//! Cooldown (8 s, no active ticket)
//!    |
//!    | cooldown complete
//!    v
//! Healthy and eligible again
//! ```
//!
//! The scheduler is a pure sequence generator. It draws `(rack, severity)`
//! pairs from one seeded ChaCha8 stream and emits them **in order**: an
//! opportunity that cannot be satisfied pauses rather than skipping ahead, so
//! when the player repairs never changes which fault comes next, only when it
//! arrives.
//!
//! ```text
//! every FAULT_INTERVAL of simulated time
//!            |
//!            v
//!         armed (the timer stops accumulating until it fires)
//!            |
//!            +-- queue at MAX_ACTIVE_TICKETS --> ScheduleBlock::AtCapacity
//!            |                                   (no RNG is consumed)
//!            v
//!      draw the next (rack, severity) from the seeded stream
//!            |
//!            +-- that rack already holds a ticket --> ScheduleBlock::DuplicateRack
//!            +-- that rack is not Healthy ----------> ScheduleBlock::RackBusy
//!            |      (the drawn candidate is held, never discarded)
//!            v
//!      emit Ticket { stable monotonic id, rack, severity, created_tick }
//! ```

use std::{fmt, time::Duration};

use bevy::prelude::*;
use rand_chacha::{
    ChaCha8Rng,
    rand_core::{Rng as _, SeedableRng},
};

use crate::{
    CellShiftSet,
    design::{AssetKind, MAX_ACTIVE_TICKETS, PropId, REPAIR_INTERACTION_RANGE},
    player::Technician,
    world::{HallColliders, HallProp, HallState},
};

/// Seed of the recurring fault stream, fixed by controller ruling.
pub const FAULT_SCHEDULER_SEED: u64 = 0xCE11_5A1F_DA7A_CE01;

/// Simulated time between two fault opportunities.
pub const FAULT_INTERVAL: Duration = Duration::from_millis(4_000);

/// How long one repair holds the technician still.
pub const REPAIR_DURATION: Duration = Duration::from_millis(3_000);

/// How long a repaired rack shows its healthy indicator before its ticket is
/// removed.
pub const RESOLVED_DISPLAY: Duration = Duration::from_millis(2_000);

/// How long a repaired rack stays ineligible for a new fault.
pub const RACK_COOLDOWN: Duration = Duration::from_millis(8_000);

/// The authored asset kind operational state is attached to.
pub const RACK_ASSET_KIND: AssetKind = AssetKind::RackRow;

/// The real key that starts a repair.
pub const REPAIR_KEY: KeyCode = KeyCode::Space;

// ---------------------------------------------------------------------------
// Rack state machine
// ---------------------------------------------------------------------------

/// Where one rack sits in the documented state machine above.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum RackState {
    /// No fault, and eligible for the next scheduled one.
    #[default]
    Healthy,
    /// A fault is open and its ticket is active.
    Faulted,
    /// The technician is repairing this rack; movement is locked.
    Repairing,
    /// Repaired, showing the healthy indicator; the ticket is still active.
    Resolved,
    /// Ticket removed, still ineligible for a new fault.
    Cooldown,
}

impl RackState {
    /// Every state, in documented order.
    pub const ALL: [Self; 5] = [
        Self::Healthy,
        Self::Faulted,
        Self::Repairing,
        Self::Resolved,
        Self::Cooldown,
    ];

    /// How long this state lasts before it advances on its own.
    pub const fn dwell(self) -> Option<Duration> {
        match self {
            Self::Repairing => Some(REPAIR_DURATION),
            Self::Resolved => Some(RESOLVED_DISPLAY),
            Self::Cooldown => Some(RACK_COOLDOWN),
            Self::Healthy | Self::Faulted => None,
        }
    }

    /// The state this one advances to when its dwell time completes.
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Repairing => Some(Self::Resolved),
            Self::Resolved => Some(Self::Cooldown),
            Self::Cooldown => Some(Self::Healthy),
            Self::Healthy | Self::Faulted => None,
        }
    }

    /// Whether the scheduler may open a new fault on a rack in this state.
    pub const fn is_eligible_for_fault(self) -> bool {
        matches!(self, Self::Healthy)
    }

    /// Whether a rack in this state carries an active ticket.
    pub const fn holds_ticket(self) -> bool {
        matches!(self, Self::Faulted | Self::Repairing | Self::Resolved)
    }

    /// Whether Task 7 draws the red fault badge over this rack.
    pub const fn shows_fault_badge(self) -> bool {
        matches!(self, Self::Faulted)
    }

    /// Whether Task 7 draws the blue wrench badge over this rack.
    pub const fn shows_wrench_badge(self) -> bool {
        matches!(self, Self::Repairing)
    }

    /// Whether Task 7 draws the healthy indicator over this rack.
    pub const fn shows_healthy_badge(self) -> bool {
        matches!(self, Self::Resolved)
    }

    /// Whether a rack in this state holds the technician still.
    pub const fn locks_movement(self) -> bool {
        matches!(self, Self::Repairing)
    }
}

/// One documented transition a rack completed on its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RackTransition {
    /// The repair timer completed; the rack now shows the healthy indicator.
    Resolved(TicketId),
    /// The resolved display completed; the ticket leaves the active queue and
    /// the rack begins its cooldown.
    TicketRemoved(TicketId),
    /// The cooldown completed; the rack is eligible for a new fault again.
    Recovered,
}

/// Operational state attached to one authored rack `HallProp`, joined to the
/// blueprint by its stable [`PropId`].
#[derive(Component, Clone, Debug, PartialEq)]
pub struct RackOperations {
    /// Stable rack index, assigned from sorted [`PropId`] order.
    pub rack: usize,
    /// The authored prop identifier this state belongs to.
    pub id: PropId,
    state: RackState,
    elapsed: Duration,
    ticket: Option<TicketId>,
}

impl RackOperations {
    /// A healthy rack with no ticket.
    pub fn new(rack: usize, id: PropId) -> Self {
        Self {
            rack,
            id,
            state: RackState::Healthy,
            elapsed: Duration::ZERO,
            ticket: None,
        }
    }

    /// Current state.
    pub fn state(&self) -> RackState {
        self.state
    }

    /// Time spent in the current state.
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Time left before the current state advances on its own.
    pub fn remaining(&self) -> Option<Duration> {
        self.state
            .dwell()
            .map(|dwell| dwell.saturating_sub(self.elapsed))
    }

    /// The active ticket this rack holds, if any.
    pub fn ticket(&self) -> Option<TicketId> {
        self.ticket
    }

    /// Opens a scheduled fault. Only a healthy rack accepts one.
    pub fn open_fault(&mut self, ticket: TicketId) -> bool {
        if !self.state.is_eligible_for_fault() {
            return false;
        }
        self.state = RackState::Faulted;
        self.elapsed = Duration::ZERO;
        self.ticket = Some(ticket);
        true
    }

    /// Starts a repair. Only an open fault accepts one.
    pub fn begin_repair(&mut self) -> Option<TicketId> {
        if self.state != RackState::Faulted {
            return None;
        }
        let ticket = self.ticket?;
        self.state = RackState::Repairing;
        self.elapsed = Duration::ZERO;
        Some(ticket)
    }

    /// Advances the timed states, returning every transition the elapsed time
    /// completed, in order.
    ///
    /// A hitch longer than one dwell walks the documented states one at a time
    /// and carries its remainder, so no state is ever skipped.
    pub fn advance(&mut self, delta: Duration) -> Vec<RackTransition> {
        let mut transitions = Vec::new();
        let Some(mut dwell) = self.state.dwell() else {
            return transitions;
        };
        self.elapsed += delta;
        while self.elapsed >= dwell {
            self.elapsed -= dwell;
            let ticket = self.ticket;
            let next = self
                .state
                .next()
                .expect("a state with a dwell time always names its successor");
            transitions.push(match self.state {
                RackState::Repairing => RackTransition::Resolved(
                    ticket.expect("a repairing rack always holds its ticket"),
                ),
                RackState::Resolved => {
                    self.ticket = None;
                    RackTransition::TicketRemoved(
                        ticket.expect("a resolved rack always holds its ticket"),
                    )
                }
                RackState::Cooldown => RackTransition::Recovered,
                RackState::Healthy | RackState::Faulted => {
                    unreachable!("untimed states have no dwell")
                }
            });
            self.state = next;
            match next.dwell() {
                Some(next_dwell) => dwell = next_dwell,
                None => {
                    self.elapsed = Duration::ZERO;
                    break;
                }
            }
        }
        transitions
    }
}

// ---------------------------------------------------------------------------
// Tickets
// ---------------------------------------------------------------------------

/// How urgent one open fault is. Declaration order is priority order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TicketSeverity {
    /// Sorted ahead of every warning.
    Critical,
    /// Sorted behind every critical fault.
    Warning,
}

impl TicketSeverity {
    /// Every severity, in priority order.
    pub const ALL: [Self; 2] = [Self::Critical, Self::Warning];

    /// Short stable label for reports and the Task 7 HUD.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Critical => "Critical",
            Self::Warning => "Warning",
        }
    }
}

/// Stable, monotonic ticket identifier. Numbers are never reused.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TicketId(u64);

impl TicketId {
    /// One identifier by value. The scheduler is the only thing that mints
    /// them at runtime; this exists so reports and contracts can name one.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw monotonic value.
    pub const fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TicketId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "T{:04}", self.0)
    }
}

/// One active ticket. Every field is a creation fact and never changes; the
/// live state lives on the rack's [`RackOperations`], so there is exactly one
/// source of truth for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ticket {
    /// Stable monotonic identifier.
    pub id: TicketId,
    /// Stable rack index.
    pub rack: usize,
    /// The authored rack prop identifier.
    pub rack_id: PropId,
    /// How urgent this fault is.
    pub severity: TicketSeverity,
    /// Simulation tick the scheduler emitted this ticket on.
    pub created_tick: u64,
}

/// Global queue order: Critical before Warning, then creation tick, then rack.
pub fn ticket_priority(ticket: &Ticket) -> (TicketSeverity, u64, usize) {
    (ticket.severity, ticket.created_tick, ticket.rack)
}

/// Why the queue refused a ticket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TicketRejection {
    /// The queue already holds [`MAX_ACTIVE_TICKETS`] tickets.
    AtCapacity {
        /// How many tickets were already active.
        active: usize,
    },
    /// That rack already holds an active ticket.
    DuplicateRack {
        /// The offending rack index.
        rack: usize,
        /// The ticket that rack already holds.
        existing: TicketId,
    },
}

/// Every active ticket, kept in global priority order.
#[derive(Resource, Clone, Debug, Default, Eq, PartialEq)]
pub struct TicketQueue {
    tickets: Vec<Ticket>,
}

impl TicketQueue {
    /// Every active ticket, in global priority order.
    pub fn ordered(&self) -> &[Ticket] {
        &self.tickets
    }

    /// How many tickets are active.
    pub fn len(&self) -> usize {
        self.tickets.len()
    }

    /// Whether no ticket is active.
    pub fn is_empty(&self) -> bool {
        self.tickets.is_empty()
    }

    /// Whether the queue is holding the maximum number of active tickets.
    pub fn is_at_capacity(&self) -> bool {
        self.tickets.len() >= MAX_ACTIVE_TICKETS
    }

    /// One active ticket by identifier.
    pub fn get(&self, id: TicketId) -> Option<&Ticket> {
        self.tickets.iter().find(|ticket| ticket.id == id)
    }

    /// The active ticket for one rack, if it has one.
    pub fn for_rack(&self, rack: usize) -> Option<&Ticket> {
        self.tickets.iter().find(|ticket| ticket.rack == rack)
    }

    /// Whether one rack already holds an active ticket.
    pub fn contains_rack(&self, rack: usize) -> bool {
        self.for_rack(rack).is_some()
    }

    /// Inserts one ticket in priority order, enforcing capacity and the
    /// one-ticket-per-rack invariant.
    pub fn insert(&mut self, ticket: Ticket) -> Result<(), TicketRejection> {
        if let Some(existing) = self.for_rack(ticket.rack) {
            return Err(TicketRejection::DuplicateRack {
                rack: ticket.rack,
                existing: existing.id,
            });
        }
        if self.is_at_capacity() {
            return Err(TicketRejection::AtCapacity {
                active: self.tickets.len(),
            });
        }
        let at = self
            .tickets
            .partition_point(|held| ticket_priority(held) < ticket_priority(&ticket));
        self.tickets.insert(at, ticket);
        Ok(())
    }

    /// Removes one ticket by identifier.
    pub fn remove(&mut self, id: TicketId) -> Option<Ticket> {
        let at = self.tickets.iter().position(|ticket| ticket.id == id)?;
        Some(self.tickets.remove(at))
    }
}

// ---------------------------------------------------------------------------
// Seeded scheduler
// ---------------------------------------------------------------------------

/// The seeded fault stream, wrapped so the number of consumed draws is
/// observable. Capacity pauses must not consume the stream.
#[derive(Clone, Debug)]
pub struct SeededFaultRng {
    inner: ChaCha8Rng,
    seed: u64,
    draws: u64,
}

impl SeededFaultRng {
    /// A stream from one seed.
    pub fn new(seed: u64) -> Self {
        Self {
            inner: ChaCha8Rng::seed_from_u64(seed),
            seed,
            draws: 0,
        }
    }

    /// The seed this stream was built from.
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// How many words have been drawn from the stream so far.
    pub const fn draws(&self) -> u64 {
        self.draws
    }

    fn next_u32(&mut self) -> u32 {
        self.draws += 1;
        self.inner.next_u32()
    }
}

impl Default for SeededFaultRng {
    fn default() -> Self {
        Self::new(FAULT_SCHEDULER_SEED)
    }
}

/// One drawn but not yet emitted fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FaultCandidate {
    /// Stable rack index the stream chose.
    pub rack: usize,
    /// Severity the stream chose.
    pub severity: TicketSeverity,
}

/// Why a matured fault opportunity has not emitted yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScheduleBlock {
    /// The queue is full, so nothing is drawn at all.
    AtCapacity {
        /// How many tickets are active.
        active: usize,
    },
    /// The drawn rack already holds an active ticket.
    DuplicateRack {
        /// The drawn rack index.
        rack: usize,
        /// The ticket that rack already holds.
        existing: TicketId,
    },
    /// The drawn rack has no ticket but is not eligible yet.
    RackBusy {
        /// The drawn rack index.
        rack: usize,
        /// The state that rack is in.
        state: RackState,
    },
    /// The hall authored no racks at all, so nothing can ever fault.
    NoRacks,
    /// The roster cannot answer for the drawn rack. The candidate is held
    /// rather than discarded, so a roster that comes back does not reroll the
    /// seeded stream.
    UnknownRack {
        /// The drawn rack index.
        rack: usize,
        /// How many racks the roster offered.
        roster: usize,
    },
}

/// Everything the scheduler needs to know about one rack this tick.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RackSnapshot {
    /// Stable rack index.
    pub rack: usize,
    /// The authored rack prop identifier.
    pub id: PropId,
    /// Current state.
    pub state: RackState,
    /// The active ticket this rack holds, if any.
    pub ticket: Option<TicketId>,
}

/// The deterministic recurring fault scheduler.
#[derive(Resource, Clone, Debug)]
pub struct FaultScheduler {
    rng: SeededFaultRng,
    racks: usize,
    elapsed: Duration,
    armed: bool,
    pending: Option<FaultCandidate>,
    next_ticket: u64,
    emitted: u64,
    blocked: Option<ScheduleBlock>,
    capacity_pauses: u64,
    duplicate_pauses: u64,
    busy_pauses: u64,
}

impl Default for FaultScheduler {
    fn default() -> Self {
        Self::new(0)
    }
}

impl FaultScheduler {
    /// A scheduler over `racks` racks, seeded with [`FAULT_SCHEDULER_SEED`].
    pub fn new(racks: usize) -> Self {
        Self::with_seed(FAULT_SCHEDULER_SEED, racks)
    }

    /// A scheduler over `racks` racks, seeded explicitly.
    pub fn with_seed(seed: u64, racks: usize) -> Self {
        Self {
            rng: SeededFaultRng::new(seed),
            racks,
            elapsed: Duration::ZERO,
            armed: false,
            pending: None,
            next_ticket: 1,
            emitted: 0,
            blocked: None,
            capacity_pauses: 0,
            duplicate_pauses: 0,
            busy_pauses: 0,
        }
    }

    /// How many racks the stream draws from.
    pub const fn racks(&self) -> usize {
        self.racks
    }

    /// The seeded stream, for draw accounting.
    pub const fn rng(&self) -> &SeededFaultRng {
        &self.rng
    }

    /// Time accumulated toward the next fault opportunity.
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Whether a fault opportunity has matured and is waiting to emit.
    pub const fn is_armed(&self) -> bool {
        self.armed
    }

    /// The drawn but not yet emitted fault, if any.
    pub const fn pending(&self) -> Option<FaultCandidate> {
        self.pending
    }

    /// Why the matured opportunity has not emitted yet.
    pub const fn blocked(&self) -> Option<ScheduleBlock> {
        self.blocked
    }

    /// How many tickets this scheduler has emitted.
    pub const fn emitted(&self) -> u64 {
        self.emitted
    }

    /// How many times a matured opportunity paused because the queue was full.
    pub const fn capacity_pauses(&self) -> u64 {
        self.capacity_pauses
    }

    /// How many times a drawn candidate paused because its rack already held
    /// an active ticket.
    pub const fn duplicate_pauses(&self) -> u64 {
        self.duplicate_pauses
    }

    /// How many times a drawn candidate paused because its rack was not
    /// eligible yet.
    pub const fn busy_pauses(&self) -> u64 {
        self.busy_pauses
    }

    /// Advances the fault timer and emits at most one ticket.
    ///
    /// The timer stops accumulating the moment an opportunity matures, so a
    /// pause never stacks two opportunities. Nothing is drawn while the queue
    /// is full, and a drawn candidate whose rack is unavailable is held rather
    /// than discarded, so the seeded sequence is the same whatever the player
    /// does.
    pub fn step(
        &mut self,
        delta: Duration,
        tick: u64,
        active: usize,
        racks: &[RackSnapshot],
    ) -> Option<Ticket> {
        if !self.armed {
            self.elapsed += delta;
            if self.elapsed < FAULT_INTERVAL {
                self.blocked = None;
                return None;
            }
            self.elapsed -= FAULT_INTERVAL;
            self.armed = true;
        }

        if self.racks == 0 || racks.is_empty() {
            self.note_block(ScheduleBlock::NoRacks);
            return None;
        }

        if self.pending.is_none() {
            if active >= MAX_ACTIVE_TICKETS {
                self.note_block(ScheduleBlock::AtCapacity { active });
                return None;
            }
            self.pending = Some(self.draw());
        }

        let candidate = self
            .pending
            .expect("a drawn candidate is present by construction");
        let Some(snapshot) = racks.get(candidate.rack) else {
            self.note_block(ScheduleBlock::UnknownRack {
                rack: candidate.rack,
                roster: racks.len(),
            });
            return None;
        };
        if let Some(existing) = snapshot.ticket {
            self.note_block(ScheduleBlock::DuplicateRack {
                rack: candidate.rack,
                existing,
            });
            return None;
        }
        if !snapshot.state.is_eligible_for_fault() {
            self.note_block(ScheduleBlock::RackBusy {
                rack: candidate.rack,
                state: snapshot.state,
            });
            return None;
        }
        if active >= MAX_ACTIVE_TICKETS {
            self.note_block(ScheduleBlock::AtCapacity { active });
            return None;
        }

        self.pending = None;
        self.armed = false;
        self.blocked = None;
        let ticket = Ticket {
            id: TicketId(self.next_ticket),
            rack: candidate.rack,
            rack_id: snapshot.id.clone(),
            severity: candidate.severity,
            created_tick: tick,
        };
        self.next_ticket += 1;
        self.emitted += 1;
        Some(ticket)
    }

    /// Draws the next `(rack, severity)` pair from the seeded stream. Both
    /// moduli divide `2^32` exactly for the authored four racks and two
    /// severities, so the stream is consumed without rejection sampling and
    /// the draw count is exactly two words per emitted candidate.
    fn draw(&mut self) -> FaultCandidate {
        let rack = (self.rng.next_u32() % self.racks as u32) as usize;
        let severity = if self.rng.next_u32().is_multiple_of(2) {
            TicketSeverity::Critical
        } else {
            TicketSeverity::Warning
        };
        FaultCandidate { rack, severity }
    }

    /// Records why a matured opportunity paused, counting each new pause once.
    fn note_block(&mut self, block: ScheduleBlock) {
        if self.blocked == Some(block) {
            return;
        }
        match block {
            ScheduleBlock::AtCapacity { .. } => self.capacity_pauses += 1,
            ScheduleBlock::DuplicateRack { .. } => self.duplicate_pauses += 1,
            ScheduleBlock::RackBusy { .. }
            | ScheduleBlock::NoRacks
            | ScheduleBlock::UnknownRack { .. } => self.busy_pauses += 1,
        }
        self.blocked = Some(block);
    }
}

// ---------------------------------------------------------------------------
// Repair interaction
// ---------------------------------------------------------------------------

/// Distance from a point to the closest point of an axis-aligned rectangle.
pub fn rack_distance(point: Vec2, center: Vec2, half_extents: Vec2) -> f32 {
    ((point - center).abs() - half_extents)
        .max(Vec2::ZERO)
        .length()
}

/// One open ticket the technician might repair, with its measured distance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RepairCandidate {
    /// The open ticket.
    pub ticket: TicketId,
    /// Stable rack index.
    pub rack: usize,
    /// How urgent the fault is.
    pub severity: TicketSeverity,
    /// Tick the ticket was created on.
    pub created_tick: u64,
    /// Distance from the technician to the rack's collider rectangle.
    pub distance: f32,
}

impl RepairCandidate {
    /// Whether this rack is close enough to repair.
    pub fn is_in_range(&self) -> bool {
        self.distance <= REPAIR_INTERACTION_RANGE
    }
}

/// Chooses the ticket a Space press repairs: only in-range open tickets, then
/// severity, then distance, then creation tick, then rack index.
///
/// Distance is compared on its exact bits through [`f32::total_cmp`], so the
/// choice never depends on the order the candidates were gathered in.
pub fn select_repair_candidate(candidates: &[RepairCandidate]) -> Option<RepairCandidate> {
    candidates
        .iter()
        .filter(|candidate| candidate.is_in_range())
        .min_by(|left, right| {
            left.severity
                .cmp(&right.severity)
                .then_with(|| left.distance.total_cmp(&right.distance))
                .then_with(|| left.created_tick.cmp(&right.created_tick))
                .then_with(|| left.rack.cmp(&right.rack))
        })
        .copied()
}

/// What the last real Space press did. Every rejection is observable rather
/// than silent, and a rejection is never recorded as a start.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum InteractionOutcome {
    /// No Space press has been handled yet.
    #[default]
    None,
    /// A repair started this frame.
    Started {
        /// The ticket that started repairing.
        ticket: TicketId,
        /// The rack that ticket belongs to.
        rack: usize,
    },
    /// Space was pressed while a repair was already running.
    AlreadyRepairing {
        /// The ticket currently being repaired.
        ticket: TicketId,
    },
    /// Space was pressed with no open ticket anywhere.
    NoOpenTickets,
    /// Space was pressed with open tickets, but none within range.
    OutOfRange {
        /// The closest open ticket's rack, when one exists.
        nearest_rack: Option<usize>,
        /// Distance to that rack's collider rectangle.
        nearest_distance: f32,
    },
}

/// The observable result of the last real Space press.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct LastInteraction {
    /// What that press did.
    pub outcome: InteractionOutcome,
    /// The simulation tick it happened on.
    pub tick: u64,
    /// How many Space presses have been handled.
    pub presses: u64,
    /// How many of them started a repair.
    pub started: u64,
    /// How many of them were rejected.
    pub rejected: u64,
}

/// Whether movement is currently locked, and by which ticket.
#[derive(Resource, Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MovementLock {
    ticket: Option<TicketId>,
}

impl MovementLock {
    /// Whether the technician is held still.
    pub const fn is_locked(&self) -> bool {
        self.ticket.is_some()
    }

    /// The ticket holding the technician still.
    pub const fn ticket(&self) -> Option<TicketId> {
        self.ticket
    }
}

// ---------------------------------------------------------------------------
// Roster and clock
// ---------------------------------------------------------------------------

/// One authored rack, joined to its collider rectangle once at spawn time.
#[derive(Clone, Debug, PartialEq)]
pub struct RackEntry {
    /// Stable rack index.
    pub rack: usize,
    /// The authored prop identifier.
    pub id: PropId,
    /// The spawned rack entity.
    pub entity: Entity,
    /// Collider centre on the ground plane.
    pub center: Vec2,
    /// Collider half extents on the ground plane.
    pub half_extents: Vec2,
}

/// Every authored rack, in stable [`PropId`] order.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct RackRoster {
    racks: Vec<RackEntry>,
}

impl RackRoster {
    /// Every rack, in stable order.
    pub fn all(&self) -> &[RackEntry] {
        &self.racks
    }

    /// How many racks the hall authored.
    pub fn len(&self) -> usize {
        self.racks.len()
    }

    /// Whether the roster has been built yet.
    pub fn is_empty(&self) -> bool {
        self.racks.is_empty()
    }

    /// One rack by stable index.
    pub fn get(&self, rack: usize) -> Option<&RackEntry> {
        self.racks.get(rack)
    }

    /// One rack by authored identifier.
    pub fn by_id(&self, id: &str) -> Option<&RackEntry> {
        self.racks.iter().find(|entry| entry.id.as_str() == id)
    }
}

/// The simulation tick tickets record as their creation time.
#[derive(Resource, Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OperationsClock {
    tick: u64,
}

impl OperationsClock {
    /// The current tick.
    pub const fn tick(&self) -> u64 {
        self.tick
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// Runs the recurring fault scheduler, the ticket queue, and the repair
/// interaction.
pub struct OperationsPlugin;

impl Plugin for OperationsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TicketQueue>()
            .init_resource::<FaultScheduler>()
            .init_resource::<RackRoster>()
            .init_resource::<OperationsClock>()
            .init_resource::<MovementLock>()
            .init_resource::<LastInteraction>()
            .add_systems(
                Update,
                attach_rack_operations
                    .in_set(CellShiftSet::SpawnWorld)
                    .run_if(in_state(HallState::Ready)),
            )
            .add_systems(
                Update,
                (advance_racks, handle_repair_input, advance_scheduler)
                    .chain()
                    .in_set(CellShiftSet::UpdateOperations)
                    .run_if(operations_are_ready),
            );
    }
}

/// Run condition gating every operations system on a built rack roster.
pub fn operations_are_ready(roster: Res<RackRoster>) -> bool {
    !roster.is_empty()
}

/// Joins operational state onto the authored rack props, once, by stable
/// [`PropId`]. Rack indices are assigned from sorted identifier order, which is
/// also the authored declaration order.
fn attach_rack_operations(
    mut commands: Commands,
    racks: Query<(Entity, &HallProp)>,
    colliders: Option<Res<HallColliders>>,
    mut roster: ResMut<RackRoster>,
    mut scheduler: ResMut<FaultScheduler>,
) {
    if !roster.is_empty() {
        return;
    }
    let Some(colliders) = colliders else {
        return;
    };

    let mut found = racks
        .iter()
        .filter(|(_, prop)| prop.asset == RACK_ASSET_KIND)
        .map(|(entity, prop)| (prop.id.clone(), entity))
        .collect::<Vec<_>>();
    if found.is_empty() {
        return;
    }
    found.sort_by(|left, right| left.0.cmp(&right.0));

    let mut entries = Vec::with_capacity(found.len());
    for (rack, (id, entity)) in found.into_iter().enumerate() {
        let Some(collider) = colliders.get(&id) else {
            error!("rack {id} has no cached collider, so repair range is undefined");
            return;
        };
        commands
            .entity(entity)
            .insert(RackOperations::new(rack, id.clone()));
        entries.push(RackEntry {
            rack,
            id,
            entity,
            center: collider.center,
            half_extents: collider.half_extents,
        });
    }

    *scheduler = FaultScheduler::new(entries.len());
    roster.racks = entries;
}

/// Advances every timed rack state, removes resolved tickets from the queue,
/// and releases the movement lock the moment a repair completes.
fn advance_racks(
    time: Res<Time>,
    roster: Res<RackRoster>,
    mut racks: Query<&mut RackOperations>,
    mut queue: ResMut<TicketQueue>,
    mut lock: ResMut<MovementLock>,
    mut clock: ResMut<OperationsClock>,
) {
    clock.tick += 1;
    let delta = time.delta();
    for entry in roster.all() {
        let Ok(mut rack) = racks.get_mut(entry.entity) else {
            error!("rack {} lost its operational state", entry.id);
            continue;
        };
        for transition in rack.advance(delta) {
            match transition {
                RackTransition::Resolved(ticket) => {
                    if lock.ticket == Some(ticket) {
                        lock.ticket = None;
                    }
                }
                RackTransition::TicketRemoved(ticket) => {
                    queue.remove(ticket);
                }
                RackTransition::Recovered => {}
            }
        }
    }
}

/// Handles the real Space key. Every rejection is recorded as a rejection and
/// never as a start, so an out-of-range press is observable without being
/// mistaken for a repair.
#[allow(clippy::too_many_arguments)]
fn handle_repair_input(
    keys: Res<ButtonInput<KeyCode>>,
    clock: Res<OperationsClock>,
    roster: Res<RackRoster>,
    mut racks: Query<&mut RackOperations>,
    queue: Res<TicketQueue>,
    players: Query<&Transform, With<Technician>>,
    mut lock: ResMut<MovementLock>,
    mut last: ResMut<LastInteraction>,
) {
    if !keys.just_pressed(REPAIR_KEY) {
        return;
    }
    let Ok(transform) = players.single() else {
        return;
    };

    last.presses += 1;
    last.tick = clock.tick();
    if let Some(ticket) = lock.ticket {
        last.outcome = InteractionOutcome::AlreadyRepairing { ticket };
        last.rejected += 1;
        return;
    }

    let position = Vec2::new(transform.translation.x, transform.translation.z);
    let mut candidates = Vec::new();
    for entry in roster.all() {
        let Ok(rack) = racks.get(entry.entity) else {
            continue;
        };
        if rack.state() != RackState::Faulted {
            continue;
        }
        let Some(id) = rack.ticket() else {
            continue;
        };
        let Some(ticket) = queue.get(id) else {
            continue;
        };
        candidates.push(RepairCandidate {
            ticket: id,
            rack: entry.rack,
            severity: ticket.severity,
            created_tick: ticket.created_tick,
            distance: rack_distance(position, entry.center, entry.half_extents),
        });
    }

    if candidates.is_empty() {
        last.outcome = InteractionOutcome::NoOpenTickets;
        last.rejected += 1;
        return;
    }

    let Some(chosen) = select_repair_candidate(&candidates) else {
        let nearest = candidates
            .iter()
            .min_by(|left, right| left.distance.total_cmp(&right.distance))
            .expect("the candidate list is not empty here");
        last.outcome = InteractionOutcome::OutOfRange {
            nearest_rack: Some(nearest.rack),
            nearest_distance: nearest.distance,
        };
        last.rejected += 1;
        return;
    };

    let Some(entry) = roster.get(chosen.rack) else {
        error!("the selected rack {} is not on the roster", chosen.rack);
        return;
    };
    let Ok(mut rack) = racks.get_mut(entry.entity) else {
        error!("rack {} lost its operational state", entry.id);
        return;
    };
    match rack.begin_repair() {
        Some(ticket) => {
            lock.ticket = Some(ticket);
            last.outcome = InteractionOutcome::Started {
                ticket,
                rack: chosen.rack,
            };
            last.started += 1;
        }
        None => {
            error!("rack {} refused a repair it was selected for", entry.id);
            last.rejected += 1;
        }
    }
}

/// Advances the seeded scheduler and opens whatever fault it emits.
fn advance_scheduler(
    time: Res<Time>,
    clock: Res<OperationsClock>,
    roster: Res<RackRoster>,
    mut racks: Query<&mut RackOperations>,
    mut queue: ResMut<TicketQueue>,
    mut scheduler: ResMut<FaultScheduler>,
) {
    let mut snapshots = Vec::with_capacity(roster.len());
    for entry in roster.all() {
        let Ok(rack) = racks.get(entry.entity) else {
            error!(
                "rack {} lost its operational state, so the scheduler is paused",
                entry.id
            );
            return;
        };
        snapshots.push(RackSnapshot {
            rack: entry.rack,
            id: entry.id.clone(),
            state: rack.state(),
            ticket: rack.ticket(),
        });
    }

    let active = queue.len();
    let Some(ticket) = scheduler.step(time.delta(), clock.tick(), active, &snapshots) else {
        return;
    };

    let id = ticket.id;
    let rack_index = ticket.rack;
    if let Err(rejection) = queue.insert(ticket) {
        error!("the queue refused a scheduled ticket: {rejection:?}");
        return;
    }
    let Some(entry) = roster.get(rack_index) else {
        error!("the scheduler emitted a ticket for unknown rack {rack_index}");
        queue.remove(id);
        return;
    };
    let Ok(mut rack) = racks.get_mut(entry.entity) else {
        error!("rack {} lost its operational state", entry.id);
        queue.remove(id);
        return;
    };
    if !rack.open_fault(id) {
        error!("rack {} refused the fault it was scheduled for", entry.id);
        queue.remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::{
        FAULT_INTERVAL_SECONDS, RACK_COOLDOWN_SECONDS, REPAIR_DURATION_SECONDS,
        RESOLVED_DISPLAY_SECONDS, SceneBlueprint,
    };

    /// The fixed verification step: one sixtieth of a second, nanosecond
    /// quantized exactly as Bevy's manual clock quantizes it.
    fn step() -> Duration {
        Duration::from_secs_f64(1.0 / 60.0)
    }

    fn ticket(id: u64, rack: usize, severity: TicketSeverity, created_tick: u64) -> Ticket {
        Ticket {
            id: TicketId(id),
            rack,
            rack_id: PropId::new(format!("rack-row-{:02}", rack + 1)),
            severity,
            created_tick,
        }
    }

    fn snapshots(states: [(RackState, Option<u64>); 4]) -> Vec<RackSnapshot> {
        states
            .into_iter()
            .enumerate()
            .map(|(rack, (state, ticket))| RackSnapshot {
                rack,
                id: PropId::new(format!("rack-row-{:02}", rack + 1)),
                state,
                ticket: ticket.map(TicketId),
            })
            .collect()
    }

    fn healthy() -> Vec<RackSnapshot> {
        snapshots([(RackState::Healthy, None); 4])
    }

    /// Runs the scheduler alone, with every rack instantly healthy again, and
    /// records the `(rack, severity)` pairs it emits.
    fn undisturbed_sequence(count: usize) -> Vec<(usize, TicketSeverity)> {
        let mut scheduler = FaultScheduler::new(4);
        let mut emitted = Vec::new();
        let mut tick = 0u64;
        while emitted.len() < count {
            tick += 1;
            if let Some(ticket) = scheduler.step(step(), tick, 0, &healthy()) {
                emitted.push((ticket.rack, ticket.severity));
            }
            assert!(tick < 1_000_000, "the scheduler stalled");
        }
        emitted
    }

    /// The pinned first draws of the reviewed seed, derived independently from
    /// ChaCha8 word order: rack is `next_u32() % 4`, severity is
    /// `next_u32() % 2` with zero meaning Critical.
    const SEEDED_SEQUENCE: [(usize, TicketSeverity); 12] = [
        (2, TicketSeverity::Critical),
        (1, TicketSeverity::Critical),
        (3, TicketSeverity::Critical),
        (1, TicketSeverity::Warning),
        (2, TicketSeverity::Critical),
        (0, TicketSeverity::Critical),
        (3, TicketSeverity::Critical),
        (1, TicketSeverity::Warning),
        (1, TicketSeverity::Warning),
        (3, TicketSeverity::Critical),
        (1, TicketSeverity::Warning),
        (1, TicketSeverity::Critical),
    ];

    #[test]
    fn operations_durations_match_the_reviewed_design_constants() {
        assert_eq!(
            FAULT_INTERVAL,
            Duration::from_secs_f32(FAULT_INTERVAL_SECONDS)
        );
        assert_eq!(
            REPAIR_DURATION,
            Duration::from_secs_f32(REPAIR_DURATION_SECONDS)
        );
        assert_eq!(
            RESOLVED_DISPLAY,
            Duration::from_secs_f32(RESOLVED_DISPLAY_SECONDS)
        );
        assert_eq!(
            RACK_COOLDOWN,
            Duration::from_secs_f32(RACK_COOLDOWN_SECONDS)
        );
        assert_eq!(MAX_ACTIVE_TICKETS, 3);
        assert_eq!(REPAIR_INTERACTION_RANGE, 1.5);
        assert_eq!(FAULT_SCHEDULER_SEED, 0xCE11_5A1F_DA7A_CE01);
    }

    #[test]
    fn operations_rack_state_machine_matches_the_documented_diagram() {
        assert_eq!(RackState::default(), RackState::Healthy);
        assert_eq!(RackState::Healthy.dwell(), None);
        assert_eq!(RackState::Faulted.dwell(), None);
        assert_eq!(RackState::Repairing.dwell(), Some(REPAIR_DURATION));
        assert_eq!(RackState::Resolved.dwell(), Some(RESOLVED_DISPLAY));
        assert_eq!(RackState::Cooldown.dwell(), Some(RACK_COOLDOWN));

        assert_eq!(RackState::Repairing.next(), Some(RackState::Resolved));
        assert_eq!(RackState::Resolved.next(), Some(RackState::Cooldown));
        assert_eq!(RackState::Cooldown.next(), Some(RackState::Healthy));
        assert_eq!(RackState::Healthy.next(), None);
        assert_eq!(RackState::Faulted.next(), None);

        for state in RackState::ALL {
            assert_eq!(
                state.is_eligible_for_fault(),
                state == RackState::Healthy,
                "{state:?} eligibility"
            );
            assert_eq!(
                state.holds_ticket(),
                matches!(
                    state,
                    RackState::Faulted | RackState::Repairing | RackState::Resolved
                ),
                "{state:?} ticket"
            );
            assert_eq!(state.locks_movement(), state == RackState::Repairing);
            assert_eq!(state.shows_fault_badge(), state == RackState::Faulted);
            assert_eq!(state.shows_wrench_badge(), state == RackState::Repairing);
            assert_eq!(state.shows_healthy_badge(), state == RackState::Resolved);
        }
    }

    #[test]
    fn operations_rack_opens_a_fault_only_from_healthy() {
        let mut rack = RackOperations::new(0, PropId::new("rack-row-01"));
        assert!(rack.open_fault(TicketId(1)));
        assert_eq!(rack.state(), RackState::Faulted);
        assert_eq!(rack.ticket(), Some(TicketId(1)));
        assert_eq!(rack.elapsed(), Duration::ZERO);

        assert!(!rack.open_fault(TicketId(2)));
        assert_eq!(rack.ticket(), Some(TicketId(1)));

        for state in [
            RackState::Repairing,
            RackState::Resolved,
            RackState::Cooldown,
        ] {
            let mut busy = RackOperations::new(1, PropId::new("rack-row-02"));
            busy.state = state;
            assert!(!busy.open_fault(TicketId(9)), "{state:?} must refuse");
            assert_eq!(busy.ticket(), None);
        }
    }

    #[test]
    fn operations_rack_begins_repair_only_from_an_open_fault() {
        let mut rack = RackOperations::new(0, PropId::new("rack-row-01"));
        assert_eq!(
            rack.begin_repair(),
            None,
            "a healthy rack has nothing to fix"
        );

        rack.open_fault(TicketId(7));
        assert_eq!(rack.begin_repair(), Some(TicketId(7)));
        assert_eq!(rack.state(), RackState::Repairing);
        assert_eq!(rack.elapsed(), Duration::ZERO);
        assert_eq!(rack.remaining(), Some(REPAIR_DURATION));
        assert_eq!(rack.begin_repair(), None, "a repair never restarts itself");
    }

    #[test]
    fn operations_rack_timers_advance_exactly_on_their_boundaries() {
        let mut rack = RackOperations::new(0, PropId::new("rack-row-01"));
        rack.open_fault(TicketId(3));
        rack.begin_repair();

        // One tick before the repair boundary, then exactly on it.
        for _ in 0..179 {
            assert!(rack.advance(step()).is_empty());
        }
        assert_eq!(rack.state(), RackState::Repairing);
        assert_eq!(
            rack.advance(step()),
            vec![RackTransition::Resolved(TicketId(3))]
        );
        assert_eq!(rack.state(), RackState::Resolved);
        assert_eq!(rack.ticket(), Some(TicketId(3)));

        for _ in 0..119 {
            assert!(rack.advance(step()).is_empty());
        }
        assert_eq!(rack.state(), RackState::Resolved);
        assert_eq!(
            rack.advance(step()),
            vec![RackTransition::TicketRemoved(TicketId(3))]
        );
        assert_eq!(rack.state(), RackState::Cooldown);
        assert_eq!(rack.ticket(), None, "cooldown carries no active ticket");

        for _ in 0..479 {
            assert!(rack.advance(step()).is_empty());
        }
        assert_eq!(rack.state(), RackState::Cooldown);
        assert_eq!(rack.advance(step()), vec![RackTransition::Recovered]);
        assert_eq!(rack.state(), RackState::Healthy);
        assert_eq!(rack.elapsed(), Duration::ZERO);
        assert!(rack.state().is_eligible_for_fault());
    }

    #[test]
    fn operations_rack_timers_carry_their_remainder_and_never_skip_a_state() {
        let mut rack = RackOperations::new(0, PropId::new("rack-row-01"));
        rack.open_fault(TicketId(1));
        rack.begin_repair();

        // One hitch longer than every dwell added together still walks the
        // documented states in order rather than jumping to Healthy.
        let transitions = rack.advance(Duration::from_secs(30));
        assert_eq!(
            transitions,
            vec![
                RackTransition::Resolved(TicketId(1)),
                RackTransition::TicketRemoved(TicketId(1)),
                RackTransition::Recovered,
            ]
        );
        assert_eq!(rack.state(), RackState::Healthy);

        // A healthy or faulted rack has no timer at all.
        assert!(rack.advance(Duration::from_secs(30)).is_empty());
        assert_eq!(rack.elapsed(), Duration::ZERO);
        rack.open_fault(TicketId(2));
        assert!(rack.advance(Duration::from_secs(30)).is_empty());
        assert_eq!(rack.state(), RackState::Faulted);
    }

    #[test]
    fn operations_queue_sorts_critical_then_creation_tick_then_rack() {
        let mut queue = TicketQueue::default();
        assert!(
            queue
                .insert(ticket(3, 2, TicketSeverity::Warning, 10))
                .is_ok()
        );
        assert!(
            queue
                .insert(ticket(1, 3, TicketSeverity::Critical, 40))
                .is_ok()
        );
        assert!(
            queue
                .insert(ticket(2, 0, TicketSeverity::Critical, 20))
                .is_ok()
        );

        assert_eq!(
            queue
                .ordered()
                .iter()
                .map(|ticket| ticket.id.value())
                .collect::<Vec<_>>(),
            vec![2, 1, 3]
        );

        // Same severity and same creation tick fall back to the rack index.
        let mut tied = TicketQueue::default();
        assert!(
            tied.insert(ticket(9, 2, TicketSeverity::Critical, 5))
                .is_ok()
        );
        assert!(
            tied.insert(ticket(8, 1, TicketSeverity::Critical, 5))
                .is_ok()
        );
        assert_eq!(
            tied.ordered().iter().map(|t| t.rack).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(TicketSeverity::Critical < TicketSeverity::Warning);
        assert_eq!(
            ticket_priority(&ticket(1, 2, TicketSeverity::Warning, 7)),
            (TicketSeverity::Warning, 7, 2)
        );
    }

    #[test]
    fn operations_queue_refuses_capacity_and_duplicate_racks() {
        let mut queue = TicketQueue::default();
        for rack in 0..3 {
            assert!(
                queue
                    .insert(ticket(rack as u64 + 1, rack, TicketSeverity::Warning, 1))
                    .is_ok()
            );
        }
        assert!(queue.is_at_capacity());
        assert_eq!(
            queue.insert(ticket(4, 3, TicketSeverity::Critical, 2)),
            Err(TicketRejection::AtCapacity { active: 3 })
        );

        let mut duplicate = TicketQueue::default();
        assert!(
            duplicate
                .insert(ticket(1, 2, TicketSeverity::Warning, 1))
                .is_ok()
        );
        assert_eq!(
            duplicate.insert(ticket(2, 2, TicketSeverity::Critical, 2)),
            Err(TicketRejection::DuplicateRack {
                rack: 2,
                existing: TicketId(1)
            })
        );
        assert_eq!(duplicate.len(), 1);
        assert!(duplicate.contains_rack(2));
        assert!(!duplicate.contains_rack(0));
    }

    #[test]
    fn operations_queue_removes_by_identifier_and_keeps_priority_order() {
        let mut queue = TicketQueue::default();
        queue.insert(ticket(1, 0, TicketSeverity::Critical, 1)).ok();
        queue.insert(ticket(2, 1, TicketSeverity::Warning, 2)).ok();
        queue.insert(ticket(3, 2, TicketSeverity::Critical, 3)).ok();

        assert_eq!(queue.remove(TicketId(1)).map(|t| t.rack), Some(0));
        assert_eq!(queue.remove(TicketId(1)), None);
        assert_eq!(
            queue
                .ordered()
                .iter()
                .map(|t| t.id.value())
                .collect::<Vec<_>>(),
            vec![3, 2]
        );
        assert_eq!(queue.for_rack(2).map(|t| t.id), Some(TicketId(3)));
        assert_eq!(queue.get(TicketId(2)).map(|t| t.rack), Some(1));
        assert!(!queue.is_at_capacity());
    }

    #[test]
    fn operations_scheduler_emits_the_pinned_seeded_sequence() {
        assert_eq!(
            undisturbed_sequence(SEEDED_SEQUENCE.len()),
            SEEDED_SEQUENCE.to_vec()
        );
    }

    #[test]
    fn operations_scheduler_fires_exactly_on_the_fault_interval() {
        let mut scheduler = FaultScheduler::new(4);
        for tick in 1..=239u64 {
            assert!(
                scheduler.step(step(), tick, 0, &healthy()).is_none(),
                "tick {tick} fired before the interval"
            );
        }
        assert!(!scheduler.is_armed());
        let ticket = scheduler
            .step(step(), 240, 0, &healthy())
            .expect("the 240th tick completes exactly four seconds");
        assert_eq!(ticket.id, TicketId(1));
        assert_eq!(ticket.created_tick, 240);
        assert_eq!((ticket.rack, ticket.severity), SEEDED_SEQUENCE[0]);
        assert_eq!(scheduler.emitted(), 1);
        assert!(!scheduler.is_armed());

        for tick in 241..=479u64 {
            assert!(scheduler.step(step(), tick, 0, &healthy()).is_none());
        }
        let second = scheduler
            .step(step(), 480, 0, &healthy())
            .expect("the cadence stays four seconds");
        assert_eq!(second.id, TicketId(2));
        assert_eq!(second.created_tick, 480);
    }

    #[test]
    fn operations_scheduler_assigns_stable_monotonic_ticket_identifiers() {
        let mut scheduler = FaultScheduler::new(4);
        let mut ids = Vec::new();
        let mut tick = 0u64;
        while ids.len() < 6 {
            tick += 1;
            if let Some(ticket) = scheduler.step(step(), tick, 0, &healthy()) {
                ids.push(ticket.id.value());
            }
            assert!(tick < 100_000, "the scheduler stalled");
        }
        assert_eq!(ids, vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(TicketId(12).to_string(), "T0012");
    }

    #[test]
    fn operations_scheduler_consumes_no_randomness_while_the_queue_is_full() {
        let mut scheduler = FaultScheduler::new(4);
        let mut tick = 0u64;
        for _ in 0..240 {
            tick += 1;
            scheduler.step(step(), tick, MAX_ACTIVE_TICKETS, &healthy());
        }
        assert!(scheduler.is_armed(), "the matured opportunity must wait");
        assert_eq!(
            scheduler.blocked(),
            Some(ScheduleBlock::AtCapacity { active: 3 })
        );
        assert_eq!(scheduler.rng().draws(), 0, "a full queue must not draw");
        assert_eq!(scheduler.pending(), None);
        assert_eq!(scheduler.capacity_pauses(), 1);

        // Another ten seconds at capacity still never touches the stream, and
        // the armed opportunity never doubles up.
        for _ in 0..600 {
            tick += 1;
            scheduler.step(step(), tick, MAX_ACTIVE_TICKETS, &healthy());
        }
        assert_eq!(scheduler.rng().draws(), 0);
        assert_eq!(scheduler.capacity_pauses(), 1);
        assert_eq!(
            scheduler.elapsed(),
            Duration::from_nanos(80),
            "the paused timer keeps only the 80 ns the 240th fixed tick overshot by"
        );

        // The instant capacity reopens the paused opportunity fires, on that
        // very tick, with the first entry of the untouched sequence.
        tick += 1;
        let ticket = scheduler
            .step(step(), tick, MAX_ACTIVE_TICKETS - 1, &healthy())
            .expect("a reopened queue emits immediately");
        assert_eq!((ticket.rack, ticket.severity), SEEDED_SEQUENCE[0]);
        assert_eq!(ticket.created_tick, tick);
        assert_eq!(scheduler.rng().draws(), 2);
        assert_eq!(scheduler.blocked(), None);
    }

    #[test]
    fn operations_scheduler_holds_a_drawn_candidate_whose_rack_is_busy() {
        let mut scheduler = FaultScheduler::new(4);
        let (first_rack, _) = SEEDED_SEQUENCE[0];

        // The rack the stream wants already holds a ticket.
        let mut busy = healthy();
        busy[first_rack].state = RackState::Faulted;
        busy[first_rack].ticket = Some(TicketId(41));

        let mut tick = 0u64;
        for _ in 0..240 {
            tick += 1;
            assert!(scheduler.step(step(), tick, 1, &busy).is_none());
        }
        assert_eq!(
            scheduler.blocked(),
            Some(ScheduleBlock::DuplicateRack {
                rack: first_rack,
                existing: TicketId(41)
            })
        );
        assert_eq!(scheduler.duplicate_pauses(), 1);
        assert_eq!(
            scheduler.pending(),
            Some(FaultCandidate {
                rack: first_rack,
                severity: SEEDED_SEQUENCE[0].1
            })
        );
        let drawn = scheduler.rng().draws();

        // Waiting never rerolls; the held candidate is still the same one.
        for _ in 0..600 {
            tick += 1;
            scheduler.step(step(), tick, 1, &busy);
        }
        assert_eq!(scheduler.rng().draws(), drawn);
        assert_eq!(scheduler.duplicate_pauses(), 1);

        // A rack in cooldown is reported as busy rather than as a duplicate.
        let mut cooling = healthy();
        cooling[first_rack].state = RackState::Cooldown;
        tick += 1;
        assert!(scheduler.step(step(), tick, 0, &cooling).is_none());
        assert_eq!(
            scheduler.blocked(),
            Some(ScheduleBlock::RackBusy {
                rack: first_rack,
                state: RackState::Cooldown
            })
        );
        assert_eq!(scheduler.busy_pauses(), 1);

        // As soon as the rack recovers the held candidate emits unchanged.
        tick += 1;
        let ticket = scheduler
            .step(step(), tick, 0, &healthy())
            .expect("a recovered rack releases the held candidate");
        assert_eq!((ticket.rack, ticket.severity), SEEDED_SEQUENCE[0]);
        assert_eq!(scheduler.rng().draws(), drawn);
    }

    #[test]
    fn operations_scheduler_sequence_is_independent_of_repair_timing() {
        let expected = undisturbed_sequence(8);

        // The same scheduler, driven through a hostile timeline built from the
        // real rack state machine: the simulated player takes a wildly
        // different, deterministic number of ticks to reach each rack, so the
        // queue fills, drawn racks are found already ticketed, and drawn racks
        // are found still cooling down.
        let mut scheduler = FaultScheduler::new(4);
        let mut racks = (0..4)
            .map(|rack| RackOperations::new(rack, PropId::new(format!("rack-row-{:02}", rack + 1))))
            .collect::<Vec<_>>();
        let mut waited = [0u64; 4];
        let mut rounds = [0u64; 4];
        let mut active = 0usize;
        let mut emitted = Vec::new();
        let mut tick = 0u64;

        while emitted.len() < expected.len() {
            tick += 1;

            for rack in &mut racks {
                for transition in rack.advance(step()) {
                    if matches!(transition, RackTransition::TicketRemoved(_)) {
                        active -= 1;
                    }
                }
            }

            // The simulated player reaches each rack after a different, uneven
            // wait, so repairs never line up with the four second cadence.
            for (index, rack) in racks.iter_mut().enumerate() {
                if rack.state() != RackState::Faulted {
                    continue;
                }
                waited[index] += 1;
                let patience = 500 + (rounds[index] * 617 + index as u64 * 211) % 2_500;
                if waited[index] >= patience {
                    waited[index] = 0;
                    rounds[index] += 1;
                    rack.begin_repair();
                }
            }

            let snapshot = racks
                .iter()
                .map(|rack| RackSnapshot {
                    rack: rack.rack,
                    id: rack.id.clone(),
                    state: rack.state(),
                    ticket: rack.ticket(),
                })
                .collect::<Vec<_>>();
            if let Some(ticket) = scheduler.step(step(), tick, active, &snapshot) {
                racks[ticket.rack].open_fault(ticket.id);
                active += 1;
                emitted.push((ticket.rack, ticket.severity));
            }

            assert!(tick < 1_000_000, "the disturbed timeline stalled");
        }

        assert_eq!(
            emitted, expected,
            "repair timing must not reroll the stream"
        );

        // The timeline really was hostile. Without these the comparison above
        // would be vacuously true of any scheduler at all.
        assert!(
            scheduler.capacity_pauses() > 0,
            "the timeline never filled the queue"
        );
        assert!(
            scheduler.duplicate_pauses() > 0,
            "the timeline never drew a rack that already held a ticket"
        );
        assert!(
            scheduler.busy_pauses() > 0,
            "the timeline never drew a rack that was still cooling down"
        );
        assert_eq!(
            scheduler.rng().draws(),
            2 * expected.len() as u64,
            "exactly two words per emitted candidate, and not one more"
        );
    }

    #[test]
    fn operations_scheduler_never_draws_without_racks() {
        let mut scheduler = FaultScheduler::new(0);
        for tick in 1..=600u64 {
            assert!(scheduler.step(step(), tick, 0, &[]).is_none());
        }
        assert_eq!(scheduler.rng().draws(), 0);
        assert_eq!(scheduler.emitted(), 0);
    }

    #[test]
    fn operations_rack_distance_measures_the_collider_rectangle() {
        let center = Vec2::new(3.0, 0.0);
        let half = Vec2::new(0.8, 8.05);

        assert_eq!(rack_distance(center, center, half), 0.0);
        assert!((rack_distance(Vec2::new(5.0, 0.0), center, half) - 1.2).abs() < 1.0e-6);
        assert!((rack_distance(Vec2::new(1.85, 0.0), center, half) - 0.35).abs() < 1.0e-6);
        assert!(
            (rack_distance(Vec2::new(4.8, 10.05), center, half) - 5.0_f32.sqrt()).abs() < 1.0e-6
        );
        assert!((rack_distance(Vec2::new(3.0, 9.05), center, half) - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn operations_repair_selection_rejects_everything_out_of_range() {
        let far = RepairCandidate {
            ticket: TicketId(1),
            rack: 0,
            severity: TicketSeverity::Critical,
            created_tick: 1,
            distance: REPAIR_INTERACTION_RANGE + 1.0e-3,
        };
        assert!(!far.is_in_range());
        assert_eq!(select_repair_candidate(&[far]), None);
        assert_eq!(select_repair_candidate(&[]), None);

        let edge = RepairCandidate {
            distance: REPAIR_INTERACTION_RANGE,
            ..far
        };
        assert!(edge.is_in_range(), "the range boundary is inclusive");
        assert_eq!(select_repair_candidate(&[edge]), Some(edge));
    }

    #[test]
    fn operations_repair_selection_orders_severity_then_distance_then_age_then_rack() {
        let base = RepairCandidate {
            ticket: TicketId(1),
            rack: 0,
            severity: TicketSeverity::Warning,
            created_tick: 10,
            distance: 0.2,
        };
        let critical = RepairCandidate {
            ticket: TicketId(2),
            rack: 3,
            severity: TicketSeverity::Critical,
            created_tick: 99,
            distance: 1.4,
        };
        assert_eq!(
            select_repair_candidate(&[base, critical]).map(|found| found.ticket),
            Some(TicketId(2)),
            "severity outranks distance"
        );

        // Same severity: the nearer rack wins even though it is younger.
        let near = RepairCandidate {
            ticket: TicketId(3),
            rack: 2,
            severity: TicketSeverity::Critical,
            created_tick: 500,
            distance: 0.1,
        };
        assert_eq!(
            select_repair_candidate(&[critical, near]).map(|found| found.ticket),
            Some(TicketId(3))
        );

        // Same severity and identical distance: the older ticket wins.
        let older = RepairCandidate {
            ticket: TicketId(4),
            created_tick: 5,
            ..near
        };
        assert_eq!(
            select_repair_candidate(&[near, older]).map(|found| found.ticket),
            Some(TicketId(4))
        );

        // Same severity, distance and creation tick: the lower rack wins.
        let low = RepairCandidate {
            ticket: TicketId(5),
            rack: 1,
            ..older
        };
        assert_eq!(
            select_repair_candidate(&[older, low]).map(|found| found.ticket),
            Some(TicketId(5))
        );
        assert_eq!(
            select_repair_candidate(&[low, older]).map(|found| found.ticket),
            Some(TicketId(5)),
            "selection must not depend on input order"
        );
    }

    #[test]
    fn operations_roster_indexes_every_authored_rack_row() {
        let blueprint = SceneBlueprint::v0();
        let racks = blueprint
            .visuals
            .iter()
            .filter(|visual| visual.asset == RACK_ASSET_KIND)
            .map(|visual| visual.id.as_str().to_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            racks,
            vec![
                "rack-row-01".to_owned(),
                "rack-row-02".to_owned(),
                "rack-row-03".to_owned(),
                "rack-row-04".to_owned(),
            ]
        );
        let mut sorted = racks.clone();
        sorted.sort();
        assert_eq!(sorted, racks, "authored order is already the stable order");
        assert_eq!(RACK_ASSET_KIND, AssetKind::RackRow);
    }
}
