//! Two signed GitHub feeds, applied as an atomic replacement of the owned bundle.
//! Publisher trust is compiled by the operator, never supplied by a download.

use super::{install, process, runtime};
use crate::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::blocking::Client;
use ring::signature::{UnparsedPublicKey, ED25519};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use url::Url;
use uuid::Uuid;

const MAX_JSON: u64 = 1024 * 1024;
const MAX_PACKAGE: u64 = 512 * 1024 * 1024;
const MAX_FILE: u64 = 256 * 1024 * 1024;
const MANIFEST_ASSET: &str = "updates.json";
const BRIDGE: &str = "osu.Game.Rulesets.Transfiguration.dll";
pub(crate) mod bridge_feed;
const PACKAGE_FILES: [&str; 17] = [
    "Kaleidoscope.exe",
    BRIDGE,
    "artifact.json",
    "compatibility.json",
    "THIRD-PARTY-NOTICES.md",
    "LICENSE",
    "README.txt",
    "Transfiguration-README.md",
    "licenses/DotNet.txt",
    "licenses/Harmony.txt",
    "licenses/Iced.txt",
    "licenses/Mono.Cecil.txt",
    "licenses/MonoMod.txt",
    "Kaleidoscope-THIRD-PARTY-NOTICES.md",
    "licenses/Nunito.txt",
    "licenses/NotoSansTC.txt",
    "package.json",
];

#[derive(Clone)]
struct Trust {
    repository: String,
    key: [u8; 32],
    version: Version,
}

fn valid_repository(repository: &str) -> bool {
    let Some((owner, name)) = repository.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && owner.len() <= 39
        && !name.is_empty()
        && name.len() <= 100
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && !matches!(name, "." | "..")
        && !name.ends_with(".git")
}

fn hex_bytes<const N: usize>(value: &str) -> Result<[u8; N]> {
    if value.len() != N * 2
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("更新簽章或雜湊格式不正確".into());
    }
    let mut output = [0; N];
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| "更新雜湊格式不正確")?;
    }
    Ok(output)
}

fn stable_version(value: &str) -> Result<Version> {
    let version = Version::parse(value).map_err(|_| "更新版本格式不正確")?;
    if !version.pre.is_empty() || !version.build.is_empty() || version.to_string() != value {
        return Err("更新頻道只接受正式版本".into());
    }
    Ok(version)
}

fn compiled_trust() -> Result<Trust> {
    let repository = option_env!("KALEIDOSCOPE_UPDATE_REPOSITORY")
        .filter(|value| valid_repository(value))
        .ok_or("此版本尚未設定 GitHub 更新來源")?;
    let key = hex_bytes(
        option_env!("KALEIDOSCOPE_UPDATE_PUBLIC_KEY").ok_or("此版本尚未設定更新簽章公鑰")?,
    )?;
    if key == [0; 32] {
        return Err("更新簽章公鑰無效".into());
    }
    Ok(Trust {
        repository: repository.to_owned(),
        key,
        version: stable_version(
            option_env!("KALEIDOSCOPE_UPDATE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")),
        )?,
    })
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub configured: bool,
    pub repository: Option<String>,
    pub current_version: String,
    pub available_version: Option<String>,
    pub download_bytes: Option<u64>,
    pub detail: String,
    pub bridge_configured: bool,
    pub bridge_repository: Option<String>,
    pub bridge_version: Option<String>,
    pub bridge_available_version: Option<String>,
    pub bridge_download_bytes: Option<u64>,
    pub bridge_detail: String,
}

#[derive(Serialize)]
pub struct BuildConfiguration {
    protocol: u8,
    configured: bool,
    repository: String,
    public_key: String,
    version: String,
    bridge_version: String,
}

pub fn build_configuration() -> Result<BuildConfiguration> {
    let trust = compiled_trust()?;
    Ok(BuildConfiguration {
        protocol: 1,
        configured: true,
        repository: trust.repository,
        public_key: trust.key.iter().map(|byte| format!("{byte:02x}")).collect(),
        version: trust.version.to_string(),
        bridge_version: bundled_bridge_version()?.to_string(),
    })
}

pub fn status() -> UpdateStatus {
    let (bridge_configured, bridge_repository, bridge_version, bridge_detail) =
        match bridge_feed::configuration() {
            Ok(configuration) => {
                let installed = std::env::current_exe()
                    .ok()
                    .and_then(|path| path.parent().map(Path::to_path_buf))
                    .ok_or("無法確認已安裝的 Transfiguration".to_owned())
                    .and_then(|path| bridge_feed::installed_version(&path, launcher_version()));
                let detail = installed
                    .as_ref()
                    .err()
                    .cloned()
                    .unwrap_or_else(|| "獨立檢查相容的連線模組更新。".into());
                (true, Some(configuration.repository), installed.ok(), detail)
            }
            Err(error) => (false, None, None, error),
        };
    let mut state = match compiled_trust() {
        Ok(trust) => UpdateStatus {
            configured: true,
            repository: Some(trust.repository),
            current_version: trust.version.to_string(),
            available_version: None,
            download_bytes: None,
            detail: "啟動器更新會附帶配套連線模組。".into(),
            bridge_configured: false,
            bridge_repository: None,
            bridge_version: None,
            bridge_available_version: None,
            bridge_download_bytes: None,
            bridge_detail: String::new(),
        },
        Err(error) => UpdateStatus {
            configured: false,
            repository: None,
            current_version: option_env!("KALEIDOSCOPE_UPDATE_VERSION")
                .unwrap_or(env!("CARGO_PKG_VERSION"))
                .to_owned(),
            available_version: None,
            download_bytes: None,
            detail: error,
            bridge_configured: false,
            bridge_repository: None,
            bridge_version: None,
            bridge_available_version: None,
            bridge_download_bytes: None,
            bridge_detail: String::new(),
        },
    };
    state.bridge_configured = bridge_configured;
    state.bridge_repository = bridge_repository;
    state.bridge_version = bridge_version;
    state.bridge_detail = bridge_detail;
    state
}

