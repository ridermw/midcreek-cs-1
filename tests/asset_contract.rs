use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use midcreek_cs_1::{
    assetgen::{
        ASSET_MODULES, ASSET_NAMES, AssetGenError, GENERATED_DIR, GENERATOR_NAME,
        MAX_PRIMITIVES_PER_MESH, MAX_TRIANGLES_PER_ASSET, MAX_VERTICES_PER_PRIMITIVE, SOURCE_DIR,
        TECHNICIAN_BONES, TECHNICIAN_CLIPS, check_assets, generate_assets, generated_path,
        load_source, parse_source, source_path, write_assets,
    },
    design::PaletteRole,
};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TempRoot(PathBuf);

impl TempRoot {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_root(label: &str) -> TempRoot {
    let path = std::env::temp_dir().join(format!(
        "midcreek-{label}-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("temp root should be creatable");
    TempRoot(path)
}

fn committed_bytes(name: &str) -> Vec<u8> {
    fs::read(generated_path(&repo_root(), name)).expect("generated asset should be committed")
}

struct Loaded {
    document: gltf::Document,
    buffers: Vec<gltf::buffer::Data>,
    json: String,
}

fn load_glb(bytes: &[u8]) -> Loaded {
    let glb = gltf::binary::Glb::from_slice(bytes).expect("bytes should be a valid GLB container");
    let json = String::from_utf8(glb.json.to_vec()).expect("GLB JSON chunk should be UTF-8");
    let (document, buffers, _) = gltf::import_slice(bytes).expect("GLB should import");
    Loaded {
        document,
        buffers,
        json,
    }
}

fn asset(name: &str) -> Loaded {
    load_glb(&committed_bytes(name))
}

fn node_by_name<'a>(loaded: &'a Loaded, name: &str) -> gltf::Node<'a> {
    loaded
        .document
        .nodes()
        .find(|node| node.name() == Some(name))
        .unwrap_or_else(|| panic!("expected a node named {name}"))
}

fn scene_names(loaded: &Loaded) -> Vec<String> {
    loaded
        .document
        .scenes()
        .map(|scene| scene.name().unwrap_or_default().to_owned())
        .collect()
}

fn material_names(loaded: &Loaded) -> Vec<String> {
    loaded
        .document
        .materials()
        .map(|material| material.name().unwrap_or_default().to_owned())
        .collect()
}

fn approved_material_names() -> BTreeSet<String> {
    PaletteRole::ALL
        .iter()
        .map(|role| format!("{role:?}"))
        .collect()
}

fn triangle_count(loaded: &Loaded) -> usize {
    loaded
        .document
        .meshes()
        .flat_map(|mesh| mesh.primitives())
        .map(|primitive| {
            primitive
                .indices()
                .expect("every primitive should be indexed")
                .count()
                / 3
        })
        .sum()
}

// ---------------------------------------------------------------------------
// Source schema and invariants
// ---------------------------------------------------------------------------

#[test]
fn every_asset_has_a_committed_source_and_output() {
    for name in ASSET_NAMES {
        let source = source_path(&repo_root(), name);
        let generated = generated_path(&repo_root(), name);
        assert!(source.exists(), "missing source for {name}: {source:?}");
        assert!(
            generated.exists(),
            "missing generated asset for {name}: {generated:?}"
        );
        assert!(
            source.starts_with(repo_root().join(SOURCE_DIR)),
            "source must live under {SOURCE_DIR}"
        );
        assert!(
            generated.starts_with(repo_root().join(GENERATED_DIR)),
            "output must live under {GENERATED_DIR}"
        );
    }
}

#[test]
fn every_source_parses_and_validates() {
    for name in ASSET_NAMES {
        let source = load_source(&repo_root(), name)
            .unwrap_or_else(|error| panic!("{name} should parse and validate: {error}"));
        assert_eq!(source.asset, name);
        assert!(!source.modules.is_empty(), "{name} should declare modules");
    }
}

#[test]
fn malformed_source_reports_the_file_and_message() {
    let text = fs::read_to_string(fixture("unknown-role.ron")).expect("fixture should be readable");
    let error = parse_source(&text, "assets/source/unknown-role.ron")
        .expect_err("unknown palette roles must be rejected");
    let AssetGenError::Parse { path, message } = &error else {
        panic!("expected a parse error, got {error:?}");
    };
    assert_eq!(path, "assets/source/unknown-role.ron");
    assert!(
        message.contains("Neon"),
        "message should name the offending value, got {message}"
    );
}

#[test]
fn invariant_violations_report_the_offending_field() {
    let cases = [
        ("unknown-bone.ron", "bone", "bone-nope"),
        ("duplicate-module.ron", "modules", "rack-row"),
        ("degenerate-box.ron", "half_extents", "half extent"),
        ("unsorted-keys.ron", "keys", "ascending"),
    ];

    for (fixture_name, expected_field, expected_message) in cases {
        let text = fs::read_to_string(fixture(fixture_name)).expect("fixture should be readable");
        let path = format!("assets/source/{fixture_name}");
        let error = parse_source(&text, &path).expect_err("fixture should fail invariant checks");
        let AssetGenError::Invalid {
            path: reported,
            field,
            message,
        } = &error
        else {
            panic!("expected an invariant error for {fixture_name}, got {error:?}");
        };
        assert_eq!(reported, &path);
        assert!(
            field.contains(expected_field),
            "{fixture_name}: field {field} should mention {expected_field}"
        );
        assert!(
            message.contains(expected_message),
            "{fixture_name}: message {message} should mention {expected_message}"
        );
    }
}

fn fixture(name: &str) -> PathBuf {
    repo_root().join("tests/fixtures/assetgen").join(name)
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

#[test]
fn generation_is_byte_identical_across_separate_temp_roots() {
    let first = temp_root("assetgen-first");
    let second = temp_root("assetgen-second");
    let written_first = write_assets(&repo_root(), first.path()).expect("first write should pass");
    let written_second =
        write_assets(&repo_root(), second.path()).expect("second write should pass");

    assert_eq!(written_first.len(), ASSET_NAMES.len());
    assert_eq!(written_second.len(), ASSET_NAMES.len());

    for name in ASSET_NAMES {
        let a = fs::read(first.path().join(format!("{name}.glb"))).expect("first output");
        let b = fs::read(second.path().join(format!("{name}.glb"))).expect("second output");
        assert_eq!(a, b, "{name}.glb differs between independent temp roots");
        assert_eq!(
            a,
            committed_bytes(name),
            "{name}.glb committed copy is stale"
        );
    }
}

#[test]
fn check_accepts_the_committed_assets() {
    let report = check_assets(&repo_root()).expect("committed assets should be current");
    assert_eq!(report.checked.len(), ASSET_NAMES.len());
    for name in ASSET_NAMES {
        assert!(report.checked.iter().any(|entry| entry == name));
    }
}

#[test]
fn check_rejects_a_stale_generated_asset() {
    let root = temp_root("assetgen-stale");
    copy_pipeline(&repo_root(), root.path());
    let target = root.path().join(GENERATED_DIR).join("rack.glb");
    let mut bytes = fs::read(&target).expect("copied asset should be readable");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    fs::write(&target, bytes).expect("stale asset should be writable");

    let error = check_assets(root.path()).expect_err("a stale asset must fail the check");
    let AssetGenError::Stale { path, .. } = &error else {
        panic!("expected a stale error, got {error:?}");
    };
    assert!(
        path.ends_with("rack.glb"),
        "stale error should name the file"
    );
}

#[test]
fn generated_json_contains_no_host_or_time_data() {
    let mut forbidden = vec![
        "/Users/".to_owned(),
        "/home/".to_owned(),
        "/tmp/".to_owned(),
        "C:\\".to_owned(),
        "\\Users\\".to_owned(),
        "extras".to_owned(),
        repo_root().to_string_lossy().into_owned(),
    ];
    for variable in ["USER", "USERNAME", "LOGNAME", "HOSTNAME", "HOME"] {
        if let Ok(value) = std::env::var(variable)
            && value.len() > 3
        {
            forbidden.push(value);
        }
    }

    for name in ASSET_NAMES {
        let loaded = asset(name);
        let json = &loaded.json;
        for entry in &forbidden {
            assert!(!json.contains(entry), "{name}.glb JSON leaks {entry:?}");
        }
        assert!(
            !contains_iso_date(json),
            "{name}.glb JSON embeds a calendar date"
        );
        let metadata = &loaded.document.as_json().asset;
        assert_eq!(metadata.generator.as_deref(), Some(GENERATOR_NAME));
        assert_eq!(metadata.version, "2.0");
        assert!(metadata.copyright.is_none());
        assert!(metadata.min_version.is_none());
    }
}

fn contains_iso_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.windows(10).any(|window| {
        window[..4].iter().all(u8::is_ascii_digit)
            && window[4] == b'-'
            && window[5..7].iter().all(u8::is_ascii_digit)
            && window[7] == b'-'
            && window[8..].iter().all(u8::is_ascii_digit)
    })
}

fn copy_pipeline(from: &Path, to: &Path) {
    for directory in [SOURCE_DIR, GENERATED_DIR] {
        let target = to.join(directory);
        fs::create_dir_all(&target).expect("temp pipeline directory should be creatable");
        for entry in fs::read_dir(from.join(directory)).expect("pipeline directory should exist") {
            let entry = entry.expect("directory entry should be readable");
            fs::copy(entry.path(), target.join(entry.file_name())).expect("copy should succeed");
        }
    }
}

// ---------------------------------------------------------------------------
// Structural contract shared by every asset
// ---------------------------------------------------------------------------

#[test]
fn every_asset_exposes_named_scenes_matching_its_modules() {
    let expected = ASSET_MODULES.iter().copied().collect::<BTreeMap<_, _>>();

    for name in ASSET_NAMES {
        let loaded = asset(name);
        let modules = expected
            .get(name)
            .unwrap_or_else(|| panic!("no expected modules declared for {name}"));
        assert_eq!(
            scene_names(&loaded),
            modules.iter().map(|m| (*m).to_owned()).collect::<Vec<_>>(),
            "{name} scene names"
        );
        for module in modules.iter() {
            let node = node_by_name(&loaded, module);
            assert!(
                node.mesh().is_some() || node.children().count() > 0,
                "{module} root must carry geometry or children"
            );
        }
    }
}

#[test]
fn every_asset_uses_only_approved_palette_materials() {
    let approved = approved_material_names();
    for name in ASSET_NAMES {
        let loaded = asset(name);
        let names = material_names(&loaded);
        assert!(!names.is_empty(), "{name} should declare materials");
        let unique = names.iter().cloned().collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), names.len(), "{name} duplicates a material");
        for material_name in &names {
            assert!(
                approved.contains(material_name),
                "{name} uses unapproved material {material_name}"
            );
        }
        for material in loaded.document.materials() {
            let role = PaletteRole::ALL
                .iter()
                .find(|role| format!("{role:?}") == material.name().unwrap_or_default())
                .expect("material name should map to a palette role");
            let color = role.color();
            let factor = material.pbr_metallic_roughness().base_color_factor();
            for (actual, expected) in
                factor
                    .iter()
                    .zip([color.red, color.green, color.blue, color.alpha])
            {
                assert!(
                    (actual - expected).abs() <= 1e-5,
                    "{name}/{material_name:?} base color drifted",
                    material_name = material.name()
                );
            }
            assert!(material.unlit(), "cel-shift materials must be unlit");
            assert_eq!(material.pbr_metallic_roughness().metallic_factor(), 0.0);
            assert_eq!(material.pbr_metallic_roughness().roughness_factor(), 1.0);
        }
    }
}

