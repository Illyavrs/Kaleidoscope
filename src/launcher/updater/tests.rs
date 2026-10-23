use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};
use zip::write::SimpleFileOptions;

fn fixture() -> (Trust, Ed25519KeyPair, Manifest) {
    // In-memory, deterministic unit fixture; never used as publisher trust.
    let pair = Ed25519KeyPair::from_seed_unchecked(&[71; 32]).unwrap();
    let trust = Trust {
        repository: "example/Kaleidoscope".into(),
        key: Some(pair.public_key().as_ref().try_into().unwrap()),
        version: Version::new(1, 0, 0),
    };
    let files = PACKAGE_FILES
        .into_iter()
        .map(|name| (name.to_owned(), hash_bytes(name.as_bytes())))
        .collect::<BTreeMap<_, _>>();
    let manifest = Manifest {
        protocol: 1,
        version: "1.1.0".into(),
        platform: "windows-x64".into(),
        repository: trust.repository.clone(),
        tag: "v1.1.0".into(),
        asset: "Kaleidoscope-1.1.0-windows-x64.zip".into(),
        bytes: 100,
        sha256: hash_bytes(b"archive"),
        launcher_sha256: files["Kaleidoscope.exe"].clone(),
        bridge_sha256: files[BRIDGE].clone(),
        kaleidoscope_commit: "1".repeat(40),
        transfiguration_commit: "2".repeat(40),
        files,
        bridge_version: "0.1.0".into(),
    };
    (trust, pair, manifest)
}

fn signed(pair: &Ed25519KeyPair, manifest: &Manifest) -> Vec<u8> {
    let payload = serde_json::to_vec(manifest).unwrap();
    serde_json::to_vec(&Envelope {
        manifest: STANDARD.encode(&payload),
        signature: STANDARD.encode(pair.sign(&payload).as_ref()),
    })
    .unwrap()
}

#[test]
fn signed_manifest_binds_exact_bundle_source_versions_and_components() {
    let (trust, pair, manifest) = fixture();
    let (_, actual) = verify_manifest(&signed(&pair, &manifest), &trust).unwrap();
    assert_eq!(actual.bridge_sha256, manifest.bridge_sha256);
    assert_eq!(actual.kaleidoscope_commit, "1".repeat(40));
    assert_eq!(actual.transfiguration_commit, "2".repeat(40));
    assert_eq!(actual.files.len(), 17);
    let mut tampered: Envelope = serde_json::from_slice(&signed(&pair, &manifest)).unwrap();
    let mut payload = STANDARD.decode(&tampered.manifest).unwrap();
    payload[0] ^= 1;
    tampered.manifest = STANDARD.encode(payload);
    assert!(verify_manifest(&serde_json::to_vec(&tampered).unwrap(), &trust).is_err());
    let mut foreign = trust.clone();
    foreign.key = Some([9; 32]);
    assert!(verify_manifest(&signed(&pair, &manifest), &foreign).is_err());
}

#[test]
fn signed_manifest_rejects_downgrades_mixed_components_foreign_sources_and_extra_files() {
    let (trust, pair, original) = fixture();
    let cases: [fn(&mut Manifest); 9] = [
        |value| value.version = "1.0.0".into(),
        |value| value.version = "1.1.0-beta.1".into(),
        |value| value.repository = "other/Kaleidoscope".into(),
        |value| value.tag = "v9.0.0".into(),
        |value| value.asset = "../Kaleidoscope.exe".into(),
        |value| value.bridge_sha256 = "0".repeat(64),
        |value| value.bytes = MAX_PACKAGE + 1,
        |value| {
            value.files.insert("../user.txt".into(), "0".repeat(64));
        },
        |value| {
            value.files.remove("LICENSE");
        },
    ];
    for change in cases {
        let mut value = original.clone();
        change(&mut value);
        assert!(verify_manifest(&signed(&pair, &value), &trust).is_err());
    }
}