fn launcher_version() -> &'static str {
    option_env!("KALEIDOSCOPE_UPDATE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

fn bundled_bridge_version() -> Result<Version> {
    stable_version(option_env!("TRANSFIGURATION_BUNDLED_VERSION").unwrap_or("0.1.0"))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    manifest: String,
    signature: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    protocol: u8,
    version: String,
    platform: String,
    repository: String,
    tag: String,
    asset: String,
    bytes: u64,
    sha256: String,
    launcher_sha256: String,
    bridge_sha256: String,
    kaleidoscope_commit: String,
    transfiguration_commit: String,
    bridge_version: String,
    files: BTreeMap<String, String>,
}

fn verify_manifest(bytes: &[u8], trust: &Trust) -> Result<(Envelope, Manifest)> {
    if bytes.len() as u64 > MAX_JSON {
        return Err("更新資訊超過大小限制".into());
    }
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|_| "無法讀取更新簽章")?;
    let payload = STANDARD
        .decode(&envelope.manifest)
        .map_err(|_| "更新簽章格式不正確")?;
    let signature = STANDARD
        .decode(&envelope.signature)
        .map_err(|_| "更新簽章格式不正確")?;
    if payload.len() as u64 > MAX_JSON || signature.len() != 64 {
        return Err("更新簽章格式不正確".into());
    }
    UnparsedPublicKey::new(&ED25519, trust.key)
        .verify(&payload, &signature)
        .map_err(|_| "更新簽章驗證失敗；未安裝任何檔案")?;
    let manifest: Manifest = serde_json::from_slice(&payload).map_err(|_| "更新資訊格式不正確")?;
    let version = stable_version(&manifest.version)?;
    stable_version(&manifest.bridge_version)?;
    if manifest.protocol != 1
        || manifest.platform != "windows-x64"
        || manifest.repository != trust.repository
        || version <= trust.version
        || manifest.tag != format!("v{version}")
        || manifest.asset != format!("Kaleidoscope-{version}-windows-x64.zip")
        || manifest.bytes == 0
        || manifest.bytes > MAX_PACKAGE
        || manifest
            .files
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != PACKAGE_FILES.into_iter().collect()
    {
        return Err("更新來源、版本或套件內容不符合此啟動器".into());
    }
    for hash in manifest.files.values().chain([
        &manifest.sha256,
        &manifest.launcher_sha256,
        &manifest.bridge_sha256,
    ]) {
        hex_bytes::<32>(hash)?;
    }
    hex_bytes::<20>(&manifest.kaleidoscope_commit)?;
    hex_bytes::<20>(&manifest.transfiguration_commit)?;
    if manifest.files.get("Kaleidoscope.exe") != Some(&manifest.launcher_sha256)
        || manifest.files.get(BRIDGE) != Some(&manifest.bridge_sha256)
    {
        return Err("啟動器與連線模組版本不一致".into());
    }
    Ok((envelope, manifest))
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

fn client() -> Result<Client> {
    Client::builder()
        .user_agent("Kaleidoscope-updater/1")
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| "無法初始化 GitHub 更新連線".into())
}

/// No authorization headers, arbitrary destinations, HTTP downgrade or local
/// addresses. GitHub's signed asset redirect is allowed only on its fixed CDN.
fn fetch(client: &Client, initial: Url, limit: u64, api: bool) -> Result<Vec<u8>> {
    let mut url = initial;
    for redirect in 0..=4 {
        let mut request = client.get(url.clone());
        if api {
            request = request
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2026-03-10");
        }
        let mut response = request
            .send()
            .map_err(|_| "GitHub 更新連線失敗，請稍後重試")?;
        if response.status().is_redirection() {
            if api || redirect == 4 {
                return Err("GitHub 更新來源重新導向不受支援".into());
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .filter(|value| value.len() <= 8192)
                .ok_or("GitHub 更新重新導向無效")?;
            let next = url.join(location).map_err(|_| "GitHub 更新重新導向無效")?;
            if next.scheme() != "https"
                || next.port_or_known_default() != Some(443)
                || !next.username().is_empty()
                || next.password().is_some()
                || next.fragment().is_some()
                || next.host_str() != Some("release-assets.githubusercontent.com")
            {
                return Err("GitHub 更新重新導向不在允許的下載來源".into());
            }
            url = next;
            continue;
        }
        if !response.status().is_success() {
            return Err(match response.status().as_u16() {
                404 => "GitHub 尚未發布可用的更新",
                403 | 429 => "GitHub 更新請求暫時受到限制，請稍後重試",
                _ => "GitHub 未能提供更新，請稍後重試",
            }
            .into());
        }
        if response.content_length().is_some_and(|size| size > limit) {
            return Err("更新檔案超過大小限制".into());
        }
        let mut output = Vec::new();
        response
            .by_ref()
            .take(limit + 1)
            .read_to_end(&mut output)
            .map_err(|_| "更新下載未完成，請重試")?;
        if output.len() as u64 > limit {
            return Err("更新檔案超過大小限制".into());
        }
        return Ok(output);
    }
    Err("GitHub 更新重新導向過多".into())
}

fn release_url(repository: &str, tag: &str, asset: &str) -> String {
    format!("https://github.com/{repository}/releases/download/{tag}/{asset}")
}

fn release_asset<'a>(release: &'a Release, repository: &str, name: &str) -> Result<&'a Asset> {
    let mut found = release.assets.iter().filter(|asset| asset.name == name);
    let asset = found.next().ok_or("GitHub 更新缺少完整套件或簽章")?;
    if found.next().is_some()
        || asset.browser_download_url != release_url(repository, &release.tag_name, name)
    {
        return Err("GitHub 更新資產來源不正確".into());
    }
    Ok(asset)
}

