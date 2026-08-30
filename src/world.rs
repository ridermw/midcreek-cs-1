//! Spawning the authored data hall from the validated scene blueprint.
//!
//! ```text
//! AssetLoadState::Ready
//!        |
//!        v
//! SceneBlueprint::validate()
//!        |
//!        +-- errors --> HallErrors + HallState::Invalid (nothing spawns)
//!        |
//!        v
//! extract collider rectangles once -> HallColliders
//!        |
//!        v
//! spawn ordered visuals under one HallRoot
//!    unit primitive -> cached mesh + palette material
//!    generated module -> one shared WorldAssetRoot handle
//!        |
//!        v
//! HallState::Ready
//! ```
//!
//! Visual and collider lists stay separate and are joined only by their stable
//! [`PropId`], so a missing, duplicated, or orphaned identifier stops the hall
//! from spawning at all.

use bevy::prelude::*;

use crate::{
    CellShiftSet,
    assets::{AssetLoadState, GeneratedAssets, RenderAssets},
    design::{
        AssetKind, ColliderSpec, PropId, SceneBlueprint, SceneValidationError, TransformSpec,
        VisualSpec,
    },
};

/// Explicit lifecycle of the authored hall.
#[derive(States, Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum HallState {
    /// Assets are not ready yet, so nothing has been spawned.
    #[default]
    Unbuilt,
    /// Every authored visual has been spawned and the colliders are cached.
    Ready,
    /// The blueprint failed validation; see [`HallErrors`].
    Invalid,
}

/// The blueprint the hall spawns. Insert this before startup to override the
/// authored [`SceneBlueprint::v0`].
#[derive(Resource, Clone, Debug)]
pub struct HallBlueprint(pub SceneBlueprint);

impl Default for HallBlueprint {
    fn default() -> Self {
        Self(SceneBlueprint::v0())
    }
}

/// Validation errors that stopped the hall from spawning.
#[derive(Resource, Clone, Debug, Default)]
pub struct HallErrors(Vec<SceneValidationError>);

impl HallErrors {
    /// Every error reported by the blueprint validator, in aggregate order.
    pub fn errors(&self) -> &[SceneValidationError] {
        &self.0
    }
}

/// Collider rectangles extracted once from the validated blueprint. Movement
/// scans this vector linearly; one authored room does not need a spatial index.
#[derive(Resource, Clone, Debug)]
pub struct HallColliders(Vec<ColliderSpec>);

impl From<Vec<ColliderSpec>> for HallColliders {
    fn from(colliders: Vec<ColliderSpec>) -> Self {
        Self(colliders)
    }
}

impl HallColliders {
    /// Every cached collider, in blueprint order.
    pub fn all(&self) -> &[ColliderSpec] {
        &self.0
    }

    /// The cached collider joined to one stable [`PropId`].
    pub fn get(&self, id: &PropId) -> Option<&ColliderSpec> {
        self.0.iter().find(|collider| &collider.id == id)
    }

    /// The first cached collider a disc of `radius` at `center` overlaps.
    pub fn first_overlap(&self, center: Vec2, radius: f32) -> Option<&ColliderSpec> {
        self.0.iter().find(|collider| {
            let delta = (center - collider.center).abs();
            delta.x <= collider.half_extents.x + radius
                && delta.y <= collider.half_extents.y + radius
        })
    }

    /// Whether a disc of `radius` at `center` overlaps any cached collider.
    pub fn overlaps(&self, center: Vec2, radius: f32) -> bool {
        self.first_overlap(center, radius).is_some()
    }
}

/// Ground position the technician spawns at.
#[derive(Resource, Clone, Copy, Debug)]
pub struct PlayerSpawnPoint(pub Vec2);

/// Parent of every spawned hall prop.
#[derive(Component, Clone, Copy, Debug)]
pub struct HallRoot;

/// One spawned authored prop, carrying the stable identifiers the blueprint,
/// colliders, and later gameplay systems all join on.
#[derive(Component, Clone, Debug)]
pub struct HallProp {
    /// Stable identifier shared with the collider list.
    pub id: PropId,
    /// Authored asset kind.
    pub asset: AssetKind,
}

/// Converts an authored transform specification into a Bevy transform.
pub fn prop_transform(spec: &TransformSpec) -> Transform {
    Transform {
        translation: spec.translation,
        rotation: Quat::from_rotation_y(spec.rotation_y_degrees.to_radians()),
        scale: spec.scale,
    }
}

/// Builds the authored hall once the generated assets are ready.
pub struct HallPlugin;

impl Plugin for HallPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<HallState>()
            .init_resource::<HallErrors>()
            .add_systems(
                OnEnter(AssetLoadState::Ready),
                spawn_hall.in_set(CellShiftSet::SpawnWorld),
            );
    }
}

