//! Loading the committed generated glTF assets and the shared render handles
//! the hall spawns from.
//!
//! ```text
//! Startup: load every generated document and module scene
//!    |
//!    v
//! AssetLoadState::Loading --- any handle failed ------> AssetLoadState::Failed
//!    |                                                       ^
//!    | every handle loaded with dependencies                 |
//!    v                                                       |
//! declared module scene names present? --- no ---------------+
//!    |                                                       |
//!   yes                                                      |
//!    v                                                       |
//! named scene == the indexed handle we spawn? --- no --------+
//!    |
//!   yes
//!    v
//! record AssetReadyProof for every handle checked
//!    |
//!    v
//! AssetLoadState::Ready
//! ```
//!
//! There is no procedural fallback. A missing, corrupt, mislabelled, or
//! misbound asset moves the app into [`AssetLoadState::Failed`] and records the
//! offending file in [`AssetLoadReport`].
//!
//! [`AssetLoadState::Ready`] is a one-way initial-load latch. The evidence for
//! it is [`AssetReadyProof`], captured in the same branch and frame that sets
//! the latch, because the live asset server keeps moving afterwards and a later
//! re-query is not the snapshot that caused Ready.

use bevy::{
    asset::{RecursiveDependencyLoadState, UntypedAssetId},
    gltf::GltfAssetLabel,
    prelude::*,
};

use crate::{
    CellShiftSet,
    assetgen::ASSET_MODULES,
    design::{AssetKind, PaletteRole, PrimitiveShape},
};

/// Directory, relative to the Bevy asset root, holding the generated GLBs.
pub const GENERATED_ASSET_DIRECTORY: &str = "generated";

/// Explicit load lifecycle of the generated assets.
#[derive(States, Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum AssetLoadState {
    /// At least one generated handle is still resolving.
    #[default]
    Loading,
    /// Every generated document and module scene loaded and matched its
    /// declared identity.
    Ready,
    /// A generated asset is missing, unreadable, or does not expose the module
    /// scene the pipeline declares.
    Failed,
}

/// One generated glTF scene: the asset file that holds it, its declared module
/// name, and its scene index within that file.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GeneratedModule {
    /// Asset file stem, for example `rack`.
    pub asset: &'static str,
    /// Declared module name, which is also the glTF scene name.
    pub module: &'static str,
    /// Index of the scene inside the asset file.
    pub scene_index: usize,
}

impl GeneratedModule {
    /// Bevy asset path of the file holding this module.
    pub fn path(&self) -> String {
        format!("{GENERATED_ASSET_DIRECTORY}/{}.glb", self.asset)
    }

    /// Bevy asset path of this module's scene sub-asset.
    pub fn scene_path(&self) -> String {
        format!("{}#Scene{}", self.path(), self.scene_index)
    }
}

/// Every generated module the pipeline declares, in stable order.
pub fn generated_modules() -> Vec<GeneratedModule> {
    ASSET_MODULES
        .into_iter()
        .flat_map(|(asset, modules)| {
            modules
                .iter()
                .enumerate()
                .map(move |(scene_index, module)| GeneratedModule {
                    asset,
                    module,
                    scene_index,
                })
        })
        .collect()
}

/// The generated module a hall [`AssetKind`] resolves to, if any. Kinds that
/// resolve to a cached unit primitive return `None`.
pub fn module_for(kind: AssetKind) -> Option<GeneratedModule> {
    let module = match kind {
        AssetKind::FloorGrid => "floor-grid",
        AssetKind::RackRow => "rack-row",
        AssetKind::CoolingUnit => "cooling-unit",
        AssetKind::OverheadTray => "overhead-tray",
        AssetKind::HoseDrop => "hose-drop",
        AssetKind::UtilityCart => "utility-cart",
        AssetKind::StepStool => "step-stool",
        AssetKind::RenderApron | AssetKind::Floor | AssetKind::Wall | AssetKind::FloorMarking => {
            return None;
        }
    };
    generated_modules()
        .into_iter()
        .find(|generated| generated.module == module)
}