struct Candidate {
    envelope: Envelope,
    manifest: Manifest,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum UpdateComponent {
    Launcher,
    Bridge,
}

#[derive(Default)]
pub struct Manager {
    candidate: Option<Candidate>,
    bridge_candidate: Option<bridge_feed::BridgeRelease>,
    last_check: Option<Instant>,
}

impl Manager {
    pub fn check(&mut self) -> Result<UpdateStatus> {
        if self
            .last_check
            .is_some_and(|when| when.elapsed() < Duration::from_secs(60))
        {
            return Err("剛剛已檢查更新，請稍後再試".into());
        }
        self.last_check = Some(Instant::now());
        self.candidate = None;
        self.bridge_candidate = None;
        let mut state = status();
        if state.configured {
            match self.check_launcher() {
                Ok(available) => {
                    state.available_version = available.available_version;
                    state.download_bytes = available.download_bytes;
                    state.detail = available.detail;
                }
                Err(error) => state.detail = error,
            }
        }
        if state.bridge_configured {
            match self.check_bridge() {
                Ok(Some(candidate)) => {
                    state.bridge_available_version = Some(candidate.manifest.version.clone());
                    state.bridge_download_bytes = Some(candidate.manifest.bytes);
                    state.bridge_detail = "有相容的連線模組更新，安裝時會保留目前啟動器。".into();
                    self.bridge_candidate = Some(candidate);
                }
                Ok(None) => state.bridge_detail = "連線模組已是最新版本。".into(),
                Err(error) => state.bridge_detail = error,
            }
        }
        Ok(state)
    }

    fn check_bridge(&self) -> Result<Option<bridge_feed::BridgeRelease>> {
        let configuration = bridge_feed::configuration()?;
        let executable =
            canonical_file(&std::env::current_exe().map_err(|_| "無法確認啟動器位置")?)?;
        let installed = bridge_feed::installed_version(
            executable.parent().ok_or("啟動器缺少資料夾")?,
            launcher_version(),
        )?;
        let client = client()?;
        let bytes = fetch(
            &client,
            Url::parse(&format!(
                "https://api.github.com/repos/{}/releases/latest",
                configuration.repository
            ))
            .map_err(|_| "Transfiguration 更新來源無效")?,
            MAX_JSON,
            true,
        )?;
        let release: Release =
            serde_json::from_slice(&bytes).map_err(|_| "Transfiguration 更新資訊無效")?;
        if release.draft || release.prerelease || release.assets.len() > 100 {
            return Err("GitHub 沒有正式的連線模組更新".into());
        }
        let version = stable_version(
            release
                .tag_name
                .strip_prefix('v')
                .ok_or("Transfiguration 更新標籤格式不正確")?,
        )?;
        if version <= stable_version(&installed)? {
            return Ok(None);
        }
        let signed = release_asset(&release, &configuration.repository, MANIFEST_ASSET)?;
        if signed.size == 0 || signed.size > MAX_JSON {
            return Err("Transfiguration 更新簽章大小不正確".into());
        }
        let bytes = fetch(
            &client,
            Url::parse(&signed.browser_download_url).map_err(|_| "Transfiguration 更新來源無效")?,
            MAX_JSON,
            false,
        )?;
        let candidate = bridge_feed::parse_signed_manifest(&bytes, launcher_version())?;
        let asset = release_asset(
            &release,
            &configuration.repository,
            &candidate.manifest.asset,
        )?;
        if candidate.manifest.tag != release.tag_name || candidate.manifest.bytes != asset.size {
            return Err("Transfiguration 發行版本與已簽章套件不一致".into());
        }
        Ok(Some(candidate))
    }

    fn check_launcher(&mut self) -> Result<UpdateStatus> {
        let trust = compiled_trust()?;
        let client = client()?;
        let bytes = fetch(
            &client,
            Url::parse(&format!(
                "https://api.github.com/repos/{}/releases/latest",
                trust.repository
            ))
            .map_err(|_| "GitHub 更新來源無效")?,
            MAX_JSON,
            true,
        )?;
        let release: Release =
            serde_json::from_slice(&bytes).map_err(|_| "GitHub 更新資訊格式不正確")?;
        if release.draft || release.prerelease || release.assets.len() > 100 {
            return Err("GitHub 沒有正式的完整更新".into());
        }
        let Some(version) = release.tag_name.strip_prefix('v') else {
            return Err("GitHub 更新標籤格式不正確".into());
        };
        let version = stable_version(version)?;
        if version <= trust.version {
            return Ok(UpdateStatus {
                detail: "目前已是最新版本。".into(),
                ..status()
            });
        }
        let signed = release_asset(&release, &trust.repository, MANIFEST_ASSET)?;
        if signed.size == 0 || signed.size > MAX_JSON {
            return Err("GitHub 更新簽章大小不正確".into());
        }
        let bytes = fetch(
            &client,
            Url::parse(&signed.browser_download_url).map_err(|_| "GitHub 更新來源無效")?,
            MAX_JSON,
            false,
        )?;
        let (envelope, manifest) = verify_manifest(&bytes, &trust)?;
        let executable =
            canonical_file(&std::env::current_exe().map_err(|_| "無法確認啟動器位置")?)?;
        let installed_bridge = bridge_feed::installed_version(
            executable.parent().ok_or("啟動器缺少資料夾")?,
            launcher_version(),
        )?;
        ensure_no_bridge_downgrade(&manifest.bridge_version, &installed_bridge)?;
        let package = release_asset(&release, &trust.repository, &manifest.asset)?;
        if manifest.tag != release.tag_name || package.size != manifest.bytes {
            return Err("GitHub 發行版本與已簽章套件不一致".into());
        }
        let state = UpdateStatus {
            available_version: Some(manifest.version.clone()),
            download_bytes: Some(manifest.bytes),
            detail: "可用更新會一併替換啟動器與連線模組；安裝後重新啟動。".into(),
            ..status()
        };
        self.candidate = Some(Candidate { envelope, manifest });
        Ok(state)
    }