#[test]
fn repository_and_hash_configuration_cannot_be_urls_or_partial_values() {
    assert!(valid_repository("owner/Kaleidoscope"));
    for value in [
        "https://github.com/owner/Kaleidoscope",
        "../repo",
        "owner/repo/extra",
        "owner/repo.git",
        "/repo",
        "owner/",
        "own er/repo",
    ] {
        assert!(!valid_repository(value), "{value}");
    }
    for value in ["a", &"A".repeat(64), &"0".repeat(63), &"g".repeat(64)] {
        assert!(hex_bytes::<32>(value).is_err());
    }
}

#[test]
fn github_asset_links_and_duplicates_are_rejected_before_download() {
    let asset = |url: &str| Asset {
        name: MANIFEST_ASSET.into(),
        browser_download_url: url.into(),
        size: 20,
        digest: None,
    };
    let mut release = Release {
        tag_name: "v1.1.0".into(),
        draft: false,
        prerelease: false,
        assets: vec![asset(
            "https://github.com/example/Kaleidoscope/releases/download/v1.1.0/updates.json",
        )],
    };
    assert!(release_asset(&release, "example/Kaleidoscope", MANIFEST_ASSET).is_ok());
    release
        .assets
        .push(asset(&release.assets[0].browser_download_url));
    assert!(release_asset(&release, "example/Kaleidoscope", MANIFEST_ASSET).is_err());
    release.assets = vec![asset("https://evil.test/updates.json")];
    assert!(release_asset(&release, "example/Kaleidoscope", MANIFEST_ASSET).is_err());
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("kaleidoscope-updater-tests-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(fs::canonicalize(root).unwrap())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let root = fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            root.parent(),
            Some(fs::canonicalize(std::env::temp_dir()).unwrap().as_path())
        );
        assert!(root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("kaleidoscope-updater-tests-"));
        fs::remove_dir_all(root).unwrap();
    }
}

