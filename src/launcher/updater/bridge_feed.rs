//! Independently signed bridge components. A persisted override is authoritative:
//! missing trust, invalid signatures, or changed component files fail closed.

use super::{
    hash_bytes, hash_file, hex_bytes, read_bounded, stable_version, valid_repository, Envelope,
};
use crate::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use ring::signature::{UnparsedPublicKey, ED25519};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

pub(super) const COMPONENT_FILES: [&str; 10] = [
    "osu.Game.Rulesets.Transfiguration.dll",
    "artifact.json",
    "compatibility.json",
    "THIRD-PARTY-NOTICES.md",
    "Transfiguration-README.md",
    "licenses/DotNet.txt",
    "licenses/Harmony.txt",
    "licenses/Iced.txt",
    "licenses/Mono.Cecil.txt",
    "licenses/MonoMod.txt",
];
pub(super) const INSTALLED_MANIFEST: &str = "bridge-update.json";
const MAX_JSON: u64 = 1024 * 1024;
const MAX_PACKAGE: u64 = 512 * 1024 * 1024;
const BRIDGE: &str = COMPONENT_FILES[0];
const NATIVE_ASSEMBLIES: [&str; 3] = ["osu.Game.dll", "osu.Framework.dll", "osu!.dll"];
const COMPATIBILITY_ASSEMBLIES: [&str; 5] = [
    "osu.Game.dll",
    "osu.Framework.dll",
    "osu!.dll",
    "System.Net.Http.dll",
    "System.Net.WebSockets.Client.dll",
];
const REQUIRED_CONTRACTS: [&str; 12] = [
    "OsuGameBase.CreateEndpoints",
    "Host.GetSuitableDesktopHost",
    "DesktopGameHost.GetDefaultGameStorage",
    "IniConfigManager<OsuSetting>.PerformLoad:auth-only stream overlay",
    "IniConfigManager<OsuSetting>.PerformSave:shared preferences and private auth partition",
    "NativeStorage.GetStream:scoped game.ini auth filtering",
    "Storage.CreateFileSafely:scoped native preference serialization",
    "OsuConfigManager.ctor(Storage):shared preferences and private auth before API",
    "APIAccess.ctor(OsuGameBase,OsuConfigManager,EndpointConfiguration,string)",
    "RulesetStore.addRuleset",
    "SocketsHttpHandler.SendAsync",
    "ClientWebSocket.ConnectAsync(Uri,CancellationToken)",
];