    /// Returns only after the independently verified helper is started. The GUI
    /// exits next; the helper checks the old process identity and idle lock again.
    pub fn install(&mut self, component: UpdateComponent) -> Result<()> {
        if component == UpdateComponent::Bridge {
            return self.install_bridge();
        }
        let candidate = self.candidate.as_ref().ok_or("請先檢查並確認可用更新")?;
        let trust = compiled_trust()?;
        verify_manifest(
            &serde_json::to_vec(&candidate.envelope).map_err(|_| "無法讀取更新簽章")?,
            &trust,
        )?;
        let executable =
            canonical_file(&std::env::current_exe().map_err(|_| "無法確認啟動器位置")?)?;
        if executable.file_name().and_then(|name| name.to_str()) != Some("Kaleidoscope.exe") {
            return Err("請從完整 Kaleidoscope 套件資料夾執行更新".into());
        }
        let target = executable.parent().ok_or("啟動器套件路徑無效")?;
        let old_files = verify_owned_package(target)?;
        ensure_no_bridge_downgrade(
            &candidate.manifest.bridge_version,
            &bridge_feed::installed_version(target, launcher_version())?,
        )?;
        let parent = target.parent().ok_or("啟動器套件必須有父資料夾")?;
        check_path(parent)?;
        let workspace = parent.join(format!(".kaleidoscope-update-{}", Uuid::new_v4()));
        fs::create_dir(&workspace).map_err(|_| "無法建立更新暫存；請確認套件資料夾可寫入")?;
        let workspace = fs::canonicalize(workspace).map_err(|_| "無法確認更新暫存路徑")?;
        let payload = workspace.join("payload");
        fs::create_dir(&payload).map_err(|_| "無法建立更新套件暫存")?;
        let result = (|| {
            let bytes = fetch(
                &client()?,
                Url::parse(&release_url(
                    &trust.repository,
                    &candidate.manifest.tag,
                    &candidate.manifest.asset,
                ))
                .map_err(|_| "GitHub 更新來源無效")?,
                candidate.manifest.bytes,
                false,
            )?;
            if bytes.len() as u64 != candidate.manifest.bytes
                || hash_bytes(&bytes) != candidate.manifest.sha256
            {
                return Err("更新套件雜湊不符合簽章；未安裝任何檔案".into());
            }
            extract_package(&bytes, &payload, &candidate.manifest)?;
            verify_owned_package(&payload)?;
            let helper = workspace.join("update-helper.exe");
            fs::copy(payload.join("Kaleidoscope.exe"), &helper).map_err(|_| "無法準備更新程序")?;
            if hash_file(&helper)? != candidate.manifest.launcher_sha256 {
                return Err("更新程序驗證失敗".into());
            }
            let transaction = Transaction {
                protocol: 1,
                parent_process: process::current_identity()?,
                target: target.to_path_buf(),
                target_identity: install::directory_identity(target)?,
                old_files,
                envelope: candidate.envelope.clone(),
                previous_version: trust.version.to_string(),
                component: UpdateComponent::Launcher,
            };
            write_new_json(&workspace.join("transaction.json"), &transaction)?;
            runtime::with_idle_update(|| {
                if verify_owned_package(target)? != transaction.old_files {
                    return Err("原有套件已變更，請重新檢查更新".into());
                }
                let mut command = Command::new(&helper);
                command
                    .arg("--kaleidoscope-apply-update")
                    .arg(&workspace)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    command.creation_flags(0x08000000);
                }
                command
                    .spawn()
                    .map_err(|_| "無法啟動更新程序；原有套件已保留")?;
                Ok(())
            })
        })();
        if result.is_err() {
            // Only this newly created workspace is eligible for removal.
            if workspace.parent() == Some(parent)
                && workspace
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".kaleidoscope-update-"))
            {
                let _ = fs::remove_dir_all(&workspace);
            }
        }
        result
    }

    fn install_bridge(&mut self) -> Result<()> {
        let candidate = self
            .bridge_candidate
            .as_ref()
            .ok_or("請先檢查並確認可用的連線模組更新")?;
        bridge_feed::parse_signed_manifest(
            &serde_json::to_vec(&candidate.envelope).map_err(|_| "連線模組更新簽章無效")?,
            launcher_version(),
        )?;
        let executable =
            canonical_file(&std::env::current_exe().map_err(|_| "無法確認啟動器位置")?)?;
        if executable.file_name().and_then(|name| name.to_str()) != Some("Kaleidoscope.exe") {
            return Err("請從完整 Kaleidoscope 套件資料夾執行更新".into());
        }
        let target = executable.parent().ok_or("啟動器套件路徑無效")?;
        let old_files = verify_owned_package(target)?;
        let installed = bridge_feed::installed_version(target, launcher_version())?;
        if stable_version(&candidate.manifest.version)? <= stable_version(&installed)? {
            return Err("此連線模組版本已安裝；請重新檢查更新".into());
        }
        let parent = target.parent().ok_or("啟動器套件必須有父資料夾")?;
        check_path(parent)?;
        let workspace = parent.join(format!(".kaleidoscope-update-{}", Uuid::new_v4()));
        fs::create_dir(&workspace).map_err(|_| "無法建立更新暫存；請確認套件資料夾可寫入")?;
        let workspace = fs::canonicalize(workspace).map_err(|_| "無法確認更新暫存路徑")?;
        let payload = workspace.join("payload");
        fs::create_dir(&payload).map_err(|_| "無法建立更新套件暫存")?;
        let result = (|| {
            for name in old_files.keys().filter(|name| {
                name.as_str() != "package.json"
                    && name.as_str() != bridge_feed::INSTALLED_MANIFEST
                    && !bridge_feed::COMPONENT_FILES.contains(&name.as_str())
            }) {
                if name.starts_with("licenses/") && !payload.join("licenses").exists() {
                    fs::create_dir(payload.join("licenses")).map_err(|_| "無法建立更新授權暫存")?;
                }
                fs::copy(target.join(name), payload.join(name))
                    .map_err(|_| "無法保留原有啟動器檔案")?;
                if old_files.get(name) != Some(&hash_file(&payload.join(name))?) {
                    return Err("原有啟動器在準備更新時已變更".into());
                }
            }
            let bytes = fetch(
                &client()?,
                Url::parse(&release_url(
                    &candidate.manifest.repository,
                    &candidate.manifest.tag,
                    &candidate.manifest.asset,
                ))
                .map_err(|_| "GitHub 連線模組更新來源無效")?,
                candidate.manifest.bytes,
                false,
            )?;
            if bytes.len() as u64 != candidate.manifest.bytes
                || hash_bytes(&bytes) != candidate.manifest.sha256
            {
                return Err("連線模組更新套件雜湊不符合簽章；未安裝任何檔案".into());
            }
            extract_files(&bytes, &payload, &candidate.manifest.files)?;
            bridge_feed::validate_component(&payload, &candidate.manifest)?;
            let package = updated_bridge_package(target, &candidate.manifest)?;
            let mut package_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(payload.join("package.json"))
                .map_err(|_| "無法保存配套更新紀錄")?;
            package_file
                .write_all(&package)
                .and_then(|()| package_file.sync_all())
                .map_err(|_| "無法保存配套更新紀錄")?;
            write_new_json(
                &payload.join(bridge_feed::INSTALLED_MANIFEST),
                &candidate.envelope,
            )?;
            verify_owned_package(&payload)?;
            let helper = workspace.join("update-helper.exe");
            fs::copy(payload.join("Kaleidoscope.exe"), &helper).map_err(|_| "無法準備更新程序")?;
            if old_files.get("Kaleidoscope.exe") != Some(&hash_file(&helper)?) {
                return Err("原有啟動器的更新程序驗證失敗".into());
            }
            let transaction = Transaction {
                protocol: 1,
                parent_process: process::current_identity()?,
                target: target.to_path_buf(),
                target_identity: install::directory_identity(target)?,
                old_files,
                envelope: candidate.envelope.clone(),
                previous_version: launcher_version().into(),
                component: UpdateComponent::Bridge,
            };
            write_new_json(&workspace.join("transaction.json"), &transaction)?;
            runtime::with_idle_update(|| {
                if verify_owned_package(target)? != transaction.old_files {
                    return Err("原有套件已變更，請重新檢查更新".into());
                }
                let mut command = Command::new(&helper);
                command
                    .arg("--kaleidoscope-apply-update")
                    .arg(&workspace)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    command.creation_flags(0x08000000);
                }
                command
                    .spawn()
                    .map_err(|_| "無法啟動更新程序；原有套件已保留")?;
                Ok(())
            })
        })();
        if result.is_err() && workspace.parent() == Some(parent) {
            let _ = fs::remove_dir_all(&workspace);
        }
        result
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Transaction {
    protocol: u8,
    parent_process: process::Identity,
    target: PathBuf,
    target_identity: String,
    old_files: BTreeMap<String, String>,
    envelope: Envelope,
    previous_version: String,
    component: UpdateComponent,
}