fn spawn_hall(
    mut commands: Commands,
    blueprint: Option<Res<HallBlueprint>>,
    generated: Res<GeneratedAssets>,
    render: Res<RenderAssets>,
    mut errors: ResMut<HallErrors>,
    mut next: ResMut<NextState<HallState>>,
) {
    let blueprint = blueprint
        .map(|resource| resource.0.clone())
        .unwrap_or_else(SceneBlueprint::v0);

    let validation = blueprint.validate();
    if !validation.is_empty() {
        for error in &validation {
            error!("hall blueprint is invalid: {error:?}");
        }
        errors.0 = validation;
        next.set(HallState::Invalid);
        return;
    }

    commands.insert_resource(HallColliders(blueprint.colliders.clone()));
    commands.insert_resource(PlayerSpawnPoint(blueprint.player_spawn));

    let root = commands
        .spawn((
            HallRoot,
            Name::new("data-hall"),
            Transform::IDENTITY,
            Visibility::default(),
        ))
        .id();

    for visual in &blueprint.visuals {
        let prop = spawn_prop(&mut commands, visual, &generated, &render);
        commands.entity(prop).insert(ChildOf(root));
    }

    next.set(HallState::Ready);
}

fn spawn_prop(
    commands: &mut Commands,
    visual: &VisualSpec,
    generated: &GeneratedAssets,
    render: &RenderAssets,
) -> Entity {
    let transform = prop_transform(&visual.transform);
    let identity = (
        HallProp {
            id: visual.id.clone(),
            asset: visual.asset,
        },
        Name::new(visual.id.as_str().to_owned()),
        transform,
    );

    if let Some((shape, role)) = visual.asset.primitive() {
        return commands
            .spawn((
                identity,
                Mesh3d(render.mesh(shape)),
                MeshMaterial3d(render.material(role)),
            ))
            .id();
    }

    let handle = generated
        .module_scene(visual.asset)
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "{} declares {:?}, which has neither a primitive nor a generated module",
                visual.id, visual.asset
            )
        });
    commands.spawn((identity, WorldAssetRoot(handle))).id()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::module_for;
    use crate::design::{
        AISLE_CENTER_X, FLOOR_MARKING_HEIGHT, PLAYER_RADIUS, PaletteRole, PrimitiveShape,
        RACK_ROW_X, RENDER_APRON_DROP, RENDER_COVERAGE_SIZE, ROOM_SIZE,
    };

    fn colliders() -> HallColliders {
        HallColliders(SceneBlueprint::v0().colliders)
    }

    #[test]
    fn hall_colliders_preserve_blueprint_order_and_join_on_prop_id() {
        let blueprint = SceneBlueprint::v0();
        let cached = HallColliders(blueprint.colliders.clone());

        assert_eq!(cached.all(), blueprint.colliders.as_slice());
        for collider in &blueprint.colliders {
            assert_eq!(cached.get(&collider.id), Some(collider));
            assert!(
                blueprint
                    .visual(collider.id.as_str())
                    .is_some_and(|visual| visual.collision_required)
            );
        }
        assert_eq!(cached.get(&PropId::new("not-a-prop")), None);
    }

    #[test]
    fn hall_colliders_scan_linearly_and_report_the_first_overlap() {
        let cached = colliders();

        assert_eq!(
            cached
                .first_overlap(Vec2::new(RACK_ROW_X[0], 0.0), PLAYER_RADIUS)
                .map(|collider| collider.id.as_str().to_owned()),
            Some("rack-row-01".to_owned())
        );
        assert_eq!(
            cached
                .first_overlap(Vec2::new(-13.0, -10.0), PLAYER_RADIUS)
                .map(|collider| collider.id.as_str().to_owned()),
            Some("utility-cart".to_owned())
        );
        assert!(!cached.overlaps(Vec2::new(AISLE_CENTER_X[0], -11.0), PLAYER_RADIUS));
        assert!(!cached.overlaps(Vec2::new(AISLE_CENTER_X[1], 0.0), PLAYER_RADIUS));
    }

    #[test]
    fn hall_collider_overlap_respects_the_player_radius() {
        let cached = colliders();
        let rack = cached
            .get(&PropId::new("rack-row-01"))
            .expect("authored rack collider");
        let edge = rack.center.x + rack.half_extents.x;

        assert!(cached.overlaps(Vec2::new(edge + PLAYER_RADIUS * 0.5, 0.0), PLAYER_RADIUS));
        assert!(!cached.overlaps(Vec2::new(edge + PLAYER_RADIUS * 1.5, 0.0), PLAYER_RADIUS));
        assert!(cached.overlaps(Vec2::new(edge - 0.001, 0.0), 0.0));
        assert!(!cached.overlaps(Vec2::new(edge + 0.001, 0.0), 0.0));
    }

    #[test]
    fn hall_prop_transform_applies_authored_translation_rotation_and_scale() {
        let spec = TransformSpec {
            translation: Vec3::new(1.0, 2.0, 3.0),
            rotation_y_degrees: 90.0,
            scale: Vec3::new(4.0, 1.0, 5.0),
        };
        let transform = prop_transform(&spec);

        assert_eq!(transform.translation, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(transform.scale, Vec3::new(4.0, 1.0, 5.0));
        assert_eq!(
            transform.rotation,
            Quat::from_rotation_y(90.0_f32.to_radians())
        );
        assert!((transform.rotation * Vec3::X).distance(Vec3::new(0.0, 0.0, -1.0)) < 1.0e-6);
        assert_eq!(
            prop_transform(&TransformSpec::from_translation(Vec3::ZERO)),
            Transform::IDENTITY
        );
    }

    #[test]
    fn hall_world_spawns_one_entity_per_authored_visual_kind() {
        let blueprint = SceneBlueprint::v0();
        let primitives = blueprint
            .visuals
            .iter()
            .filter(|visual| visual.asset.primitive().is_some())
            .count();
        let modules = blueprint
            .visuals
            .iter()
            .filter(|visual| module_for(visual.asset).is_some())
            .count();

        assert_eq!(primitives + modules, blueprint.visuals.len());
        assert_eq!(primitives, 14);
        assert_eq!(modules, 16);
        assert_eq!(
            blueprint
                .visual("floor")
                .map(|visual| visual.asset.primitive()),
            Some(Some((PrimitiveShape::Quad, PaletteRole::FloorLight)))
        );
        assert_eq!(
            blueprint
                .visual("render-apron")
                .map(|visual| visual.asset.primitive()),
            Some(Some((PrimitiveShape::Quad, PaletteRole::FloorShadow)))
        );
    }

    #[test]
    fn hall_world_spawns_the_render_apron_as_a_visual_only_background() {
        let blueprint = SceneBlueprint::v0();
        let apron = blueprint
            .visual("render-apron")
            .expect("the authored blueprint must carry the rendered-coverage apron");

        assert_eq!(apron.asset, AssetKind::RenderApron);
        assert!(
            !apron.collision_required,
            "the apron is building shell, not a room the technician can hit"
        );
        assert_eq!(blueprint.collider("render-apron"), None);
        assert_eq!(
            blueprint.count_of(AssetKind::RenderApron),
            1,
            "exactly one apron covers the rendered area"
        );

        // Exactly 72 m square, centred on the room, and dropped clear of the
        // coplanar 40 m floor so the shared square never z-fights.
        assert_eq!(
            apron.transform.scale,
            Vec3::new(RENDER_COVERAGE_SIZE.x, 1.0, RENDER_COVERAGE_SIZE.y)
        );
        assert_eq!(
            apron.transform.translation,
            Vec3::new(0.0, -RENDER_APRON_DROP, 0.0)
        );
        let floor = blueprint.visual("floor").expect("authored floor");
        assert!(
            apron.transform.translation.y < floor.transform.translation.y,
            "the apron must sit below the walkable floor"
        );
        assert!(
            apron.transform.translation.y < -FLOOR_MARKING_HEIGHT,
            "the apron must also sit below the painted floor markings"
        );

        // It is drawn before everything it sits behind.
        assert_eq!(blueprint.visuals[0].id.as_str(), "render-apron");

        // The apron is strictly larger than the walkable room on both axes, and
        // the walkable room is unchanged.
        assert_eq!(blueprint.room.size, ROOM_SIZE);
        assert_eq!(blueprint.room.coverage, RENDER_COVERAGE_SIZE);
        assert!(
            blueprint.room.coverage.x > blueprint.room.size.x
                && blueprint.room.coverage.y > blueprint.room.size.y,
            "the apron must be strictly larger than the walkable room"
        );
        assert_eq!(blueprint.validate(), Vec::<SceneValidationError>::new());
    }

    #[test]
    fn hall_world_keeps_every_collider_inside_the_room() {
        let blueprint = SceneBlueprint::v0();
        let half = ROOM_SIZE * 0.5;

        for collider in &blueprint.colliders {
            assert!(collider.center.x.abs() + collider.half_extents.x <= half.x);
            assert!(collider.center.y.abs() + collider.half_extents.y <= half.y);
        }
        assert_eq!(blueprint.validate(), Vec::<SceneValidationError>::new());
    }
}