#[derive(Clone)]
struct Trust {
    repository: String,
    key: [u8; 32],
    bundled_version: Version,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct BridgeConfiguration {
    pub repository: String,
    pub public_key: String,
    pub bundled_version: String,
}

fn bundled_version() -> Result<Version> {
    stable_version(option_env!("TRANSFIGURATION_BUNDLED_VERSION").unwrap_or("0.1.0"))
}

fn compiled_trust() -> Result<Trust> {
    let repository = option_env!("TRANSFIGURATION_UPDATE_REPOSITORY")
        .filter(|value| valid_repository(value))
        .ok_or("此版本尚未設定 Transfiguration 的 GitHub 更新來源")?;
    let key = hex_bytes(
        option_env!("TRANSFIGURATION_UPDATE_PUBLIC_KEY")
            .ok_or("此版本尚未設定 Transfiguration 更新簽章公鑰")?,
    )?;
    if key == [0; 32] {
        return Err("Transfiguration 更新簽章公鑰無效".into());
    }
    Ok(Trust {
        repository: repository.to_owned(),
        key,
        bundled_version: bundled_version()?,
    })
}

pub(super) fn configuration() -> Result<BridgeConfiguration> {
    let trust = compiled_trust()?;
    Ok(BridgeConfiguration {
        repository: trust.repository,
        public_key: trust.key.iter().map(|byte| format!("{byte:02x}")).collect(),
        bundled_version: trust.bundled_version.to_string(),
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BridgeManifest {
    pub protocol: u8,
    pub version: String,
    pub platform: String,
    pub repository: String,
    pub tag: String,
    pub asset: String,
    pub bytes: u64,
    pub sha256: String,
    pub bridge_sha256: String,
    pub transfiguration_commit: String,
    pub launcher_requirement: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub(super) struct BridgeRelease {
    pub envelope: Envelope,
    pub manifest: BridgeManifest,
}

pub(super) fn parse_signed_manifest(bytes: &[u8], launcher_version: &str) -> Result<BridgeRelease> {
    parse_with_trust(bytes, launcher_version, &compiled_trust()?)
}

fn parse_with_trust(bytes: &[u8], launcher_version: &str, trust: &Trust) -> Result<BridgeRelease> {
    if bytes.len() as u64 > MAX_JSON {
        return Err("Transfiguration 更新資訊超過大小限制".into());
    }
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|_| "無法讀取 Transfiguration 更新簽章")?;
    let payload = STANDARD
        .decode(&envelope.manifest)
        .map_err(|_| "Transfiguration 更新簽章格式不正確")?;
    let signature = STANDARD
        .decode(&envelope.signature)
        .map_err(|_| "Transfiguration 更新簽章格式不正確")?;
    if payload.len() as u64 > MAX_JSON || signature.len() != 64 {
        return Err("Transfiguration 更新簽章格式不正確".into());
    }
    UnparsedPublicKey::new(&ED25519, trust.key)
        .verify(&payload, &signature)
        .map_err(|_| "Transfiguration 更新簽章驗證失敗；未安裝任何檔案")?;
    let manifest: BridgeManifest =
        serde_json::from_slice(&payload).map_err(|_| "Transfiguration 更新資訊格式不正確")?;
    let version = stable_version(&manifest.version)?;
    let launcher = stable_version(launcher_version)?;
    let requirement = if manifest.launcher_requirement.len() <= 128
        && !manifest.launcher_requirement.trim().is_empty()
    {
        VersionReq::parse(&manifest.launcher_requirement)
            .map_err(|_| "Transfiguration 的啟動器相容版本格式不正確")?
    } else {
        return Err("Transfiguration 的啟動器相容版本格式不正確".into());
    };
    if !requirement.matches(&launcher) {
        return Err("新版 Transfiguration 需要更新 Kaleidoscope；請先更新啟動器".into());
    }
    if manifest.protocol != 1
        || manifest.platform != "windows-x64"
        || manifest.repository != trust.repository
        || version <= trust.bundled_version
        || manifest.tag != format!("v{version}")
        || manifest.asset != format!("Transfiguration-{version}-windows-x64.zip")
        || manifest.bytes == 0
        || manifest.bytes > MAX_PACKAGE
        || manifest
            .files
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != COMPONENT_FILES.into_iter().collect()
    {
        return Err("Transfiguration 更新來源、版本或套件內容不符合此啟動器".into());
    }
    for hash in manifest
        .files
        .values()
        .chain([&manifest.sha256, &manifest.bridge_sha256])
    {
        hex_bytes::<32>(hash)?;
    }
    hex_bytes::<20>(&manifest.transfiguration_commit)?;
    if manifest.files.get(BRIDGE) != Some(&manifest.bridge_sha256) {
        return Err("Transfiguration 簽章與連線模組雜湊不一致".into());
    }
    Ok(BridgeRelease { envelope, manifest })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    protocol: u8,
    artifact: String,
    sha256: String,
    default_endpoint: String,
    endpoint_mode: String,
    configurable_endpoint: bool,
    private_auth_filename: String,
    shared_game_preferences: bool,
    fixture: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeAssembly {
    version: String,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompatibilityMetadata {
    protocol: u8,
    artifact: String,
    default_endpoint: String,
    endpoint_mode: String,
    configurable_endpoint: bool,
    private_auth_filename: String,
    shared_game_preferences: bool,
    fixture: bool,
    platform: String,
    native_client_version: String,
    runtime: String,
    ruleset_api: String,
    harmony: String,
    assemblies: BTreeMap<String, NativeAssembly>,
    contracts: Vec<String>,
    verification: BTreeMap<String, String>,
}

fn four_part_version(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 4
        && parts.into_iter().all(|part| {
            !part.is_empty()
                && (part.len() == 1 || !part.starts_with('0'))
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && part.parse::<u32>().is_ok()
        })
}

/// Read and authenticate the exact metadata bytes used to select native pins.
/// Re-reading a path after an earlier file hash would permit a race to supply
/// unverified pins, so every metadata read is bound to the signed file digest.
fn component_compatibility(
    root: &Path,
    manifest: &BridgeManifest,
) -> Result<CompatibilityMetadata> {
    let bytes = read_bounded(&root.join("compatibility.json"), MAX_JSON)?;
    if manifest.files.get("compatibility.json") != Some(&hash_bytes(&bytes)) {
        return Err("Transfiguration 相容性紀錄不符合已簽章內容".into());
    }
    let compatibility: CompatibilityMetadata =
        serde_json::from_slice(&bytes).map_err(|_| "Transfiguration 相容性紀錄格式不正確")?;
    if compatibility.protocol != 1
        || compatibility.artifact != BRIDGE
        || compatibility.fixture
        || compatibility.platform != "windows-x64"
        || compatibility.default_endpoint != "https://illya.test"
        || compatibility.endpoint_mode != "https_origin_per_session"
        || !compatibility.configurable_endpoint
        || compatibility.private_auth_filename != "auth.illyaverse.json"
        || !compatibility.shared_game_preferences
    {
        return Err("Transfiguration 相容性紀錄與私服連線策略不一致".into());
    }
    for version in [
        &compatibility.native_client_version,
        &compatibility.runtime,
        &compatibility.ruleset_api,
        &compatibility.harmony,
    ] {
        stable_version(version)?;
    }
    if compatibility
        .assemblies
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != COMPATIBILITY_ASSEMBLIES.into_iter().collect()
    {
        return Err("Transfiguration 相容性紀錄需要完整且精確的組件清單".into());
    }
    for assembly in compatibility.assemblies.values() {
        if !four_part_version(&assembly.version) {
            return Err("Transfiguration 原生組件版本格式不正確".into());
        }
        hex_bytes::<32>(&assembly.sha256.to_ascii_lowercase())?;
    }
    let client_assembly_version = format!("{}.0", compatibility.native_client_version);
    if compatibility.assemblies["osu.Game.dll"].version != client_assembly_version
        || compatibility.assemblies["osu!.dll"].version != client_assembly_version
    {
        return Err("Transfiguration 原生組件與指定 lazer 版本不一致".into());
    }
    let contracts: BTreeSet<_> = compatibility.contracts.iter().map(String::as_str).collect();
    if compatibility.contracts.is_empty()
        || compatibility.contracts.len() > 256
        || contracts.len() != compatibility.contracts.len()
        || contracts
            .iter()
            .any(|contract| contract.trim().is_empty() || contract.len() > 512)
        || REQUIRED_CONTRACTS
            .into_iter()
            .any(|required| !contracts.contains(required))
        || compatibility.verification.is_empty()
        || compatibility.verification.len() > 128
        || compatibility.verification.iter().any(|(key, value)| {
            key.is_empty() || key.len() > 256 || value.is_empty() || value.len() > 256
        })
    {
        return Err("Transfiguration 相容性紀錄缺少必要連線契約或驗證紀錄".into());
    }
    Ok(compatibility)
}

fn native_assemblies(root: &Path, manifest: &BridgeManifest) -> Result<BTreeMap<String, String>> {
    let compatibility = component_compatibility(root, manifest)?;
    Ok(NATIVE_ASSEMBLIES
        .into_iter()
        .map(|name| {
            (
                name.to_owned(),
                compatibility.assemblies[name].sha256.to_ascii_lowercase(),
            )
        })
        .collect())
}

/// All component files, including compatibility metadata and licenses, must still
/// match the signed release. Only that publisher can authorize a new exact native
/// assembly triplet; ranges and wildcard assembly metadata are never accepted.
pub(super) fn validate_component(root: &Path, manifest: &BridgeManifest) -> Result<()> {
    if manifest
        .files
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != COMPONENT_FILES.into_iter().collect()
        || manifest.files.get(BRIDGE) != Some(&manifest.bridge_sha256)
    {
        return Err("Transfiguration 元件內容不完整".into());
    }
    let mut total = 0_u64;
    for name in COMPONENT_FILES {
        let path = root.join(name);
        let length = fs::metadata(&path)
            .map_err(|_| "Transfiguration 元件檔案缺失")?
            .len();
        total = total
            .checked_add(length)
            .ok_or("Transfiguration 元件大小無效")?;
        if length == 0
            || total > MAX_PACKAGE
            || manifest.files.get(name) != Some(&hash_file(&path)?)
        {
            return Err("Transfiguration 元件已變更或不符合簽章".into());
        }
    }
    let artifact_bytes = read_bounded(&root.join("artifact.json"), MAX_JSON)?;
    if manifest.files.get("artifact.json") != Some(&hash_bytes(&artifact_bytes)) {
        return Err("Transfiguration 正式元件紀錄不符合已簽章內容".into());
    }
    let artifact: Artifact =
        serde_json::from_slice(&artifact_bytes).map_err(|_| "Transfiguration 正式元件紀錄無效")?;
    if artifact.protocol != 1
        || artifact.artifact != BRIDGE
        || artifact.sha256 != manifest.bridge_sha256
        || artifact.fixture
        || artifact.default_endpoint != "https://illya.test"
        || artifact.endpoint_mode != "https_origin_per_session"
        || !artifact.configurable_endpoint
        || artifact.private_auth_filename != "auth.illyaverse.json"
        || !artifact.shared_game_preferences
    {
        return Err("Transfiguration 正式元件與私服連線策略不一致".into());
    }
    component_compatibility(root, manifest)?;
    Ok(())
}

fn installed_release(root: &Path, launcher_version: &str) -> Result<Option<BridgeRelease>> {
    let path = root.join(INSTALLED_MANIFEST);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("無法確認已安裝的 Transfiguration 更新紀錄".into()),
        Ok(_) => {}
    }
    let release = parse_signed_manifest(&read_bounded(&path, MAX_JSON)?, launcher_version)?;
    validate_component(root, &release.manifest)?;
    Ok(Some(release))
}

pub(super) fn installed_version(root: &Path, launcher_version: &str) -> Result<String> {
    Ok(match installed_release(root, launcher_version)? {
        Some(release) => release.manifest.version,
        None => bundled_version()?.to_string(),
    })
}

/// Only a genuinely absent override permits the original compiled bridge hash.
/// The root is the verified current launcher directory, never player storage.
pub(crate) fn expected_bridge_hash(root: &Path) -> Result<Option<String>> {
    let launcher_version =
        option_env!("KALEIDOSCOPE_UPDATE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
    Ok(installed_release(root, launcher_version)?.map(|release| release.manifest.bridge_sha256))
}

/// An independently verified component may pin a newer tested native client.
/// Missing overrides keep the unsigned launcher's original compiled triplet.
pub(crate) fn expected_native_assemblies(root: &Path) -> Result<Option<BTreeMap<String, String>>> {
    let launcher_version =
        option_env!("KALEIDOSCOPE_UPDATE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
    installed_release(root, launcher_version)?
        .map(|release| native_assemblies(root, &release.manifest))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use sha2::{Digest, Sha256};

    fn signing() -> (Ed25519KeyPair, Trust) {
        let pair = Ed25519KeyPair::from_seed_unchecked(&[19; 32]).unwrap();
        let trust = Trust {
            repository: "example/Transfiguration".into(),
            key: pair.public_key().as_ref().try_into().unwrap(),
            bundled_version: Version::parse("0.1.0").unwrap(),
        };
        (pair, trust)
    }

    fn manifest() -> BridgeManifest {
        let hash = "a".repeat(64);
        BridgeManifest {
            protocol: 1,
            version: "0.1.1".into(),
            platform: "windows-x64".into(),
            repository: "example/Transfiguration".into(),
            tag: "v0.1.1".into(),
            asset: "Transfiguration-0.1.1-windows-x64.zip".into(),
            bytes: 1024,
            sha256: hash.clone(),
            bridge_sha256: hash.clone(),
            transfiguration_commit: "b".repeat(40),
            launcher_requirement: ">=0.1.0, <0.2.0".into(),
            files: COMPONENT_FILES
                .into_iter()
                .map(|name| (name.to_owned(), hash.clone()))
                .collect(),
        }
    }

    fn signed(pair: &Ed25519KeyPair, manifest: &BridgeManifest) -> Vec<u8> {
        let payload = serde_json::to_vec(manifest).unwrap();
        serde_json::to_vec(&Envelope {
            manifest: STANDARD.encode(&payload),
            signature: STANDARD.encode(pair.sign(&payload).as_ref()),
        })
        .unwrap()
    }

    #[test]
    fn independent_bridge_accepts_signed_compatible_component_and_rejects_tampering() {
        let (pair, trust) = signing();
        let bytes = signed(&pair, &manifest());
        let release = parse_with_trust(&bytes, "0.1.0", &trust).unwrap();
        assert_eq!(release.manifest.version, "0.1.1");
        let mut envelope: Envelope = serde_json::from_slice(&bytes).unwrap();
        let mut payload = STANDARD.decode(&envelope.manifest).unwrap();
        payload[0] ^= 1;
        envelope.manifest = STANDARD.encode(payload);
        assert!(
            parse_with_trust(&serde_json::to_vec(&envelope).unwrap(), "0.1.0", &trust).is_err()
        );
        let (other_pair, _) = signing();
        let other_key = Ed25519KeyPair::from_seed_unchecked(&[20; 32]).unwrap();
        assert!(parse_with_trust(&signed(&other_key, &manifest()), "0.1.0", &trust).is_err());
        assert!(parse_with_trust(&signed(&other_pair, &manifest()), "0.2.0", &trust).is_err());
    }

    #[test]
    fn independent_bridge_enforces_release_identity_file_inventory_and_bounds() {
        let (pair, trust) = signing();
        let changes: [fn(&mut BridgeManifest); 13] = [
            |m| m.repository = "other/Transfiguration".into(),
            |m| m.version = "0.1.0".into(),
            |m| m.version = "0.1.2-beta.1".into(),
            |m| m.tag = "v0.1.2".into(),
            |m| m.asset = "other.zip".into(),
            |m| m.platform = "linux-x64".into(),
            |m| m.bytes = 0,
            |m| m.bytes = MAX_PACKAGE + 1,
            |m| {
                m.files.remove("compatibility.json");
            },
            |m| {
                m.files.insert("../other.dll".into(), "a".repeat(64));
            },
            |m| m.bridge_sha256 = "b".repeat(64),
            |m| m.transfiguration_commit = "A".repeat(40),
            |m| m.launcher_requirement = "not a requirement".into(),
        ];
        for change in changes {
            let mut value = manifest();
            change(&mut value);
            assert!(parse_with_trust(&signed(&pair, &value), "0.1.0", &trust).is_err());
        }
        assert!(parse_with_trust(&vec![0; MAX_JSON as usize + 1], "0.1.0", &trust).is_err());
    }

    fn component(root: &Path) -> BridgeManifest {
        fs::create_dir(root.join("licenses")).unwrap();
        for name in COMPONENT_FILES {
            fs::write(root.join(name), b"synthetic component").unwrap();
        }
        let bridge_hash = format!("{:x}", Sha256::digest(b"synthetic component"));
        let policy = serde_json::json!({
            "protocol": 1, "artifact": BRIDGE, "sha256": bridge_hash,
            "default_endpoint": "https://illya.test", "endpoint_mode": "https_origin_per_session",
            "configurable_endpoint": true, "private_auth_filename": "auth.illyaverse.json",
            "shared_game_preferences": true, "fixture": false,
        });
        fs::write(
            root.join("artifact.json"),
            serde_json::to_vec(&policy).unwrap(),
        )
        .unwrap();
        let mut compatibility = policy;
        compatibility.as_object_mut().unwrap().remove("sha256");
        compatibility["platform"] = "windows-x64".into();
        compatibility["native_client_version"] = "2026.1012.0".into();
        compatibility["runtime"] = "10.0.12".into();
        compatibility["ruleset_api"] = "2026.818.0".into();
        compatibility["harmony"] = "2.4.2".into();
        compatibility["assemblies"] = COMPATIBILITY_ASSEMBLIES.into_iter().map(|name| {
            let version = if NATIVE_ASSEMBLIES.contains(&name) { "2026.1012.0.0" } else { "10.0.0.0" };
            (name.to_owned(), serde_json::json!({
                "version": version, "sha256": format!("{:x}", Sha256::digest(name.as_bytes())),
            }))
        }).collect::<serde_json::Map<String, serde_json::Value>>().into();
        compatibility["contracts"] = serde_json::json!(REQUIRED_CONTRACTS);
        compatibility["verification"] = serde_json::json!({"synthetic_test": "parser_fixture"});
        fs::write(
            root.join("compatibility.json"),
            serde_json::to_vec(&compatibility).unwrap(),
        )
        .unwrap();
        let mut value = manifest();
        value.bridge_sha256 = bridge_hash;
        for name in COMPONENT_FILES {
            value
                .files
                .insert(name.to_owned(), hash_file(&root.join(name)).unwrap());
        }
        value
    }

    #[test]
    fn independent_bridge_component_verifies_metadata_licenses_and_exact_hashes() {
        let root =
            std::env::temp_dir().join(format!("kaleidoscope-bridge-feed-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let value = component(&root);
        validate_component(&root, &value).unwrap();
        fs::write(root.join("licenses/Harmony.txt"), b"changed").unwrap();
        assert!(validate_component(&root, &value).is_err());
        let mut changed = value;
        changed.files.insert(
            "licenses/Harmony.txt".into(),
            hash_file(&root.join("licenses/Harmony.txt")).unwrap(),
        );
        let mut artifact: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("artifact.json")).unwrap()).unwrap();
        artifact["fixture"] = true.into();
        fs::write(
            root.join("artifact.json"),
            serde_json::to_vec(&artifact).unwrap(),
        )
        .unwrap();
        changed.files.insert(
            "artifact.json".into(),
            hash_file(&root.join("artifact.json")).unwrap(),
        );
        assert!(validate_component(&root, &changed).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn independent_bridge_absent_override_allows_bundle_but_present_invalid_never_falls_back() {
        let root =
            std::env::temp_dir().join(format!("kaleidoscope-bridge-feed-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        assert!(expected_bridge_hash(&root).unwrap().is_none());
        assert!(expected_native_assemblies(&root).unwrap().is_none());
        fs::write(root.join(INSTALLED_MANIFEST), b"invalid override").unwrap();
        assert!(expected_bridge_hash(&root).is_err());
        assert!(expected_native_assemblies(&root).is_err());
        assert!(installed_version(&root, "0.1.0").is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    fn change_compatibility(
        root: &Path,
        manifest: &mut BridgeManifest,
        change: impl FnOnce(&mut serde_json::Value),
    ) {
        let path = root.join("compatibility.json");
        let mut value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        change(&mut value);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        manifest
            .files
            .insert("compatibility.json".into(), hash_file(&path).unwrap());
    }

    #[test]
    fn independent_bridge_signed_future_native_triplet_matches_synthetic_files_only() {
        let root =
            std::env::temp_dir().join(format!("kaleidoscope-bridge-feed-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let mut manifest = component(&root);
        let client = root.join("synthetic-native");
        fs::create_dir(&client).unwrap();
        for name in NATIVE_ASSEMBLIES {
            fs::write(client.join(name), name.as_bytes()).unwrap();
        }
        let (pair, trust) = signing();
        // Existing metadata contains uppercase native hashes. They are accepted
        // only after hex validation and normalized for exact file comparisons.
        change_compatibility(&root, &mut manifest, |compatibility| {
            for name in NATIVE_ASSEMBLIES {
                let hash = compatibility["assemblies"][name]["sha256"]
                    .as_str()
                    .unwrap()
                    .to_uppercase();
                compatibility["assemblies"][name]["sha256"] = hash.into();
            }
        });
        let release = parse_with_trust(&signed(&pair, &manifest), "0.1.0", &trust).unwrap();
        validate_component(&root, &release.manifest).unwrap();
        let pins = native_assemblies(&root, &release.manifest).unwrap();
        assert_eq!(
            pins.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            NATIVE_ASSEMBLIES.into_iter().collect()
        );
        for (name, expected) in &pins {
            assert_eq!(hash_file(&client.join(name)).unwrap(), *expected);
        }
        fs::write(client.join("osu.Game.dll"), b"unmatched native assembly").unwrap();
        assert_ne!(
            hash_file(&client.join("osu.Game.dll")).unwrap(),
            pins["osu.Game.dll"]
        );
        // A compatibility edit after release verification cannot supply new pins.
        let mut changed: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("compatibility.json")).unwrap()).unwrap();
        changed["assemblies"]["osu.Game.dll"]["sha256"] = "f".repeat(64).into();
        fs::write(
            root.join("compatibility.json"),
            serde_json::to_vec(&changed).unwrap(),
        )
        .unwrap();
        assert!(native_assemblies(&root, &release.manifest).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn independent_bridge_native_metadata_rejects_ranges_missing_pins_and_wrong_version_binding() {
        let root =
            std::env::temp_dir().join(format!("kaleidoscope-bridge-feed-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let base = component(&root);
        let original = fs::read(root.join("compatibility.json")).unwrap();
        let mutations: [fn(&mut serde_json::Value); 10] = [
            |value| value["native_client_version"] = "2026.1012".into(),
            |value| value["native_client_version"] = "2026.1012.0-beta.1".into(),
            |value| value["assemblies"]["osu!.dll"]["version"] = "2026.1013.0.0".into(),
            |value| value["assemblies"]["osu.Framework.dll"]["version"] = "2026.*.0.0".into(),
            |value| value["assemblies"]["osu.Framework.dll"]["sha256"] = "*".into(),
            |value| {
                value["assemblies"]
                    .as_object_mut()
                    .unwrap()
                    .remove("osu.Game.dll");
            },
            |value| {
                value["assemblies"]["other.dll"] =
                    serde_json::json!({"version": "1.0.0.0", "sha256": "a".repeat(64)})
            },
            |value| value["runtime"] = ">=10.0.0".into(),
            |value| value["ruleset_api"] = "*".into(),
            |value| value["contracts"] = serde_json::json!([]),
        ];
        let (pair, trust) = signing();
        for mutation in mutations {
            fs::write(root.join("compatibility.json"), &original).unwrap();
            let mut candidate = base.clone();
            change_compatibility(&root, &mut candidate, mutation);
            let release = parse_with_trust(&signed(&pair, &candidate), "0.1.0", &trust).unwrap();
            assert!(validate_component(&root, &release.manifest).is_err());
            assert!(native_assemblies(&root, &release.manifest).is_err());
        }
        fs::remove_dir_all(&root).unwrap();
    }
}