fn updated_bridge_package(
    original: &Path,
    manifest: &bridge_feed::BridgeManifest,
) -> Result<Vec<u8>> {
    let mut package: serde_json::Value =
        serde_json::from_slice(&read_bounded(&original.join("package.json"), MAX_JSON)?)
            .map_err(|_| "原有完整套件紀錄無效")?;
    let content = package["content_sha256"]
        .as_object_mut()
        .ok_or("原有套件缺少內容紀錄")?;
    for (name, hash) in &manifest.files {
        if !content.contains_key(name) {
            return Err("連線模組更新含有不允許的套件檔案".into());
        }
        content.insert(name.clone(), serde_json::Value::String(hash.clone()));
    }
    package["bridge_sha256"] = serde_json::Value::String(manifest.bridge_sha256.clone());
    serde_json::to_vec(&package).map_err(|_| "無法序列化配套更新紀錄".into())
}

fn write_new_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "無法寫入更新程序紀錄")?;
    let bytes = serde_json::to_vec(value).map_err(|_| "無法序列化更新程序紀錄")?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| "無法保存更新程序紀錄".into())
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    canonical_file(path)?;
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| "無法讀取更新套件")?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "無法讀取更新套件")?;
    if bytes.len() as u64 > limit {
        return Err("更新套件檔案超過大小限制".into());
    }
    Ok(bytes)
}

fn check_path(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err("更新只接受完整本機路徑".into());
    }
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).map_err(|_| "無法確認更新路徑")?;
        if metadata.file_type().is_symlink() {
            return Err("更新路徑不能包含符號連結".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("更新路徑不能包含 reparse point".into());
            }
        }
    }
    Ok(())
}