/// Handles for every generated document and module scene.
#[derive(Resource, Clone, Debug)]
pub struct GeneratedAssets {
    documents: Vec<(&'static str, Handle<Gltf>)>,
    scenes: Vec<(GeneratedModule, Handle<WorldAsset>)>,
}

impl GeneratedAssets {
    /// Every generated glTF document, keyed by asset file stem.
    pub fn documents(&self) -> &[(&'static str, Handle<Gltf>)] {
        &self.documents
    }

    /// Every generated module scene, keyed by its declared module.
    pub fn scenes(&self) -> &[(GeneratedModule, Handle<WorldAsset>)] {
        &self.scenes
    }

    /// The document handle for one asset file stem.
    pub fn document(&self, asset: &str) -> Option<&Handle<Gltf>> {
        self.documents
            .iter()
            .find(|(name, _)| *name == asset)
            .map(|(_, handle)| handle)
    }

    /// The scene handle for one declared module.
    pub fn scene(&self, asset: &str, module: &str) -> Option<&Handle<WorldAsset>> {
        self.scenes
            .iter()
            .find(|(generated, _)| generated.asset == asset && generated.module == module)
            .map(|(_, handle)| handle)
    }

    /// The single shared scene handle a hall [`AssetKind`] spawns.
    pub fn module_scene(&self, kind: AssetKind) -> Option<&Handle<WorldAsset>> {
        let module = module_for(kind)?;
        self.scene(module.asset, module.module)
    }
}

/// Every generated-asset failure observed while resolving the load state.
#[derive(Resource, Clone, Debug, Default)]
pub struct AssetLoadReport {
    failures: Vec<String>,
}

impl AssetLoadReport {
    /// Explicit failure messages, each naming the offending asset path.
    pub fn failures(&self) -> &[String] {
        &self.failures
    }
}

/// One generated glTF document observed fully loaded in the frame that decided
/// [`AssetLoadState::Ready`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvenDocument {
    /// Asset file stem, for example `rack`.
    pub asset: &'static str,
    /// Bevy asset path that was loaded.
    pub path: String,
    /// The tracked document handle that was observed loaded.
    pub handle: UntypedAssetId,
}

/// One generated module scene observed fully loaded *and* identity-checked in
/// the frame that decided [`AssetLoadState::Ready`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvenModule {
    /// The declared module: asset file stem, module name, and scene index.
    pub module: GeneratedModule,
    /// Bevy asset path of the scene sub-asset.
    pub scene_path: String,
    /// The tracked scene handle the hall spawns.
    pub handle: UntypedAssetId,
    /// The scene the document binds to the declared module name.
    pub named_scene: UntypedAssetId,
    /// The scene the document holds at the declared index.
    pub indexed_scene: UntypedAssetId,
}

/// Immutable evidence of why [`AssetLoadState::Ready`] was set.
///
/// [`AssetLoadState::Ready`] is a one-way initial-load latch, so the live asset
/// server keeps moving after it: Bevy re-emits sub-asset load events, and a
/// later re-query is not the snapshot that caused Ready. This proof *is* that
/// snapshot. It is built only in the branch where every tracked document and
/// scene reported loaded and every module name, index, and handle agreed, and
/// it is published in the same frame the latch is set. There is no proof while
/// [`AssetLoadState::Loading`], and none after [`AssetLoadState::Failed`].
#[derive(Resource, Clone, Debug, Eq, PartialEq)]
pub struct AssetReadyProof {
    documents: Vec<ProvenDocument>,
    modules: Vec<ProvenModule>,
}

impl AssetReadyProof {
    /// Records the documents and modules observed loaded and identity-checked.
    pub fn new(documents: Vec<ProvenDocument>, modules: Vec<ProvenModule>) -> Self {
        Self { documents, modules }
    }

    /// Every generated document this proof accounts for.
    pub fn documents(&self) -> &[ProvenDocument] {
        &self.documents
    }

    /// Every generated module scene this proof accounts for.
    pub fn modules(&self) -> &[ProvenModule] {
        &self.modules
    }