fn archive(names: &[&str]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for name in names {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(name.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[test]
fn extraction_checks_every_file_and_rejects_foreign_paths_without_overwrite() {
    let scratch = Scratch::new();
    let (_, _, manifest) = fixture();
    let payload = scratch.0.join("payload");
    fs::create_dir(&payload).unwrap();
    extract_package(&archive(&PACKAGE_FILES), &payload, &manifest).unwrap();
    for name in PACKAGE_FILES {
        assert_eq!(fs::read(payload.join(name)).unwrap(), name.as_bytes());
    }
    assert!(extract_package(&archive(&PACKAGE_FILES), &payload, &manifest).is_err());
    let invalid = scratch.0.join("invalid");
    fs::create_dir(&invalid).unwrap();
    let mut names = PACKAGE_FILES;
    names[0] = "../outside.txt";
    assert!(extract_package(&archive(&names), &invalid, &manifest).is_err());
    assert!(!scratch.0.join("outside.txt").exists());
}

fn owned_package(root: &Path) -> BTreeMap<String, String> {
    fs::create_dir(root).unwrap();
    fs::create_dir(root.join("licenses")).unwrap();
    for name in PACKAGE_FILES
        .into_iter()
        .filter(|name| *name != "package.json")
    {
        fs::write(root.join(name), name).unwrap();
    }
    let bridge_hash = hash_file(&root.join(BRIDGE)).unwrap();
    fs::write(
        root.join("artifact.json"),
        serde_json::to_vec(&serde_json::json!({
            "fixture": false, "artifact": BRIDGE, "sha256": bridge_hash,
        }))
        .unwrap(),
    )
    .unwrap();
    let content: BTreeMap<_, _> = PACKAGE_FILES
        .into_iter()
        .filter(|name| *name != "package.json")
        .map(|name| (name, hash_file(&root.join(name)).unwrap()))
        .collect();
    fs::write(root.join("package.json"), serde_json::to_vec(&serde_json::json!({
        "version": 1, "status": "complete", "fixture": false, "embedded_interface_verified": true,
        "verification_kind": "embedded_interface_command", "build_profile": "debug",
        "bridge_sha256": bridge_hash, "content_sha256": content,
    })).unwrap()).unwrap();
    verify_owned_package(root).unwrap()
}

#[test]
fn installation_refuses_directories_containing_user_files_or_partial_bundles() {
    let scratch = Scratch::new();
    let bundle = scratch.0.join("bundle");
    let original = owned_package(&bundle);
    assert_eq!(verify_owned_package(&bundle).unwrap(), original);
    fs::write(bundle.join("personal-photo.png"), b"must stay").unwrap();
    assert!(verify_owned_package(&bundle).is_err());
    assert_eq!(
        fs::read(bundle.join("personal-photo.png")).unwrap(),
        b"must stay"
    );
    fs::remove_file(bundle.join("personal-photo.png")).unwrap();
    fs::write(bundle.join(BRIDGE), b"wrong component").unwrap();
    assert!(verify_owned_package(&bundle).is_err());
}

#[test]
fn moved_bundle_identity_and_content_are_checked_on_the_directory_actually_installed() {
    let scratch = Scratch::new();
    let payload = scratch.0.join("payload");
    let expected = owned_package(&payload);
    let identity = install::directory_identity(&payload).unwrap();
    let target = scratch.0.join("installed");
    fs::rename(&payload, &target).unwrap();
    let guards = verify_moved_package(&target, &identity, &expected).unwrap();
    drop(guards);
    let foreign = scratch.0.join("foreign");
    owned_package(&foreign);
    assert!(verify_moved_package(&foreign, &identity, &expected).is_err());
    fs::write(target.join("README.txt"), b"changed during swap").unwrap();
    assert!(verify_moved_package(&target, &identity, &expected).is_err());
}

#[test]
fn sparse_oversized_files_fail_before_hashing_the_entire_file() {
    let scratch = Scratch::new();
    let path = scratch.0.join("huge");
    let file = File::create(&path).unwrap();
    file.set_len(MAX_FILE + 1).unwrap();
    drop(file);
    assert!(hash_file(&path).is_err());
}

#[test]
fn failed_startup_rollback_restores_exact_old_bundle_and_preserves_failed_payload() {
    let scratch = Scratch::new();
    let target = scratch.0.join("target");
    let payload = scratch.0.join("payload");
    let backup = scratch.0.join("previous");
    let original = owned_package(&target);
    owned_package(&payload);
    fs::write(
        payload.join("README.txt"),
        b"new version retained for inspection",
    )
    .unwrap();
    let mut package: serde_json::Value =
        serde_json::from_slice(&fs::read(payload.join("package.json")).unwrap()).unwrap();
    package["content_sha256"]["README.txt"] =
        hash_file(&payload.join("README.txt")).unwrap().into();
    fs::write(
        payload.join("package.json"),
        serde_json::to_vec(&package).unwrap(),
    )
    .unwrap();
    let updated = verify_owned_package(&payload).unwrap();
    assert_ne!(updated, original);
    fs::rename(&target, &backup).unwrap();
    fs::rename(&payload, &target).unwrap();
    for (error, confirmed_gone) in [
        ("spawn failed without creating any child", Ok(true)),
        (
            "owned Child handle confirmed exit before identity capture",
            Ok(true),
        ),
    ] {
        let stopped = stopped_before_identity(error.into(), confirmed_gone);
        assert_eq!(
            rollback_stopped_startup(stopped, &target, &payload, &backup).unwrap(),
            Some(error.into())
        );
        assert_eq!(verify_owned_package(&target).unwrap(), original);
        assert_eq!(verify_owned_package(&payload).unwrap(), updated);
        assert!(!backup.exists());
        fs::rename(&target, &backup).unwrap();
        fs::rename(&payload, &target).unwrap();
    }
    for confirmed_gone in [Ok(false), Err("Child lifetime cannot be inspected".into())] {
        let unknown =
            stopped_before_identity("Identity cannot be inspected".into(), confirmed_gone);
        assert!(rollback_stopped_startup(unknown, &target, &payload, &backup).is_err());
        assert_eq!(verify_owned_package(&target).unwrap(), updated);
        assert_eq!(verify_owned_package(&backup).unwrap(), original);
        assert!(!payload.exists());
    }
    // A malformed receipt exercises the same failure resolution as production,
    // using a pure close/observe adapter. No fixture ever starts an application.
    let malformed_ready = || {
        serde_json::from_slice::<process::Identity>(b"{")
            .map(|_| ())
            .map_err(|_| "新版啟動確認無效".into())
    };
    for state in [process::State::Live, process::State::Ambiguous] {
        let refused = resolve_startup(malformed_ready(), || Ok(state));
        assert!(rollback_stopped_startup(refused, &target, &payload, &backup).is_err());
        assert_eq!(verify_owned_package(&target).unwrap(), updated);
        assert_eq!(verify_owned_package(&backup).unwrap(), original);
        assert!(!payload.exists());
    }
    let foreign_identity = resolve_startup(malformed_ready(), || {
        Err("Process identity changed; no close message was sent".into())
    });
    assert!(rollback_stopped_startup(foreign_identity, &target, &payload, &backup).is_err());
    assert_eq!(verify_owned_package(&target).unwrap(), updated);
    assert_eq!(verify_owned_package(&backup).unwrap(), original);
    let close_observed = std::cell::Cell::new(false);
    let stopped = resolve_startup(malformed_ready(), || {
        close_observed.set(true);
        Ok(process::State::Gone)
    });
    assert!(close_observed.get());
    assert_eq!(
        rollback_stopped_startup(stopped, &target, &payload, &backup).unwrap(),
        Some("新版啟動確認無效".into())
    );
    assert_eq!(verify_owned_package(&target).unwrap(), original);
    assert_eq!(verify_owned_package(&payload).unwrap(), updated);
    assert!(!backup.exists());
    assert!(matches!(
        resolve_startup(Ok(()), || panic!("a ready application must remain running")),
        Ok(StartupOutcome::Ready)
    ));
}

#[test]
fn component_composition_keeps_launcher_and_user_independent_files_byte_exact() {
    let scratch = Scratch::new();
    let original = scratch.0.join("original");
    let old_files = owned_package(&original);
    let replacement = bridge_feed::BridgeManifest {
        protocol: 1,
        version: "0.1.1".into(),
        platform: "windows-x64".into(),
        repository: "example/Transfiguration".into(),
        tag: "v0.1.1".into(),
        asset: "Transfiguration-0.1.1-windows-x64.zip".into(),
        bytes: 1024,
        sha256: "a".repeat(64),
        bridge_sha256: "b".repeat(64),
        transfiguration_commit: "c".repeat(40),
        launcher_requirement: ">=0.1.0, <0.2.0".into(),
        files: bridge_feed::COMPONENT_FILES
            .into_iter()
            .map(|name| {
                (
                    name.to_owned(),
                    if name == BRIDGE {
                        "b".repeat(64)
                    } else {
                        "d".repeat(64)
                    },
                )
            })
            .collect(),
    };
    let bytes = updated_bridge_package(&original, &replacement).unwrap();
    let package: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        package["content_sha256"]["Kaleidoscope.exe"],
        old_files["Kaleidoscope.exe"]
    );
    assert_eq!(package["content_sha256"]["LICENSE"], old_files["LICENSE"]);
    assert_eq!(
        package["content_sha256"]["licenses/Nunito.txt"],
        old_files["licenses/Nunito.txt"]
    );
    assert_eq!(package["bridge_sha256"], replacement.bridge_sha256);
    for (name, hash) in &replacement.files {
        assert_eq!(package["content_sha256"][name], *hash);
    }
    assert_eq!(verify_owned_package(&original).unwrap(), old_files);
}

#[test]
fn launcher_updates_never_roll_back_an_independently_updated_bridge() {
    assert!(ensure_no_bridge_downgrade("0.1.0", "0.1.5").is_err());
    assert!(ensure_no_bridge_downgrade("0.1.5", "0.1.5").is_ok());
    assert!(ensure_no_bridge_downgrade("0.2.0", "0.1.5").is_ok());
    assert!(ensure_no_bridge_downgrade("0.2.0-beta.1", "0.1.5").is_err());
}

fn unsigned_fixture() -> (Trust, Manifest) {
    let (mut trust, _, mut manifest) = fixture();
    trust.key = None;
    manifest.asset = "Kaleidoscope-1.1.0-windows-x64-portable.zip".into();
    (trust, manifest)
}

fn unsigned_bytes(manifest: &Manifest) -> Vec<u8> {
    serde_json::to_vec(&unsigned_envelope(manifest).unwrap()).unwrap()
}

#[test]
fn unsigned_channel_accepts_only_unsigned_records_for_the_portable_asset() {
    let (trust, manifest) = unsigned_fixture();
    verify_manifest(&unsigned_bytes(&manifest), &trust).unwrap();
    // A signed build never accepts a record without a valid signature.
    let mut signed_trust = trust.clone();
    signed_trust.key = Some([7; 32]);
    assert!(verify_manifest(&unsigned_bytes(&manifest), &signed_trust).is_err());
    // An unsigned build never accepts signature material it cannot verify.
    let mut envelope = unsigned_envelope(&manifest).unwrap();
    envelope.signature = STANDARD.encode([0u8; 64]);
    assert!(verify_manifest(&serde_json::to_vec(&envelope).unwrap(), &trust).is_err());
    let cases: [fn(&mut Manifest); 7] = [
        |value| value.version = "1.0.0".into(),
        |value| value.version = "1.1.0-beta.1".into(),
        |value| value.repository = "other/Kaleidoscope".into(),
        |value| value.tag = "v9.0.0".into(),
        |value| value.asset = "Kaleidoscope-1.1.0-windows-x64.zip".into(),
        |value| value.bytes = MAX_PACKAGE + 1,
        |value| {
            value.files.insert("../user.txt".into(), "0".repeat(64));
        },
    ];
    for change in cases {
        let mut value = manifest.clone();
        change(&mut value);
        assert!(verify_manifest(&unsigned_bytes(&value), &trust).is_err());
    }
}

#[test]
fn github_asset_digest_must_be_a_lowercase_sha256() {
    let asset = |digest: Option<&str>| Asset {
        name: "a.zip".into(),
        browser_download_url: String::new(),
        size: 1,
        digest: digest.map(str::to_owned),
    };
    let hash = "a".repeat(64);
    assert_eq!(
        asset_digest(&asset(Some(&format!("sha256:{hash}")))).unwrap(),
        hash
    );
    for bad in [
        None,
        Some("sha256:abc"),
        Some(&format!("sha1:{hash}")[..]),
        Some(&format!("sha256:{}", "A".repeat(64))[..]),
        Some(&hash[..]),
    ] {
        assert!(asset_digest(&asset(bad)).is_err());
    }
}

#[test]
fn unsigned_extraction_requires_exactly_the_allowed_files_and_reports_their_hashes() {
    let scratch = Scratch::new();
    let payload = scratch.0.join("payload");
    fs::create_dir(&payload).unwrap();
    let computed = extract_files(&archive(&PACKAGE_FILES), &payload, None, &PACKAGE_FILES).unwrap();
    assert_eq!(computed.len(), PACKAGE_FILES.len());
    for name in PACKAGE_FILES {
        assert_eq!(computed[name], hash_bytes(name.as_bytes()));
    }
    let extra = scratch.0.join("extra");
    fs::create_dir(&extra).unwrap();
    let mut names = PACKAGE_FILES.to_vec();
    names.push("extra.dll");
    assert!(extract_files(&archive(&names), &extra, None, &PACKAGE_FILES).is_err());
    let missing = scratch.0.join("missing");
    fs::create_dir(&missing).unwrap();
    assert!(extract_files(&archive(&PACKAGE_FILES[1..]), &missing, None, &PACKAGE_FILES).is_err());
    let traversal = scratch.0.join("traversal");
    fs::create_dir(&traversal).unwrap();
    let mut names = PACKAGE_FILES;
    names[0] = "../outside.txt";
    assert!(extract_files(&archive(&names), &traversal, None, &PACKAGE_FILES).is_err());
    assert!(!scratch.0.join("outside.txt").exists());
}