fn canonical_file(path: &Path) -> Result<PathBuf> {
    check_path(path)?;
    if !fs::metadata(path)
        .map_err(|_| "無法讀取更新檔案")?
        .is_file()
    {
        return Err("更新內容不是一般檔案".into());
    }
    fs::canonicalize(path).map_err(|_| "無法確認更新檔案路徑".into())
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hash_file(path: &Path) -> Result<String> {
    canonical_file(path)?;
    let mut file = File::open(path).map_err(|_| "無法讀取更新檔案")?;
    if file.metadata().map_err(|_| "無法確認更新檔案大小")?.len() > MAX_FILE {
        return Err("更新檔案超過大小限制".into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut buffer).map_err(|_| "無法讀取更新檔案")?;
        if count == 0 {
            break;
        }
        total = total.checked_add(count as u64).ok_or("更新檔案大小無效")?;
        if total > MAX_FILE {
            return Err("更新檔案超過大小限制".into());
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn inventory(root: &Path) -> Result<BTreeSet<String>> {
    check_path(root)?;
    let mut files = BTreeSet::new();
    for item in fs::read_dir(root).map_err(|_| "無法確認完整啟動器套件")? {
        let path = item.map_err(|_| "無法讀取套件檔案")?.path();
        check_path(&path)?;
        if path.file_name().and_then(|name| name.to_str()) == Some("licenses") && path.is_dir() {
            for license in fs::read_dir(path).map_err(|_| "無法讀取套件授權")? {
                let path = license.map_err(|_| "無法讀取套件授權")?.path();
                canonical_file(&path)?;
                files.insert(format!(
                    "licenses/{}",
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .ok_or("套件檔名無效")?
                ));
            }
        } else {
            canonical_file(&path)?;
            files.insert(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("套件檔名無效")?
                    .to_owned(),
            );
        }
    }
    let mut expected: BTreeSet<_> = PACKAGE_FILES.into_iter().map(str::to_owned).collect();
    if files.contains(bridge_feed::INSTALLED_MANIFEST) {
        expected.insert(bridge_feed::INSTALLED_MANIFEST.to_owned());
    }
    if files != expected {
        return Err("此資料夾含有額外或缺少的檔案；請先將完整套件放入獨立資料夾".into());
    }
    Ok(files)
}

fn verify_owned_package(root: &Path) -> Result<BTreeMap<String, String>> {
    let names = inventory(root)?;
    let files: BTreeMap<_, _> = names
        .iter()
        .map(|name| Ok((name.clone(), hash_file(&root.join(name))?)))
        .collect::<Result<_>>()?;
    let package: serde_json::Value =
        serde_json::from_slice(&read_bounded(&root.join("package.json"), MAX_JSON)?)
            .map_err(|_| "完整套件紀錄無效")?;
    let content = package["content_sha256"]
        .as_object()
        .ok_or("完整套件缺少內容紀錄")?;
    if package["version"] != 1
        || package["status"] != "complete"
        || package["fixture"] != false
        || package["embedded_interface_verified"] != true
        || package["verification_kind"] != "embedded_interface_command"
        || !matches!(package["build_profile"].as_str(), Some("debug" | "release"))
        || content.len() != PACKAGE_FILES.len() - 1
    {
        return Err("此資料夾不是已驗證的完整 GUI 套件".into());
    }
    for (name, hash) in files.iter().filter(|(name, _)| {
        name.as_str() != "package.json" && name.as_str() != bridge_feed::INSTALLED_MANIFEST
    }) {
        if content.get(name).and_then(|value| value.as_str()) != Some(hash) {
            return Err("套件内容已變更，請重新下載完整套件".into());
        }
    }
    let artifact: serde_json::Value =
        serde_json::from_slice(&read_bounded(&root.join("artifact.json"), MAX_JSON)?)
            .map_err(|_| "連線模組紀錄無效")?;
    if package["bridge_sha256"].as_str() != files.get(BRIDGE).map(String::as_str)
        || artifact["sha256"].as_str() != files.get(BRIDGE).map(String::as_str)
        || artifact["fixture"] != false
        || artifact["artifact"] != BRIDGE
    {
        return Err("啟動器套件與連線模組不一致".into());
    }
    if files.contains_key(bridge_feed::INSTALLED_MANIFEST) {
        let expected =
            bridge_feed::expected_bridge_hash(root)?.ok_or("連線模組更新缺少有效簽章")?;
        if files.get(BRIDGE) != Some(&expected) {
            return Err("連線模組更新內容與簽章不一致".into());
        }
    }
    Ok(files)
}

fn extract_package(bytes: &[u8], root: &Path, manifest: &Manifest) -> Result<()> {
    extract_files(bytes, root, &manifest.files)
}

fn extract_files(bytes: &[u8], root: &Path, files: &BTreeMap<String, String>) -> Result<()> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| "更新 ZIP 無效")?;
    if archive.len() != files.len() {
        return Err("更新 ZIP 內容數量不正確".into());
    }
    let mut seen = BTreeSet::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|_| "無法讀取更新 ZIP")?;
        let name = entry.name().to_owned();
        if !files.contains_key(&name)
            || !PACKAGE_FILES.contains(&name.as_str())
            || !seen.insert(name.clone())
            || entry.is_dir()
            || entry.size() == 0
            || entry.size() > MAX_FILE
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("更新 ZIP 含有不允許的檔案".into());
        }
        total = total
            .checked_add(entry.size())
            .ok_or("更新 ZIP 大小不正確")?;
        if total > MAX_PACKAGE {
            return Err("更新 ZIP 解壓縮超過大小限制".into());
        }
        let path = root.join(&name);
        if name.starts_with("licenses/") {
            let directory = root.join("licenses");
            if !directory.exists() {
                fs::create_dir(&directory).map_err(|_| "無法建立更新授權資料夾")?;
            }
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| "無法建立更新檔案")?;
        let declared_size = entry.size();
        let copied = std::io::copy(&mut entry.by_ref().take(MAX_FILE + 1), &mut file)
            .map_err(|_| "更新 ZIP 未完整解壓縮")?;
        if copied == 0 || copied > MAX_FILE || copied != declared_size {
            return Err("更新 ZIP 檔案大小不正確".into());
        }
        file.sync_all().map_err(|_| "無法保存更新檔案")?;
        if files.get(&name) != Some(&hash_file(&path)?) {
            return Err("更新檔案雜湊不符合簽章".into());
        }
    }
    Ok(())
}

pub fn handle_internal(arguments: &[String]) -> Option<Result<i32>> {
    if arguments.first().map(String::as_str) != Some("--kaleidoscope-apply-update") {
        return None;
    }
    Some(if arguments.len() != 2 {
        Err("更新程序參數無效".into())
    } else {
        apply_workspace(Path::new(&arguments[1])).map(|()| 0)
    })
}