    /// Every way this proof fails to account for the generated assets the hall
    /// tracks and the modules the pipeline declares. Readiness requires this to
    /// be empty, so a proof that skips, duplicates, or misbinds a handle can
    /// never satisfy [`AssetLoadState::Ready`].
    pub fn gaps(&self, generated: &GeneratedAssets) -> Vec<String> {
        let mut gaps = Vec::new();

        for (asset, handle) in generated.documents() {
            let path = format!("{GENERATED_ASSET_DIRECTORY}/{asset}.glb");
            let proven = self
                .documents
                .iter()
                .filter(|proven| proven.asset == *asset)
                .collect::<Vec<_>>();
            let [proven] = proven[..] else {
                gaps.push(format!(
                    "{path} is proven loaded {} times instead of once",
                    proven.len()
                ));
                continue;
            };
            if proven.path != path {
                gaps.push(format!("{path} is proven under the path {}", proven.path));
            }
            if proven.handle != handle.id().untyped() {
                gaps.push(format!(
                    "{path} is proven with a handle the hall never loaded"
                ));
            }
        }
        for proven in &self.documents {
            if generated.document(proven.asset).is_none() {
                gaps.push(format!("{} is proven but is not generated", proven.path));
            }
        }

        for module in generated_modules() {
            let scene_path = module.scene_path();
            let proven = self
                .modules
                .iter()
                .filter(|proven| proven.module == module)
                .collect::<Vec<_>>();
            let [proven] = proven[..] else {
                gaps.push(format!(
                    "{scene_path} is proven loaded {} times instead of once",
                    proven.len()
                ));
                continue;
            };
            let Some(spawned) = generated
                .scene(module.asset, module.module)
                .map(|handle| handle.id().untyped())
            else {
                gaps.push(format!("{scene_path} is not tracked for spawning"));
                continue;
            };
            if proven.scene_path != scene_path {
                gaps.push(format!(
                    "{scene_path} is proven under the path {}",
                    proven.scene_path
                ));
            }
            if proven.handle != spawned {
                gaps.push(format!(
                    "{scene_path} is proven with a handle the hall never spawns"
                ));
            }
            if proven.named_scene != spawned || proven.indexed_scene != spawned {
                gaps.push(format!(
                    "{scene_path} is proven without {} and scene {} resolving to the spawned handle",
                    module.module, module.scene_index
                ));
            }
        }
        for proven in &self.modules {
            if !generated_modules().contains(&proven.module) {
                gaps.push(format!(
                    "{} is proven but is not declared",
                    proven.scene_path
                ));
            }
        }

        gaps
    }
}

/// One reusable unit mesh per primitive shape and one material per palette
/// role. Nothing else may create hall meshes or materials at runtime.
#[derive(Resource, Clone, Debug)]
pub struct RenderAssets {
    meshes: Vec<(PrimitiveShape, Handle<Mesh>)>,
    materials: Vec<(PaletteRole, Handle<StandardMaterial>)>,
}

impl RenderAssets {
    fn new(meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>) -> Self {
        Self {
            meshes: PrimitiveShape::ALL
                .into_iter()
                .map(|shape| {
                    let mesh = match shape {
                        PrimitiveShape::Cuboid => Mesh::from(Cuboid::new(1.0, 1.0, 1.0)),
                        PrimitiveShape::Quad => {
                            Mesh::from(Plane3d::default().mesh().size(1.0, 1.0))
                        }
                    };
                    (shape, meshes.add(mesh))
                })
                .collect(),
            materials: PaletteRole::ALL
                .into_iter()
                .map(|role| {
                    let material = materials.add(StandardMaterial {
                        base_color: Color::Srgba(role.color()),
                        unlit: true,
                        ..default()
                    });
                    (role, material)
                })
                .collect(),
        }
    }

    /// The one cached unit mesh for a primitive shape.
    pub fn mesh(&self, shape: PrimitiveShape) -> Handle<Mesh> {
        self.meshes
            .iter()
            .find(|(cached, _)| *cached == shape)
            .map(|(_, handle)| handle.clone())
            .expect("every primitive shape is cached at startup")
    }

    /// The one cached material for a palette role.
    pub fn material(&self, role: PaletteRole) -> Handle<StandardMaterial> {
        self.materials
            .iter()
            .find(|(cached, _)| *cached == role)
            .map(|(_, handle)| handle.clone())
            .expect("every palette role is cached at startup")
    }
}

/// Loads every generated asset and publishes the shared render handles.
pub struct GeneratedAssetPlugin;

impl Plugin for GeneratedAssetPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<AssetLoadState>()
            .init_resource::<AssetLoadReport>()
            .add_systems(Startup, (load_generated_assets, cache_render_assets))
            .add_systems(
                Update,
                resolve_asset_load_state
                    .in_set(CellShiftSet::AssetReady)
                    .run_if(in_state(AssetLoadState::Loading)),
            );
    }
}

fn load_generated_assets(mut commands: Commands, server: Res<AssetServer>) {
    let modules = generated_modules();
    let mut documents = Vec::new();
    for (asset, _) in ASSET_MODULES {
        documents.push((
            asset,
            server.load::<Gltf>(format!("{GENERATED_ASSET_DIRECTORY}/{asset}.glb")),
        ));
    }
    let scenes = modules
        .into_iter()
        .map(|module| {
            let handle = server.load::<WorldAsset>(
                GltfAssetLabel::Scene(module.scene_index).from_asset(module.path()),
            );
            (module, handle)
        })
        .collect();

    commands.insert_resource(GeneratedAssets { documents, scenes });
}

fn cache_render_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(RenderAssets::new(&mut meshes, &mut materials));
}