#[test]
fn every_primitive_is_finite_indexed_and_bounded() {
    for name in ASSET_NAMES {
        let loaded = asset(name);
        assert_eq!(
            loaded.document.textures().count(),
            0,
            "{name} uses textures"
        );
        assert_eq!(loaded.document.images().count(), 0, "{name} embeds images");

        let triangles = triangle_count(&loaded);
        assert!(triangles > 0, "{name} generated no triangles");
        assert!(
            triangles <= MAX_TRIANGLES_PER_ASSET,
            "{name} has {triangles} triangles, above the {MAX_TRIANGLES_PER_ASSET} budget"
        );

        for mesh in loaded.document.meshes() {
            let mesh_name = mesh.name().unwrap_or_default().to_owned();
            let primitives = mesh.primitives().count();
            assert!(primitives > 0, "{mesh_name} has no primitives");
            assert!(
                primitives <= MAX_PRIMITIVES_PER_MESH,
                "{mesh_name} has {primitives} primitives, above the merge budget"
            );

            let mut seen_materials = BTreeSet::new();
            for primitive in mesh.primitives() {
                assert_eq!(primitive.mode(), gltf::mesh::Mode::Triangles);
                let material = primitive
                    .material()
                    .name()
                    .expect("primitive material must be named")
                    .to_owned();
                assert!(
                    seen_materials.insert(material.clone()),
                    "{mesh_name} did not merge geometry for {material}"
                );

                let reader = primitive.reader(|buffer| Some(&loaded.buffers[buffer.index()]));
                let positions = reader
                    .read_positions()
                    .expect("POSITION is required")
                    .collect::<Vec<_>>();
                let normals = reader
                    .read_normals()
                    .expect("NORMAL is required")
                    .collect::<Vec<_>>();
                let indices = reader
                    .read_indices()
                    .expect("indices are required")
                    .into_u32()
                    .collect::<Vec<_>>();

                assert_eq!(positions.len(), normals.len());
                assert!(
                    positions.len() <= MAX_VERTICES_PER_PRIMITIVE,
                    "{mesh_name}/{material} exceeds the vertex budget"
                );
                assert_eq!(indices.len() % 3, 0);
                for index in &indices {
                    assert!(
                        (*index as usize) < positions.len(),
                        "{mesh_name}/{material} has an out-of-range index"
                    );
                }
                for position in &positions {
                    for component in position {
                        assert!(
                            component.is_finite() && component.abs() < 1_000.0,
                            "{mesh_name}/{material} has an unbounded position"
                        );
                    }
                }
                for normal in &normals {
                    let length =
                        (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2])
                            .sqrt();
                    assert!(
                        (length - 1.0).abs() <= 1e-3,
                        "{mesh_name}/{material} has a non-unit normal"
                    );
                }

                let bounds = primitive.bounding_box();
                for axis in 0..3 {
                    assert!(bounds.min[axis].is_finite() && bounds.max[axis].is_finite());
                    assert!(bounds.min[axis] <= bounds.max[axis]);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Technician rig contract
// ---------------------------------------------------------------------------

#[test]
fn technician_exposes_the_eleven_bone_rig() {
    let loaded = asset("technician");
    assert_eq!(TECHNICIAN_BONES.len(), 11);

    let root = node_by_name(&loaded, "technician");
    let children = root
        .children()
        .map(|child| child.name().unwrap_or_default().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(children, vec!["technician-skin", "bone-hips"]);

    let skin_node = node_by_name(&loaded, "technician-skin");
    let skin = skin_node.skin().expect("technician-skin must carry a skin");
    let joints = skin
        .joints()
        .map(|joint| joint.name().unwrap_or_default().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        joints,
        TECHNICIAN_BONES
            .iter()
            .map(|b| (*b).to_owned())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        skin.skeleton().and_then(|node| node.name()),
        Some("bone-hips")
    );

    let reader = skin.reader(|buffer| Some(&loaded.buffers[buffer.index()]));
    let matrices = reader
        .read_inverse_bind_matrices()
        .expect("inverse bind matrices are required")
        .collect::<Vec<_>>();
    assert_eq!(matrices.len(), TECHNICIAN_BONES.len());
    for matrix in &matrices {
        for column in matrix {
            for value in column {
                assert!(value.is_finite());
            }
        }
    }

    let mut hierarchy = BTreeMap::new();
    for node in loaded.document.nodes() {
        for child in node.children() {
            hierarchy.insert(
                child.name().unwrap_or_default().to_owned(),
                node.name().unwrap_or_default().to_owned(),
            );
        }
    }
    for bone in TECHNICIAN_BONES {
        assert!(
            bone == "bone-hips" || hierarchy.contains_key(bone),
            "{bone} must be parented inside the rig"
        );
    }
    assert_eq!(
        hierarchy.get("bone-hips").map(String::as_str),
        Some("technician")
    );
}

#[test]
fn technician_skin_weights_are_rigid_and_in_range() {
    let loaded = asset("technician");
    let skin_node = node_by_name(&loaded, "technician-skin");
    let mesh = skin_node.mesh().expect("technician-skin must carry a mesh");
    let joint_count = skin_node.skin().expect("skin").joints().count();

    let mut vertices = 0usize;
    for primitive in mesh.primitives() {
        let reader = primitive.reader(|buffer| Some(&loaded.buffers[buffer.index()]));
        let joints = reader
            .read_joints(0)
            .expect("JOINTS_0 is required")
            .into_u16()
            .collect::<Vec<_>>();
        let weights = reader
            .read_weights(0)
            .expect("WEIGHTS_0 is required")
            .into_f32()
            .collect::<Vec<_>>();
        assert_eq!(joints.len(), weights.len());
        vertices += joints.len();

        for (joint, weight) in joints.iter().zip(&weights) {
            assert_eq!(weight, &[1.0, 0.0, 0.0, 0.0], "weights must be rigid");
            assert!(
                (joint[0] as usize) < joint_count,
                "joint index out of range: {}",
                joint[0]
            );
            assert_eq!(&joint[1..], &[0, 0, 0]);
        }
    }
    assert!(vertices > 0, "technician skin has no vertices");
}

#[test]
fn technician_declares_the_three_required_clips() {
    let loaded = asset("technician");
    let clips = loaded
        .document
        .animations()
        .map(|animation| animation.name().unwrap_or_default().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        clips,
        TECHNICIAN_CLIPS
            .iter()
            .map(|c| (*c).to_owned())
            .collect::<Vec<_>>()
    );
    assert_eq!(clips, vec!["Idle", "Walk", "Repair"]);

    let bones = TECHNICIAN_BONES.iter().copied().collect::<BTreeSet<_>>();
    for animation in loaded.document.animations() {
        let name = animation.name().unwrap_or_default().to_owned();
        let mut channels = 0usize;
        let mut targets = BTreeSet::new();
        for channel in animation.channels() {
            channels += 1;
            let target = channel
                .target()
                .node()
                .name()
                .unwrap_or_default()
                .to_owned();
            assert!(bones.contains(target.as_str()), "{name} targets {target}");
            let property = channel.target().property();
            assert!(
                matches!(
                    property,
                    gltf::animation::Property::Rotation | gltf::animation::Property::Translation
                ),
                "{name} uses an unsupported channel"
            );
            assert!(
                targets.insert((target.clone(), format!("{property:?}"))),
                "{name} declares a duplicate channel for {target}"
            );

            let sampler = channel.sampler();
            assert_eq!(
                sampler.interpolation(),
                gltf::animation::Interpolation::Linear
            );
            let reader = channel.reader(|buffer| Some(&loaded.buffers[buffer.index()]));
            let inputs = reader.read_inputs().expect("inputs").collect::<Vec<_>>();
            assert!(inputs.len() >= 2, "{name}/{target} needs keyframes");
            for window in inputs.windows(2) {
                assert!(window[0] < window[1], "{name}/{target} keys must ascend");
            }
            assert_eq!(inputs[0], 0.0, "{name}/{target} must start at zero");

            let outputs = reader.read_outputs().expect("outputs");
            let count = match outputs {
                gltf::animation::util::ReadOutputs::Rotations(values) => {
                    let values = values.into_f32().collect::<Vec<_>>();
                    for quaternion in &values {
                        let length = quaternion.iter().map(|v| v * v).sum::<f32>().sqrt();
                        assert!(
                            (length - 1.0).abs() <= 1e-3,
                            "{name}/{target} has a non-unit quaternion"
                        );
                    }
                    values.len()
                }
                gltf::animation::util::ReadOutputs::Translations(values) => {
                    let values = values.collect::<Vec<_>>();
                    for translation in &values {
                        assert!(translation.iter().all(|v| v.is_finite()));
                    }
                    values.len()
                }
                _ => panic!("{name}/{target} uses an unsupported output"),
            };
            assert_eq!(count, inputs.len(), "{name}/{target} sampler length");
        }
        assert!(channels >= 4, "{name} should animate the whole silhouette");
    }
}

#[test]
fn technician_has_adult_proportions_and_authored_palette() {
    let loaded = asset("technician");
    let (min, max) = technician_rest_bounds(&loaded);

    let height = max[1] - min[1];
    assert!(
        (1.70..=2.00).contains(&height),
        "technician height {height} is not adult"
    );
    assert!(
        min[1].abs() <= 0.02,
        "technician must stand on the floor, got {}",
        min[1]
    );
    let width = max[0] - min[0];
    assert!(
        (0.40..=0.80).contains(&width),
        "technician width {width} is implausible"
    );
    let depth = max[2] - min[2];
    assert!(
        (0.20..=0.60).contains(&depth),
        "technician depth {depth} is implausible"
    );

    let roles = material_names(&loaded).into_iter().collect::<BTreeSet<_>>();
    for required in [
        PaletteRole::WorkerHardHat,
        PaletteRole::WorkerHiVis,
        PaletteRole::WorkerSlate,
        PaletteRole::WorkerTrousers,
        PaletteRole::WorkerBoots,
        PaletteRole::WorkerSkin,
        PaletteRole::Ink,
    ] {
        assert!(
            roles.contains(&format!("{required:?}")),
            "technician is missing {required:?}"
        );
    }
}

/// Rebuilds model space bounds for the skinned technician. Vertices are stored
/// relative to their bone, so the rest pose is the stored position plus the
/// bone origin recovered from the inverse bind matrix.
fn technician_rest_bounds(loaded: &Loaded) -> ([f32; 3], [f32; 3]) {
    let skin_node = node_by_name(loaded, "technician-skin");
    let skin = skin_node.skin().expect("skin");
    let origins = skin
        .reader(|buffer| Some(&loaded.buffers[buffer.index()]))
        .read_inverse_bind_matrices()
        .expect("inverse bind matrices")
        .map(|matrix| [-matrix[3][0], -matrix[3][1], -matrix[3][2]])
        .collect::<Vec<_>>();

    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for primitive in skin_node.mesh().expect("mesh").primitives() {
        let reader = primitive.reader(|buffer| Some(&loaded.buffers[buffer.index()]));
        let positions = reader.read_positions().expect("positions");
        let joints = reader
            .read_joints(0)
            .expect("joints")
            .into_u16()
            .collect::<Vec<_>>();
        for (index, position) in positions.enumerate() {
            let origin = origins[joints[index][0] as usize];
            for axis in 0..3 {
                let value = position[axis] + origin[axis];
                min[axis] = min[axis].min(value);
                max[axis] = max[axis].max(value);
            }
        }
    }
    (min, max)
}

// ---------------------------------------------------------------------------
// Rack and equipment contract
// ---------------------------------------------------------------------------

#[test]
fn rack_row_is_merged_and_readable() {
    let loaded = asset("rack");
    assert_eq!(
        loaded.document.nodes().count(),
        1,
        "rack slots and lights must not be separate nodes"
    );
    let node = node_by_name(&loaded, "rack-row");
    let mesh = node.mesh().expect("rack-row must carry merged geometry");
    assert_eq!(mesh.name(), Some("rack-row-mesh"));

    let roles = mesh
        .primitives()
        .map(|primitive| primitive.material().name().unwrap_or_default().to_owned())
        .collect::<BTreeSet<_>>();
    for required in [
        PaletteRole::RackWhite,
        PaletteRole::RackShadow,
        PaletteRole::Ink,
        PaletteRole::HealthyGreen,
        PaletteRole::FloorShadow,
    ] {
        assert!(
            roles.contains(&format!("{required:?}")),
            "rack is missing {required:?}"
        );
    }

    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    let mut body = None;
    for primitive in mesh.primitives() {
        let bounds = primitive.bounding_box();
        if primitive.material().name() == Some(&format!("{:?}", PaletteRole::RackWhite)) {
            body = Some(bounds.clone());
        }
        for axis in 0..3 {
            min[axis] = min[axis].min(bounds.min[axis]);
            max[axis] = max[axis].max(bounds.max[axis]);
        }
    }
    let body = body.expect("the rack must expose a lit body primitive");
    assert!(
        (1.4..=1.7).contains(&(body.max[0] - body.min[0])),
        "rack cabinet width {} must match the 1.5m collider",
        body.max[0] - body.min[0]
    );
    assert!(
        (15.9..=16.1).contains(&(body.max[2] - body.min[2])),
        "rack cabinet length {} must match the 16m collider",
        body.max[2] - body.min[2]
    );
    assert!(
        (15.9..=16.4).contains(&(max[2] - min[2])),
        "rack row length {} must stay inside the 16m footprint",
        max[2] - min[2]
    );
    assert!(
        (2.0..=2.6).contains(&max[1]),
        "rack height {} is implausible",
        max[1]
    );

    assert!(
        triangle_count(&loaded) > 1_000,
        "the rack should be deliberately authored, not a bare box"
    );
}

#[test]
fn equipment_and_props_are_authored_with_the_expected_shape() {
    let cooling = asset("cooling-unit");
    let cooling_mesh = node_by_name(&cooling, "cooling-unit").mesh().expect("mesh");
    let cooling_roles = cooling_mesh
        .primitives()
        .map(|p| p.material().name().unwrap_or_default().to_owned())
        .collect::<BTreeSet<_>>();
    for required in [
        PaletteRole::RackWhite,
        PaletteRole::TealAccent,
        PaletteRole::Ink,
    ] {
        assert!(cooling_roles.contains(&format!("{required:?}")));
    }

    let props = asset("utility-props");
    assert_eq!(scene_names(&props), vec!["utility-cart", "step-stool"]);
    let cart_roles = node_by_name(&props, "utility-cart")
        .mesh()
        .expect("cart mesh")
        .primitives()
        .map(|p| p.material().name().unwrap_or_default().to_owned())
        .collect::<BTreeSet<_>>();
    assert!(cart_roles.contains(&format!("{:?}", PaletteRole::FaultRed)));
    let stool_roles = node_by_name(&props, "step-stool")
        .mesh()
        .expect("stool mesh")
        .primitives()
        .map(|p| p.material().name().unwrap_or_default().to_owned())
        .collect::<BTreeSet<_>>();
    assert!(stool_roles.contains(&format!("{:?}", PaletteRole::SignatureYellow)));

    let infrastructure = asset("infrastructure");
    assert_eq!(
        scene_names(&infrastructure),
        vec!["overhead-tray", "hose-drop"]
    );
    let hose_roles = node_by_name(&infrastructure, "hose-drop")
        .mesh()
        .expect("hose mesh")
        .primitives()
        .map(|p| p.material().name().unwrap_or_default().to_owned())
        .collect::<BTreeSet<_>>();
    assert!(hose_roles.contains(&format!("{:?}", PaletteRole::HoseCharcoal)));
}

// ---------------------------------------------------------------------------
// Pipeline autonomy
// ---------------------------------------------------------------------------

#[test]
fn generation_reads_only_repository_owned_declarative_sources() {
    let assets = generate_assets(&repo_root()).expect("generation should succeed");
    assert_eq!(assets.len(), ASSET_NAMES.len());
    for asset in &assets {
        assert!(asset.file_name.ends_with(".glb"));
        assert!(!asset.bytes.is_empty());
        assert_eq!(&asset.bytes[0..4], b"glTF");
    }
}

#[test]
fn no_external_authoring_tool_is_referenced_anywhere() {
    let root = repo_root();
    let mut pipeline_files = vec![
        root.join("src/assetgen.rs"),
        root.join("src/bin/assetgen.rs"),
        root.join("Cargo.toml"),
    ];
    for directory in ["assets", "scripts", ".github"] {
        let directory = root.join(directory);
        if directory.exists() {
            pipeline_files.extend(walk(&directory));
        }
    }

    let mut checked = 0usize;
    for entry in &pipeline_files {
        let Some(extension) = entry.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        if !matches!(extension, "rs" | "ron" | "toml" | "yml" | "yaml" | "sh") {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry) else {
            continue;
        };
        checked += 1;
        let lowered = text.to_lowercase();
        for forbidden in ["blender", "maya", "3ds max", "substance painter", "houdini"] {
            assert!(
                !lowered.contains(forbidden),
                "{entry:?} references the external authoring tool {forbidden:?}"
            );
        }
    }
    assert!(checked > 5, "the autonomy scan inspected too few files");

    for pipeline_file in ["src/assetgen.rs", "src/bin/assetgen.rs"] {
        let text = fs::read_to_string(root.join(pipeline_file)).expect("pipeline source");
        for forbidden in ["Command", "reqwest", "download", "http://", "https://"] {
            assert!(
                !text.contains(forbidden),
                "{pipeline_file} must not shell out or fetch remote data ({forbidden})"
            );
        }
    }
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if matches!(name.as_str(), ".git" | "target" | "node_modules") {
                    continue;
                }
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}