fn apply_workspace(workspace: &Path) -> Result<()> {
    check_path(workspace)?;
    let workspace = fs::canonicalize(workspace).map_err(|_| "更新暫存路徑無效")?;
    let Some(name) = workspace.file_name().and_then(|name| name.to_str()) else {
        return Err("更新暫存名稱無效".into());
    };
    Uuid::parse_str(
        name.strip_prefix(".kaleidoscope-update-")
            .ok_or("更新暫存名称無效")?,
    )
    .map_err(|_| "更新暫存名稱無效")?;
    let helper = canonical_file(&std::env::current_exe().map_err(|_| "無法確認更新程序")?)?;
    if helper != canonical_file(&workspace.join("update-helper.exe"))? {
        return Err("更新程序不在預期暫存資料夾".into());
    }
    let transaction: Transaction = serde_json::from_slice(&read_bounded(
        &workspace.join("transaction.json"),
        MAX_JSON,
    )?)
    .map_err(|_| "更新程序紀錄無效")?;
    if transaction.protocol != 1
        || transaction.target.parent() != workspace.parent()
        || transaction.target == workspace
        || transaction.parent_process.executable != transaction.target.join("Kaleidoscope.exe")
    {
        return Err("更新目標不符合原有套件".into());
    }
    let verified = verify_transaction_payload(&transaction, &workspace, &helper)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match process::state(&transaction.parent_process)? {
            process::State::Gone => break,
            process::State::Ambiguous => {
                return Err("原有啟動器程序身分無法確認；未修改套件".into())
            }
            process::State::Live if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100))
            }
            process::State::Live => return Err("原有啟動器尚未關閉；未修改套件".into()),
        }
    }
    let result = runtime::with_idle_update(|| {
        if install::directory_identity(&transaction.target)? != transaction.target_identity
            || verify_owned_package(&transaction.target)? != transaction.old_files
            || !process::find_by_executable(&transaction.target.join("Kaleidoscope.exe"))?
                .is_empty()
        {
            return Err("原有套件已變更或另一個啟動器仍在執行；未修改套件".into());
        }
        let payload = workspace.join("payload");
        let hashes = verify_owned_package(&payload)?;
        if hashes != verified.files {
            return Err("更新暫存已變更；未修改套件".into());
        }
        let payload_identity = install::directory_identity(&payload)?;
        let backup = workspace.join("previous");
        fs::rename(&transaction.target, &backup).map_err(|_| "無法備份原有套件；未安裝更新")?;
        if install::directory_identity(&backup)? != transaction.target_identity
            || verify_owned_package(&backup)? != transaction.old_files
        {
            return Err("備份套件身分已變更；兩份套件均已保留，未啟動更新".into());
        }
        if let Err(error) = fs::rename(&payload, &transaction.target) {
            fs::rename(&backup, &transaction.target)
                .map_err(|_| "更新中斷，原有套件保留於更新暫存 previous 資料夾")?;
            let _ = Command::new(transaction.target.join("Kaleidoscope.exe")).spawn();
            return Err(format!("更新未安裝，已還原原有套件：{error}"));
        }
        let guards =
            match verify_moved_package(&transaction.target, &payload_identity, &verified.files) {
                Ok(guards) => guards,
                Err(error) => {
                    rollback_paths(&transaction.target, &payload, &backup)?;
                    let _ = Command::new(transaction.target.join("Kaleidoscope.exe")).spawn();
                    return Err(error);
                }
            };
        let startup = launch_and_wait(&transaction.target, &workspace);
        drop(guards);
        match rollback_stopped_startup(startup, &transaction.target, &payload, &backup) {
            Ok(None) => Ok(()),
            Ok(Some(error)) => {
                let _ = Command::new(transaction.target.join("Kaleidoscope.exe")).spawn();
                Err(format!("新版未能完成啟動，已還原原有套件：{error}"))
            }
            Err(error) => Err(error),
        }
    });
    // Keep the previous whole bundle and a bounded receipt for recovery. Never
    // recursively delete the old installation or an operator's directory.
    let _ = write_new_json(
        &workspace.join("result.json"),
        &serde_json::json!({
            "protocol": 1, "component": transaction.component, "version": verified.version, "installed": result.is_ok(),
            "detail": result.as_ref().err(),
        }),
    );
    result
}

fn rollback_paths(target: &Path, payload: &Path, backup: &Path) -> Result<()> {
    fs::rename(target, payload)
        .map_err(|_| "新版尚未關閉；原有完整套件保留於更新暫存 previous 資料夾")?;
    fs::rename(backup, target).map_err(|_| "原有套件保留於更新暫存 previous 資料夾".into())
}

struct VerifiedBundle {
    version: String,
    files: BTreeMap<String, String>,
}

fn verify_transaction_payload(
    transaction: &Transaction,
    workspace: &Path,
    helper: &Path,
) -> Result<VerifiedBundle> {
    let payload = workspace.join("payload");
    match transaction.component {
        UpdateComponent::Launcher => {
            // This helper is already the new launcher version. The signed
            // upgrade must advance its captured old version and match this one.
            let mut trust = compiled_trust()?;
            let helper_version = trust.version.clone();
            trust.version = stable_version(&transaction.previous_version)?;
            let (_, manifest) = verify_manifest(
                &serde_json::to_vec(&transaction.envelope).map_err(|_| "更新簽章無效")?,
                &trust,
            )?;
            if stable_version(&manifest.version)? != helper_version
                || hash_file(helper)? != manifest.launcher_sha256
            {
                return Err("更新程序與已簽章版本不一致".into());
            }
            if stable_version(&manifest.bridge_version)? != bundled_bridge_version()? {
                return Err("新版啟動器附帶的連線模組版本與簽章不一致".into());
            }
            ensure_no_bridge_downgrade(
                &manifest.bridge_version,
                &bridge_feed::installed_version(
                    &transaction.target,
                    &transaction.previous_version,
                )?,
            )?;
            Ok(VerifiedBundle {
                version: manifest.version,
                files: manifest.files,
            })
        }
        UpdateComponent::Bridge => {
            if transaction.previous_version != launcher_version()
                || transaction.old_files.get("Kaleidoscope.exe") != Some(&hash_file(helper)?)
            {
                return Err("連線模組更新不能替換目前啟動器".into());
            }
            let release = bridge_feed::parse_signed_manifest(
                &serde_json::to_vec(&transaction.envelope).map_err(|_| "連線模組更新簽章無效")?,
                launcher_version(),
            )?;
            let installed =
                bridge_feed::installed_version(&transaction.target, launcher_version())?;
            if stable_version(&release.manifest.version)? <= stable_version(&installed)? {
                return Err("連線模組更新不允許降版或重複安裝".into());
            }
            bridge_feed::validate_component(&payload, &release.manifest)?;
            let mut expected = transaction.old_files.clone();
            for (name, hash) in &release.manifest.files {
                expected.insert(name.clone(), hash.clone());
            }
            let package = updated_bridge_package(&transaction.target, &release.manifest)?;
            let envelope =
                serde_json::to_vec(&release.envelope).map_err(|_| "連線模組更新簽章無效")?;
            expected.insert("package.json".into(), hash_bytes(&package));
            expected.insert(
                bridge_feed::INSTALLED_MANIFEST.into(),
                hash_bytes(&envelope),
            );
            Ok(VerifiedBundle {
                version: release.manifest.version,
                files: expected,
            })
        }
    }
}

fn ensure_no_bridge_downgrade(bundled: &str, installed: &str) -> Result<()> {
    if stable_version(bundled)? < stable_version(installed)? {
        return Err("此啟動器發行版附帶較舊的連線模組，請等待相容的配套更新".into());
    }
    Ok(())
}