fn resolve_asset_load_state(
    mut commands: Commands,
    server: Res<AssetServer>,
    generated: Res<GeneratedAssets>,
    documents: Res<Assets<Gltf>>,
    mut report: ResMut<AssetLoadReport>,
    mut next: ResMut<NextState<AssetLoadState>>,
) {
    let mut failures = Vec::new();
    let mut pending = false;
    let mut proven_documents = Vec::new();
    let mut loaded_scenes = Vec::new();

    for (asset, handle) in generated.documents() {
        match load_outcome(&server, handle.id().untyped()) {
            LoadOutcome::Loaded => proven_documents.push(ProvenDocument {
                asset,
                path: format!("{GENERATED_ASSET_DIRECTORY}/{asset}.glb"),
                handle: handle.id().untyped(),
            }),
            LoadOutcome::Pending => pending = true,
            LoadOutcome::Failed(message) => failures.push(format!(
                "{GENERATED_ASSET_DIRECTORY}/{asset}.glb failed to load: {message}"
            )),
        }
    }
    for (module, handle) in generated.scenes() {
        match load_outcome(&server, handle.id().untyped()) {
            LoadOutcome::Loaded => loaded_scenes.push(handle.id().untyped()),
            LoadOutcome::Pending => pending = true,
            LoadOutcome::Failed(message) => {
                failures.push(format!("{} failed to load: {message}", module.scene_path()))
            }
        }
    }

    if failures.is_empty() && pending {
        return;
    }

    let mut proven_modules = Vec::new();
    if failures.is_empty() {
        for module in generated_modules() {
            let Some(document) = generated
                .document(module.asset)
                .and_then(|handle| documents.get(handle))
            else {
                failures.push(format!("{} loaded without a glTF document", module.path()));
                continue;
            };
            let Some(spawned) = generated.scene(module.asset, module.module) else {
                failures.push(format!(
                    "{} is not tracked for spawning",
                    module.scene_path()
                ));
                continue;
            };
            let Some(named) = document.named_scenes.get(module.module) else {
                failures.push(format!(
                    "{} does not expose the declared module scene {}",
                    module.path(),
                    module.module
                ));
                continue;
            };
            // Presence of the name is not enough: the hall spawns the indexed
            // sub-asset, so that exact handle must be the one the document
            // binds to the declared module name.
            let Some(indexed) = document
                .scenes
                .get(module.scene_index)
                .filter(|scene| scene.id() == named.id())
            else {
                failures.push(format!(
                    "{} binds {} to a different scene than {}",
                    module.path(),
                    module.module,
                    module.scene_path()
                ));
                continue;
            };
            if spawned.id() != named.id() {
                failures.push(format!(
                    "{} does not resolve to the {} scene of {}",
                    module.scene_path(),
                    module.module,
                    module.path()
                ));
                continue;
            }
            if !loaded_scenes.contains(&spawned.id().untyped()) {
                failures.push(format!("{} was never observed loaded", module.scene_path()));
                continue;
            }
            proven_modules.push(ProvenModule {
                module,
                scene_path: module.scene_path(),
                handle: spawned.id().untyped(),
                named_scene: named.id().untyped(),
                indexed_scene: indexed.id().untyped(),
            });
        }
    }

    if failures.is_empty() {
        // Readiness is a one-way latch, so the reason for it is captured here,
        // in the same branch and the same frame, and never re-derived later.
        let proof = AssetReadyProof::new(proven_documents, proven_modules);
        failures = proof.gaps(&generated);
        if failures.is_empty() {
            commands.insert_resource(proof);
            next.set(AssetLoadState::Ready);
            return;
        }
    }

    commands.remove_resource::<AssetReadyProof>();
    for failure in &failures {
        error!("generated asset failure: {failure}");
    }
    report.failures = failures;
    next.set(AssetLoadState::Failed);
}

enum LoadOutcome {
    Loaded,
    Pending,
    Failed(String),
}

fn load_outcome(server: &AssetServer, id: UntypedAssetId) -> LoadOutcome {
    classify_load_state(server.get_recursive_dependency_load_state(id))
}

fn classify_load_state(state: Option<RecursiveDependencyLoadState>) -> LoadOutcome {
    match state {
        Some(RecursiveDependencyLoadState::Loaded) => LoadOutcome::Loaded,
        Some(RecursiveDependencyLoadState::Failed(error)) => LoadOutcome::Failed(error.to_string()),
        Some(RecursiveDependencyLoadState::Loading)
        | Some(RecursiveDependencyLoadState::NotLoaded) => LoadOutcome::Pending,
        None => LoadOutcome::Failed("recursive dependency load state is missing".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_recursive_dependency_state_is_a_terminal_failure() {
        assert!(matches!(
            classify_load_state(None),
            LoadOutcome::Failed(message) if message.contains("missing")
        ));
    }
}