/// Lock every installed payload against write/delete on Windows before the
/// final hash pass and keep the handles through the new WebView acknowledgement.
fn verify_moved_package(
    root: &Path,
    identity: &str,
    expected: &BTreeMap<String, String>,
) -> Result<Vec<File>> {
    if install::directory_identity(root)? != identity {
        return Err("更新套件身分已變更".into());
    }
    let mut guards = Vec::new();
    for name in expected.keys() {
        let path = root.join(name);
        canonical_file(&path)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let mut file = options
            .open(&path)
            .map_err(|_| "無法鎖定已驗證的更新檔案")?;
        let mut digest = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        let mut total = 0_u64;
        loop {
            let count = file.read(&mut buffer).map_err(|_| "無法驗證更新檔案")?;
            if count == 0 {
                break;
            }
            total = total.checked_add(count as u64).ok_or("更新檔案大小無效")?;
            if total > MAX_FILE {
                return Err("更新檔案超過大小限制".into());
            }
            digest.update(&buffer[..count]);
        }
        if expected.get(name) != Some(&format!("{:x}", digest.finalize())) {
            return Err("已安裝的更新內容不符合簽章".into());
        }
        guards.push(file);
    }
    if install::directory_identity(root)? != identity || inventory(root)?.len() != expected.len() {
        return Err("更新套件身分或內容已變更".into());
    }
    Ok(guards)
}

enum StartupOutcome {
    Ready,
    Stopped(String),
}

fn stopped_before_identity(error: String, confirmed_gone: Result<bool>) -> Result<StartupOutcome> {
    if confirmed_gone? {
        Ok(StartupOutcome::Stopped(error))
    } else {
        Err(error)
    }
}

fn resolve_startup(
    result: Result<()>,
    close_and_observe: impl FnOnce() -> Result<process::State>,
) -> Result<StartupOutcome> {
    match result {
        Ok(()) => Ok(StartupOutcome::Ready),
        Err(error) => {
            if close_and_observe()? != process::State::Gone {
                return Err("新版尚未安全關閉；原有完整套件保留於更新暫存 previous 資料夾".into());
            }
            Ok(StartupOutcome::Stopped(error))
        }
    }
}

fn rollback_stopped_startup(
    startup: Result<StartupOutcome>,
    target: &Path,
    payload: &Path,
    backup: &Path,
) -> Result<Option<String>> {
    match startup? {
        StartupOutcome::Ready => Ok(None),
        StartupOutcome::Stopped(error) => {
            rollback_paths(target, payload, backup)?;
            Ok(Some(error))
        }
    }
}

fn launch_and_wait(target: &Path, workspace: &Path) -> Result<StartupOutcome> {
    let mut command = Command::new(target.join("Kaleidoscope.exe"));
    command
        .arg("--kaleidoscope-update-ready")
        .arg(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return stopped_before_identity("無法啟動新版".into(), Ok(true)),
    };
    let identity = match process::identity(child.id()) {
        Ok(Some(identity)) => identity,
        inspection => {
            let error = match inspection {
                Ok(None) => "新版在初始化時退出".into(),
                Err(error) => error,
                Ok(Some(_)) => unreachable!(),
            };
            // The owned Child handle is authoritative for an early exit. A
            // missing/unreadable identity alone must never permit rollback.
            return stopped_before_identity(
                error,
                child
                    .try_wait()
                    .map(|exit| exit.is_some())
                    .map_err(|_| "無法確認新版程序是否退出".into()),
            );
        }
    };
    if identity.executable != target.join("Kaleidoscope.exe") {
        return Err("新版程序位置無法確認".into());
    }
    let result: Result<()> = (|| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if child.try_wait().map_err(|_| "無法確認新版程序")?.is_some() {
                return Err("新版在初始化時退出".into());
            }
            if workspace.join("ready.json").exists() {
                let ready: process::Identity =
                    serde_json::from_slice(&read_bounded(&workspace.join("ready.json"), MAX_JSON)?)
                        .map_err(|_| "新版啟動確認無效")?;
                if ready != identity || process::state(&identity)? != process::State::Live {
                    return Err("新版啟動程序身分無法確認".into());
                }
                std::thread::sleep(Duration::from_millis(300));
                if child.try_wait().map_err(|_| "無法確認新版程序")?.is_some()
                    || process::state(&identity)? != process::State::Live
                {
                    return Err("新版在介面初始化後退出".into());
                }
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("新版初始化逾時".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })();
    resolve_startup(result, || {
        // Every failed acknowledgement must close this exact child before the
        // caller can restore the previous bundle, including malformed receipts.
        process::close_gracefully(&identity, Duration::from_secs(5))
            .map_err(|_| "新版初始化失敗，且尚未安全關閉；原有套件已保留")?;
        process::state(&identity)
    })
}

/// The mounted React launcher calls this command after the initial native
/// snapshot succeeds. A proof, CLI, or empty WebView cannot acknowledge readiness.
pub fn acknowledge_ready(arguments: &[String]) -> Result<()> {
    if arguments.first().map(String::as_str) != Some("--kaleidoscope-update-ready") {
        return Ok(());
    }
    if arguments.len() != 2 {
        return Err("更新啟動確認參數無效".into());
    }
    let workspace = Path::new(&arguments[1]);
    check_path(workspace)?;
    let workspace = fs::canonicalize(workspace).map_err(|_| "更新啟動確認路徑無效")?;
    let name = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("更新啟動確認名稱無效")?;
    Uuid::parse_str(
        name.strip_prefix(".kaleidoscope-update-")
            .ok_or("更新啟動確認名稱無效")?,
    )
    .map_err(|_| "更新啟動確認名稱無效")?;
    let transaction: Transaction = serde_json::from_slice(&read_bounded(
        &workspace.join("transaction.json"),
        MAX_JSON,
    )?)
    .map_err(|_| "更新啟動確認紀錄無效")?;
    let identity = process::current_identity()?;
    if transaction.protocol != 1
        || transaction.target.parent() != workspace.parent()
        || identity.executable != transaction.target.join("Kaleidoscope.exe")
    {
        return Err("更新啟動確認不符合已安裝套件".into());
    }
    write_new_json(&workspace.join("ready.json"), &identity)
}

#[cfg(test)]
mod tests;
