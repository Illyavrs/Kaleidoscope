//! Per-session endpoint launcher sessions. An independent guardian owns installation,
//! client lifetime, and exact-file cleanup even after the visible UI closes.

use super::endpoint::Endpoint;
use super::install::{artifact_sha256, ManagedRuleset, RULESET_FILENAME};
use super::storage::Library;
use super::{host, process, storage};
use crate::Result;
use fs2::FileExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

const SERVER: &str = "https://illya.test";
// Only stock assemblies independently inspected and tested with the bridge.
const SUPPORTED_GAME_SHA256: &str =
    "ad9527e411111cb4b4045c76df61c3474fd872a04af73160c05d24e44a588249";
const SUPPORTED_FRAMEWORK_SHA256: &str =
    "93b62f61c72539ed6fdb3c8eba18d276a74de8e1c094280b2531f5a76caea528";
const SUPPORTED_ENTRY_SHA256: &str =
    "18ae9bb6bb441e9a654ac72c859f527041bde52e5dce4109f8d9074584fa57a4";
const PREFS_FILENAME: &str = "prefs.json";
const SESSION_FILENAME: &str = "managed-session.json";
const MAX_RECORD_BYTES: u64 = 16 * 1024;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(15);
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const CLI_START_TIMEOUT: Duration = Duration::from_secs(40);
const MAX_FAILURE_BYTES: u64 = 2048;
const MAX_PROGRESS_BYTES: u64 = 2048;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Client {
    Stable,
    Lazer,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Preparing,
    Running,
    Closing,
    Cleaning,
    CleanupFailed,
    Error,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Compatibility {
    Unknown,
    Supported,
    Unsupported,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StableSnapshot {
    pub path: Option<String>,
    pub running: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LazerSnapshot {
    pub path: Option<String>,
    pub running: bool,
    pub compatibility: Compatibility,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub stable: StableSnapshot,
    pub lazer: LazerSnapshot,
    pub phase: Phase,
    pub detail: String,
    pub plugin_present: bool,
    pub active_endpoint: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LauncherSettings {
    pub endpoint: String,
}

/// Package-only diagnostics. This command reads the launcher and its adjacent
/// bridge, without resolving user storage, preferences or any game process.
#[derive(Debug, Serialize)]
pub struct PackageBindings {
    pub protocol: u8,
    pub artifact_kind: &'static str,
    pub default_endpoint: &'static str,
    pub endpoint_mode: &'static str,
    pub configurable_endpoint: bool,
    pub bridge_sha256: String,
    pub bridge_verified: bool,
    pub official_fallback: bool,
    pub shared_existing_library: bool,
    pub private_auth_filename: &'static str,
    pub shared_game_preferences: bool,
    pub supported_assemblies: std::collections::BTreeMap<&'static str, &'static str>,
}

pub fn inspect_package_bindings() -> Result<PackageBindings> {
    let hash = option_env!("ILLYA_BRIDGE_SHA256")
        .ok_or("此啟動器缺少正式連線模組發行雜湊，暫不支援 lazer")?;
    let launcher = canonical_file(&std::env::current_exe().map_err(|error| error.to_string())?)?;
    let bridge = bridge_at_launcher(&launcher)?;
    verify_bridge_artifact(&bridge, hash)?;
    Ok(PackageBindings {
        protocol: 1,
        artifact_kind: "cli_connection_test",
        default_endpoint: SERVER,
        endpoint_mode: "https_origin_per_session",
        configurable_endpoint: true,
        bridge_sha256: hash.to_owned(),
        bridge_verified: true,
        official_fallback: false,
        shared_existing_library: true,
        private_auth_filename: "auth.illyaverse.json",
        shared_game_preferences: true,
        supported_assemblies: std::collections::BTreeMap::from([
            ("osu.Game.dll", SUPPORTED_GAME_SHA256),
            ("osu.Framework.dll", SUPPORTED_FRAMEWORK_SHA256),
            ("osu!.dll", SUPPORTED_ENTRY_SHA256),
        ]),
    })
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Preferences {
    stable: Option<PathBuf>,
    lazer: Option<PathBuf>,
    #[serde(default)]
    endpoint: Endpoint,
}

impl Preferences {
    fn path(&self, client: Client) -> Option<&Path> {
        match client {
            Client::Stable => self.stable.as_deref(),
            Client::Lazer => self.lazer.as_deref(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Session {
    version: u8,
    nonce: String,
    client: Client,
    #[serde(default)]
    endpoint: Option<Endpoint>,
    selected_executable: PathBuf,
    selected_identity: String,
    launcher_executable: PathBuf,
    launcher_identity: String,
    guardian: Option<process::Identity>,
    child: Option<process::Identity>,
    install_session: Option<Uuid>,
    // Version 2 records the resolved library before the guardian is started.
    // Recovery must never follow a subsequently changed storage.ini/preferences.
    #[serde(default)]
    library: Option<Library>,
    // Version 1 wrote readiness beside the old private library. This flag only
    // permits that fixed historical location while migrating recovery records.
    #[serde(default)]
    legacy_ready: bool,
    phase: Phase,
    detail: String,
    stop_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure: Option<Failure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host_status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    child_exit: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_progress: Option<ProgressStage>,
}

impl Session {
    fn endpoint(&self) -> &Endpoint {
        self.endpoint.as_ref().expect("validated session endpoint")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProgressStage {
    OriginalMainEntered,
    GameHostCreated,
    GameHostRunEntered,
    RendererInitializing,
    WindowCreated,
    RendererInitialized,
    FirstSuccessfulDraw,
}

impl ProgressStage {
    const ALL: [Self; 7] = [
        Self::OriginalMainEntered,
        Self::GameHostCreated,
        Self::GameHostRunEntered,
        Self::RendererInitializing,
        Self::WindowCreated,
        Self::RendererInitialized,
        Self::FirstSuccessfulDraw,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::OriginalMainEntered => "original_main_entered",
            Self::GameHostCreated => "game_host_created",
            Self::GameHostRunEntered => "game_host_run_entered",
            Self::RendererInitializing => "renderer_initializing",
            Self::RendererInitialized => "renderer_initialized",
            Self::WindowCreated => "window_created",
            Self::FirstSuccessfulDraw => "first_successful_draw",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Progress {
    protocol: u8,
    session: String,
    stage: ProgressStage,
    endpoint: String,
    fixture: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Failure {
    protocol: u8,
    session: String,
    stage: String,
    code: String,
    exception: String,
    #[serde(default, skip_serializing_if = "FailureEndpoint::is_legacy")]
    endpoint: FailureEndpoint,
    fixture: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
enum FailureEndpoint {
    Selected(String),
    Unavailable,
    #[default]
    #[serde(skip)]
    LegacyMissing,
}

impl FailureEndpoint {
    fn is_legacy(&self) -> bool {
        matches!(self, Self::LegacyMissing)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    protocol: u8,
    session: String,
    status: String,
    endpoint: String,
    fixture: bool,
}

#[derive(Clone)]
struct Paths {
    control: PathBuf,
    local: PathBuf,
    roaming: PathBuf,
    legacy_data: PathBuf,
    launcher: PathBuf,
}

impl Paths {
    fn production() -> Result<Self> {
        #[cfg(not(windows))]
        return Err("Kaleidoscope 原生啟動器目前僅支援 Windows".into());
        #[cfg(windows)]
        {
            let local = storage::local_app_data()?;
            check_components(&local)?;
            let base = local.join("Illyaverse");
            let control = base.join("Kaleidoscope");
            ensure_directory(&control)?;
            let control = fs::canonicalize(control).map_err(|error| error.to_string())?;
            let roaming = storage::roaming_app_data()?;
            check_components(&roaming)?;
            let legacy_data = base.join("lazer");
            let launcher =
                canonical_file(&std::env::current_exe().map_err(|error| error.to_string())?)?;
            Ok(Self {
                control,
                local,
                roaming,
                legacy_data,
                launcher,
            })
        }
    }
    fn prefs(&self) -> PathBuf {
        self.control.join(PREFS_FILENAME)
    }
    fn session(&self) -> PathBuf {
        self.control.join(SESSION_FILENAME)
    }
    fn ready(&self, nonce: &str) -> PathBuf {
        self.control.join(format!("session-ready-{nonce}.json"))
    }
    fn failure(&self, nonce: &str) -> PathBuf {
        self.control.join(format!("session-failure-{nonce}.json"))
    }
    fn progress(&self, nonce: &str, stage: ProgressStage) -> PathBuf {
        self.control
            .join(format!("session-progress-{nonce}-{}.json", stage.name()))
    }
    fn bridge(&self) -> Result<PathBuf> {
        bridge_at_launcher(&self.launcher)
    }
}

fn bridge_at_launcher(launcher: &Path) -> Result<PathBuf> {
    Ok(launcher
        .parent()
        .ok_or("啟動器缺少安裝資料夾")?
        .join(RULESET_FILENAME))
}

fn resolve_library(paths: &Paths, executable: &Path) -> Result<Library> {
    storage::resolve(&paths.roaming, &paths.control, executable)
}

fn refuse_other_lazer_instances() -> Result<()> {
    // Also catches a selected client restarted during the launch race window.
    // Only start() may gracefully close the exact user-selected installation.
    if !process::find_lazer_candidates()?.is_empty() {
        return Err("另一個 lazer 正在執行；請先關閉後再使用現有圖譜庫".into());
    }
    Ok(())
}

/// Returns client paths, launcher state and the immutable active session endpoint.
pub fn snapshot() -> Result<Snapshot> {
    snapshot_at(&Paths::production()?)
}

pub fn launcher_settings() -> Result<LauncherSettings> {
    settings_at(&Paths::production()?)
}

/// Saves the next launch's endpoint. A live session keeps its captured endpoint.
pub fn launcher_set_endpoint(endpoint: &str) -> Result<LauncherSettings> {
    save_endpoint_at(&Paths::production()?, endpoint)
}

fn settings_at(paths: &Paths) -> Result<LauncherSettings> {
    Ok(LauncherSettings {
        endpoint: read_preferences(paths)?.endpoint.as_str().to_owned(),
    })
}

fn save_endpoint_at(paths: &Paths, endpoint: &str) -> Result<LauncherSettings> {
    let endpoint = Endpoint::parse(endpoint)?;
    let _lock = ControlLock::open(paths)?;
    let mut preferences = read_preferences(paths)?;
    preferences.endpoint = endpoint;
    write_json(&paths.prefs(), &preferences)?;
    Ok(LauncherSettings {
        endpoint: preferences.endpoint.as_str().to_owned(),
    })
}

/// The caller owns the native file chooser; this validates and stores one client
/// executable path. Preferences never contain commands or arguments.
pub fn save_client_path(client: Client, executable: impl AsRef<Path>) -> Result<Snapshot> {
    let paths = Paths::production()?;
    let executable = validate_client(executable.as_ref(), client)?;
    let _lock = ControlLock::open(&paths)?;
    if let Some(session) = read_session(&paths)? {
        if session_is_live(&session)? {
            return Err("遊戲執行期間不能變更安裝路徑".into());
        }
    }
    let mut preferences = read_preferences(&paths)?;
    match client {
        Client::Stable => preferences.stable = Some(executable),
        Client::Lazer => preferences.lazer = Some(executable),
    }
    write_json(&paths.prefs(), &preferences)?;
    drop(_lock);
    snapshot_at(&paths)
}

/// A click authorizes gracefully restarting the exact selected installation.
/// No unrelated process or official-profile data is modified.
pub fn start(client: Client) -> Result<Snapshot> {
    let paths = Paths::production()?;
    start_session(&paths, client)?;
    snapshot_at(&paths)
}

/// The CLI stays attached until the exact new session reaches a verified
/// outcome. The GUI continues using the asynchronous `start` API.
pub fn start_and_wait(client: Client) -> Result<Snapshot> {
    let paths = Paths::production()?;
    let nonce = start_session(&paths, client)?;
    wait_for_start(|| matching_session(&paths, &nonce), CLI_START_TIMEOUT)?;
    snapshot_at(&paths)
}

fn wait_for_start(mut read: impl FnMut() -> Result<Session>, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let session = read()?;
        match session.phase {
            Phase::Running => return Ok(()),
            Phase::Error | Phase::Idle if session_completed(&session) => return Ok(()),
            Phase::CleanupFailed if session.guardian.is_none() && session.child.is_none() => {
                return Ok(())
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            if session.client == Client::Lazer {
                return Err("CLI 啟動確認逾時；lazer 尚未確認首個遊戲畫面，工作階段會繼續等待。請執行 launcher status 查看狀態".into());
            }
            return Err(
                "CLI 啟動確認逾時；尚未確認遊戲安全啟動。請執行 launcher status 查看工作階段"
                    .into(),
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn start_session(paths: &Paths, client: Client) -> Result<String> {
    let preferences = read_preferences(paths)?;
    if client == Client::Stable {
        preferences.endpoint.stable_domain()?;
    }
    let selected =
        selected_client_path(paths, &preferences, client).ok_or("請先選擇 osu!.exe 安裝路徑")?;
    let selected = validate_client(&selected, client)?;
    if client == Client::Lazer {
        verify_lazer_compatibility(paths, &selected)?;
    }
    if read_session(paths)?
        .as_ref()
        .map(session_is_live)
        .transpose()?
        .unwrap_or(false)
    {
        stop_at(paths)?;
        wait_for_session_end(paths, CLOSE_TIMEOUT + Duration::from_secs(5))?;
    }
    recover_at(paths)?;
    // All shutdown identity checks happen before a ruleset is installed.
    for running in process::find_by_executable(&selected)? {
        process::close_gracefully(&running, CLOSE_TIMEOUT)?;
    }
    let library = if client == Client::Lazer {
        refuse_other_lazer_instances()?;
        Some(resolve_library(paths, &selected)?)
    } else {
        None
    };
    let _lock = ControlLock::open(paths)?;
    if read_session(paths)?
        .as_ref()
        .map(session_is_live)
        .transpose()?
        .unwrap_or(false)
    {
        return Err("另一個啟動器已建立遊戲工作階段".into());
    }
    let nonce = fresh_nonce()?;
    let session = Session {
        version: 3,
        nonce: nonce.clone(),
        client,
        endpoint: Some(preferences.endpoint.clone()),
        selected_identity: process::executable_identity(&selected)?,
        selected_executable: selected,
        launcher_identity: process::executable_identity(&paths.launcher)?,
        launcher_executable: paths.launcher.clone(),
        guardian: None,
        child: None,
        install_session: None,
        library,
        legacy_ready: false,
        phase: Phase::Preparing,
        detail: "正在準備 Illyaverse 連線".into(),
        stop_requested: false,
        failure: None,
        host_status: None,
        child_exit: None,
        last_progress: None,
    };
    write_json(&paths.session(), &session)?;
    let mut command = Command::new(&paths.launcher);
    command.arg("__launch-session").arg(&nonce);
    hidden_child(&mut command);
    if let Err(error) = command.spawn() {
        let mut failed = session;
        failed.phase = Phase::Error;
        failed.detail = "無法啟動獨立的遊戲清理守護程序".into();
        write_json(&paths.session(), &failed)?;
        return Err(format!("無法啟動遊戲守護程序：{error}"));
    }
    drop(_lock);
    Ok(nonce)
}

pub fn stop() -> Result<Snapshot> {
    let paths = Paths::production()?;
    stop_at(&paths)?;
    snapshot_at(&paths)
}

/// Recovery refuses to remove an installed bridge while its recorded guardian or
/// child is alive, and preserves ambiguous ownership instead of closing a PID.
pub fn recover() -> Result<Snapshot> {
    let paths = Paths::production()?;
    recover_at(&paths)?;
    snapshot_at(&paths)
}

/// Invoke before creating any CLI or Tauri UI. Arguments exclude the executable.
pub fn handle_internal(arguments: &[String]) -> Option<Result<i32>> {
    let operation = arguments.first()?.as_str();
    if operation != "__launch-session" && operation != "__host-lazer" {
        return None;
    }
    Some((|| {
        if arguments.len() != 2 || !valid_nonce(&arguments[1]) {
            return Err("內部啟動器命令缺少有效的工作階段識別碼".into());
        }
        let paths = Paths::production()?;
        match operation {
            "__launch-session" => run_guardian(&paths, &arguments[1]),
            "__host-lazer" => run_lazer_host(&paths, &arguments[1]),
            _ => unreachable!(),
        }
    })())
}

fn snapshot_at(paths: &Paths) -> Result<Snapshot> {
    let preferences = read_preferences(paths)?;
    let stable = selected_client_path(paths, &preferences, Client::Stable);
    let session = read_session(paths)?;
    let selected_running = |client: Client| -> Result<bool> {
        let path = match client {
            Client::Stable => stable.as_deref(),
            Client::Lazer => preferences.lazer.as_deref(),
        };
        if let Some(path) = path {
            if path.exists() && !process::find_by_executable(path)?.is_empty() {
                return Ok(true);
            }
        }
        if let Some(session) = &session {
            if session.client == client {
                if let Some(child) = &session.child {
                    return Ok(process::state(child)? == process::State::Live);
                }
            }
        }
        Ok(false)
    };
    let compatibility = match &preferences.lazer {
        None => Compatibility::Unknown,
        Some(path) => {
            if validate_client(path, Client::Lazer).is_ok()
                && verify_lazer_compatibility(paths, path).is_ok()
            {
                Compatibility::Supported
            } else {
                Compatibility::Unsupported
            }
        }
    };
    let current_library = session
        .as_ref()
        .filter(|record| !session_completed(record))
        .and_then(|record| record.library.clone())
        .or_else(|| {
            preferences
                .lazer
                .as_deref()
                .and_then(|path| resolve_library(paths, path).ok())
        });
    let plugin_present = match current_library {
        Some(library) => path_exists(&library.rulesets().join(RULESET_FILENAME))?,
        None => path_exists(&paths.legacy_data.join("rulesets").join(RULESET_FILENAME))?,
    };
    Ok(Snapshot {
        stable: StableSnapshot {
            path: stable.as_deref().map(display_path),
            running: selected_running(Client::Stable)?,
        },
        lazer: LazerSnapshot {
            path: preferences.lazer.as_deref().map(display_path),
            running: selected_running(Client::Lazer)?,
            compatibility,
        },
        phase: session
            .as_ref()
            .map(|session| session.phase)
            .unwrap_or(Phase::Idle),
        detail: session
            .as_ref()
            .map(|session| session.detail.clone())
            .unwrap_or_else(|| {
                if stable.is_some() || preferences.lazer.is_some() {
                    "遊戲位置已確認，可以啟動。".into()
                } else {
                    "未找到 stable 預設安裝位置，請選擇遊戲安裝路徑。".into()
                }
            }),
        plugin_present,
        active_endpoint: session
            .as_ref()
            .filter(|record| !session_completed(record))
            .map(|record| record.endpoint().as_str().to_owned()),
    })
}

fn selected_client_path(
    paths: &Paths,
    preferences: &Preferences,
    client: Client,
) -> Option<PathBuf> {
    if let Some(selected) = preferences.path(client) {
        // An explicit selection remains authoritative, including when its
        // installation has moved. Never silently switch to a different client.
        return Some(selected.to_path_buf());
    }
    if client == Client::Stable {
        // Stable's standard per-user installation. Inspect only this executable;
        // discovery neither scans game data nor persists an inferred preference.
        return validate_client(&paths.local.join("osu!").join("osu!.exe"), client).ok();
    }
    None
}

fn stop_at(paths: &Paths) -> Result<()> {
    let mut session = {
        let _lock = ControlLock::open(paths)?;
        let Some(mut session) = read_session(paths)? else {
            return Err("目前沒有受管理的遊戲工作階段".into());
        };
        session.stop_requested = true;
        session.phase = Phase::Closing;
        session.detail = "正在等待遊戲安全關閉".into();
        write_json(&paths.session(), &session)?;
        session
    };
    if let Some(child) = &session.child {
        match process::state(child)? {
            process::State::Gone => {}
            process::State::Ambiguous => return Err("無法確認遊戲程序身分；未送出關閉訊息".into()),
            process::State::Live => {
                if let Err(error) = process::close_gracefully(child, CLOSE_TIMEOUT) {
                    session.phase = Phase::Error;
                    session.detail = error.clone();
                    let _lock = ControlLock::open(paths)?;
                    if read_session(paths)?.is_some_and(|current| {
                        current.nonce == session.nonce && current.phase == Phase::Closing
                    }) {
                        write_json(&paths.session(), &session)?;
                    }
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

fn wait_for_session_end(paths: &Paths, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if !read_session(paths)?
            .as_ref()
            .map(session_is_live)
            .transpose()?
            .unwrap_or(false)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("遊戲或清理守護程序仍在執行；尚未重新啟動".into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn session_is_live(session: &Session) -> Result<bool> {
    for identity in [&session.child, &session.guardian].into_iter().flatten() {
        match process::state(identity)? {
            process::State::Live => return Ok(true),
            process::State::Ambiguous => {
                return Err("工作階段程序身分無法確認；請先關閉遊戲並檢查狀態".into())
            }
            process::State::Gone => {}
        }
    }
    Ok(false)
}

fn session_completed(session: &Session) -> bool {
    matches!(session.phase, Phase::Idle | Phase::Error)
        && session.install_session.is_none()
        && session.child.is_none()
        && session.guardian.is_none()
        && !session.legacy_ready
}

fn recover_at(paths: &Paths) -> Result<()> {
    let _lock = ControlLock::open(paths)?;
    let session = read_session(paths)?;
    if session
        .as_ref()
        .map(session_is_live)
        .transpose()?
        .unwrap_or(false)
    {
        return Err("遊戲或其守護程序仍在執行；不會移除正在使用的連線模組".into());
    }
    let completed = session.as_ref().is_some_and(session_completed);
    let library = match &session {
        Some(_) if completed => None,
        Some(record) => record.library.clone(),
        None => {
            let preferences = read_preferences(paths)?;
            if let Some(selected) = preferences.lazer.as_deref() {
                Some(resolve_library(paths, selected)?)
            } else if path_exists(&paths.legacy_data)? {
                Some(Library::open(&paths.legacy_data)?)
            } else {
                None
            }
        }
    };
    if let Some(library) = library {
        library.validate()?;
        let rulesets = library.rulesets();
        if path_exists(&rulesets)? {
            let mut managed = ManagedRuleset::open(&rulesets)?;
            let journal = managed.active_session()?;
            if journal.is_some() {
                refuse_other_lazer_instances()?;
            }
            if journal.is_some() && session.is_none() {
                return Err("連線模組有復原紀錄，但缺少程序所有權紀錄；檔案已保留".into());
            }
            if let (Some(owned), Some(receipt)) = (
                session.as_ref().and_then(|value| value.install_session),
                journal.as_ref(),
            ) {
                if owned != receipt.session_id {
                    return Err("連線模組的工作階段身分不符；檔案已保留".into());
                }
            }
            if let Err(error) = managed.recover() {
                if let Some(mut failed) = session.clone() {
                    failed.phase = Phase::CleanupFailed;
                    failed.detail = error.clone();
                    write_json(&paths.session(), &failed)?;
                }
                return Err(error);
            }
        }
    }
    if let Some(mut session) = session {
        cleanup_session_ready(paths, &session)?;
        cleanup_failure(paths, &session.nonce)?;
        cleanup_progress(paths, &session.nonce, session.endpoint())?;
        session.phase = Phase::Idle;
        session.detail = "已關閉遊戲並移除 Illyaverse 連線模組".into();
        session.guardian = None;
        session.child = None;
        session.install_session = None;
        session.legacy_ready = false;
        session.stop_requested = false;
        session.failure = None;
        session.host_status = None;
        session.child_exit = None;
        session.last_progress = None;
        write_json(&paths.session(), &session)?;
    }
    Ok(())
}

fn run_guardian(paths: &Paths, nonce: &str) -> Result<i32> {
    let _guardian_lock = exclusive_file(&paths.control.join(".guardian.lock"))?;
    let mut session = {
        let _lock = ControlLock::open(paths)?;
        let mut session = matching_session(paths, nonce)?;
        if session.phase != Phase::Preparing
            || session.guardian.is_some()
            || session.child.is_some()
        {
            return Err("這個工作階段已被使用或取消".into());
        }
        let current = process::current_identity()?;
        if current.executable != session.launcher_executable
            || current.executable_identity != session.launcher_identity
        {
            return Err("守護程序並非此工作階段的啟動器".into());
        }
        session.guardian = Some(current);
        write_json(&paths.session(), &session)?;
        session
    };
    let mut managed: Option<ManagedRuleset> = None;
    let mut child: Option<Child> = None;
    let mut result = run_client(paths, &mut session, &mut managed, &mut child);
    if let Err(error) = &result {
        let _ = update_session(paths, nonce, |record| {
            record.phase = Phase::Error;
            record.detail = error.clone();
        });
    }
    // Never clean up on an error path while a spawned child remains alive.
    if let Some(child) = &mut child {
        let status = match child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => child
                .wait()
                .map_err(|_| "無法確認遊戲關閉；復原紀錄已保留")?,
            Err(_) => return Err("遊戲狀態無法確認；連線模組與復原紀錄已保留".into()),
        };
        capture_closed_child(paths, nonce, session.client, status.code(), &mut result)?;
    }
    update_session(paths, nonce, |record| {
        record.phase = Phase::Cleaning;
        record.detail = "正在移除連線模組".into();
    })?;
    let cleanup = match &mut managed {
        Some(managed) => session
            .library
            .as_ref()
            .ok_or("清理缺少原有圖譜資料夾紀錄".into())
            .and_then(Library::validate)
            .and_then(|()| refuse_other_lazer_instances())
            .and_then(|()| managed.cleanup().map(|_| ())),
        None => Ok(()),
    }
    .and_then(|()| cleanup_ready(paths, nonce, session.endpoint()))
    .and_then(|()| cleanup_failure(paths, nonce))
    .and_then(|()| cleanup_progress(paths, nonce, session.endpoint()));
    let _lock = ControlLock::open(paths)?;
    let mut current = matching_session(paths, nonce)?;
    current.guardian = None;
    current.child = None;
    match cleanup {
        Ok(()) => {
            current.install_session = None;
            current.phase = if result.is_ok() {
                Phase::Idle
            } else {
                Phase::Error
            };
            current.detail = result.as_ref().err().cloned().unwrap_or_else(|| {
                with_progress(
                    "已關閉遊戲並移除 Illyaverse 連線模組".into(),
                    current.last_progress,
                )
            });
        }
        Err(error) => {
            current.phase = Phase::CleanupFailed;
            let detail = match &result {
                Err(failure) => format!("{failure}；清理失敗：{error}"),
                Ok(()) => error,
            };
            current.detail = retained_cleanup_detail(detail, &current);
        }
    }
    write_json(&paths.session(), &current)?;
    Ok(if result.is_ok() && current.phase != Phase::CleanupFailed {
        0
    } else {
        1
    })
}

fn run_client(
    paths: &Paths,
    session: &mut Session,
    managed: &mut Option<ManagedRuleset>,
    child: &mut Option<Child>,
) -> Result<()> {
    let selected = validate_client(&session.selected_executable, session.client)?;
    if process::executable_identity(&selected)? != session.selected_identity {
        return Err("選擇的遊戲執行檔已變更；請重新選擇路徑".into());
    }
    if session.client == Client::Lazer {
        verify_lazer_compatibility(paths, &selected)?;
        refuse_other_lazer_instances()?;
        session
            .library
            .as_ref()
            .ok_or("lazer 缺少原有圖譜資料夾紀錄")?
            .validate()?;
    }
    if matching_session(paths, &session.nonce)?.stop_requested {
        return Err("啟動已取消".into());
    }
    if !process::find_by_executable(&selected)?.is_empty() {
        return Err("選擇的遊戲再次啟動；尚未安裝連線模組".into());
    }
    let mut command;
    match session.client {
        Client::Stable => {
            command = Command::new(&selected);
            command
                .arg("-devserver")
                .arg(session.endpoint().stable_domain()?);
        }
        Client::Lazer => {
            let (bridge, hash) = verify_bridge(paths)?;
            let library = session
                .library
                .as_ref()
                .ok_or("lazer 缺少原有圖譜資料夾紀錄")?;
            library.validate()?;
            ensure_directory(&library.rulesets())?;
            *managed = Some(ManagedRuleset::open(library.rulesets())?);
            library.validate()?;
            let receipt = managed
                .as_mut()
                .expect("installation guard exists")
                .install(&bridge, &hash)?;
            session.install_session = Some(receipt.session_id);
            // Keep the guard even if installing or persisting the receipt fails.
            update_session(paths, &session.nonce, |record| {
                record.install_session = session.install_session
            })?;
            command = Command::new(&paths.launcher);
            command.arg("__host-lazer").arg(&session.nonce);
            command.env(
                "ILLYAVERSE_LAZER_INSTALL_ROOT",
                selected.parent().ok_or("lazer 安裝路徑缺少父資料夾")?,
            );
            command.env("ILLYAVERSE_LAZER_DATA_ROOT", &library.root);
            command.env("ILLYAVERSE_LAZER_CONTROL_ROOT", &paths.control);
            command.env("ILLYAVERSE_LAZER_SESSION", &session.nonce);
            command.env("ILLYAVERSE_LAZER_READY_FILE", paths.ready(&session.nonce));
            command.env(
                "ILLYAVERSE_LAZER_SERVER_ENDPOINT",
                session.endpoint().as_str(),
            );
            command.env("OSU_DISABLE_ERROR_REPORTING", "1");
            command.env("OSU_EXTERNAL_UPDATE_PROVIDER", "Illyaverse");
        }
    }
    sanitize_injection_environment(&mut command);
    hidden_child(&mut command);
    let spawned = command.spawn().map_err(|_| "無法啟動選擇的遊戲")?;
    let pid = spawned.id();
    *child = Some(spawned);
    if session.client == Client::Stable {
        let identity = process::identity(pid)?.ok_or("stable 在啟動完成前已關閉")?;
        update_session(paths, &session.nonce, |record| {
            record.child = Some(identity);
            record.phase = Phase::Running;
            record.detail = "stable 已啟動，使用 Illyaverse 私服參數".into();
        })?;
    }
    let ready_deadline = Instant::now() + READY_TIMEOUT;
    let mut guard_verified = session.client == Client::Stable;
    let mut started = session.client == Client::Stable;
    loop {
        let status = child
            .as_mut()
            .expect("child exists after spawn")
            .try_wait()
            .map_err(|_| "無法查詢遊戲狀態")?;
        if let Some(status) = status {
            return finish_child_exit(paths, session, started, status.success(), status.code());
        }
        let record = matching_session(paths, &session.nonce)?;
        if session.client == Client::Lazer && !started {
            if let Some(failure) = read_failure(
                paths,
                &session.nonce,
                session.endpoint(),
                session.version < 3,
            )? {
                let detail = failure_detail(&failure, record.host_status, None);
                update_session(paths, &session.nonce, |record| {
                    record.failure = Some(failure)
                })?;
                return Err(detail);
            }
            if !guard_verified {
                if let Some(signal) = read_ready(paths, &session.nonce)? {
                    validate_ready(&signal, &session.nonce, session.endpoint())?;
                    if record.child.as_ref().map(process::state).transpose()?
                        != Some(process::State::Live)
                    {
                        return Err("安全啟動訊號缺少有效的遊戲程序身分".into());
                    }
                    guard_verified = true;
                } else if Instant::now() >= ready_deadline {
                    return Err("未收到安全啟動確認；關閉遊戲後才會清理連線模組".into());
                }
            }
            let progress = read_progress(paths, &session.nonce, session.endpoint())?;
            let child_state = record.child.as_ref().map(process::state).transpose()?;
            let _lock = ControlLock::open(paths)?;
            let mut current = matching_session(paths, &session.nonce)?;
            let previous = (current.phase, current.detail.clone(), current.last_progress);
            started = apply_lazer_progress(&mut current, guard_verified, progress, child_state)?;
            if (current.phase, &current.detail, current.last_progress)
                != (previous.0, &previous.1, previous.2)
            {
                write_json(&paths.session(), &current)?;
            }
        }
        if record.stop_requested {
            if let Some(identity) = &record.child {
                if process::state(identity)? == process::State::Live {
                    // The stop caller also requests closure; a duplicate WM_CLOSE
                    // is harmless and ownership is revalidated before each post.
                    process::close_gracefully(identity, CLOSE_TIMEOUT)?;
                }
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn apply_lazer_progress(
    session: &mut Session,
    guard_verified: bool,
    progress: Option<ProgressStage>,
    child_state: Option<process::State>,
) -> Result<bool> {
    session.last_progress = session.last_progress.max(progress);
    let first_draw = session.last_progress == Some(ProgressStage::FirstSuccessfulDraw);
    if guard_verified
        && first_draw
        && (session.child.is_none() || child_state != Some(process::State::Live))
    {
        return Err("首個畫面訊號缺少有效的遊戲程序身分；不會接受啟動結果".into());
    }
    let started = guard_verified
        && first_draw
        && session.child.is_some()
        && child_state == Some(process::State::Live);
    if !session.stop_requested && session.phase == Phase::Preparing {
        if started {
            session.phase = Phase::Running;
            session.detail = with_progress(
                "lazer 首個畫面已繪製，已使用 Illyaverse 私服設定".into(),
                session.last_progress,
            );
        } else if guard_verified {
            session.detail = with_progress(
                "lazer 啟動防護驗證完成；正在等待首個遊戲畫面".into(),
                session.last_progress,
            );
        } else if session.last_progress.is_some() {
            session.detail =
                with_progress("正在等待 lazer 啟動防護驗證".into(), session.last_progress);
        }
    }
    Ok(started && !session.stop_requested)
}

fn with_progress(mut detail: String, progress: Option<ProgressStage>) -> String {
    for stage in ProgressStage::ALL {
        detail = detail.replace(&format!(" last_progress={}", stage.name()), "");
    }
    if let Some(stage) = progress {
        detail.push_str(" last_progress=");
        detail.push_str(stage.name());
    }
    detail
}

fn retained_cleanup_detail(mut detail: String, session: &Session) -> String {
    if session.client == Client::Lazer {
        detail = with_progress(detail, session.last_progress);
        if let Some(exit) = session.child_exit {
            if !detail.contains(" child_exit=") {
                detail.push_str(&format!(" child_exit={exit}"));
            }
        }
    }
    detail
}

fn finish_child_exit(
    paths: &Paths,
    session: &Session,
    ready: bool,
    success: bool,
    exit_code: Option<i32>,
) -> Result<()> {
    // Consume the fixed safe failure before cleanup removes its sidecar. Raw
    // game stdout/stderr never enters this record or the public snapshot.
    let failure = if session.client == Client::Lazer {
        read_failure(
            paths,
            &session.nonce,
            session.endpoint(),
            session.version < 3,
        )?
    } else {
        None
    };
    let progress = if session.client == Client::Lazer {
        read_progress(paths, &session.nonce, session.endpoint())?
    } else {
        None
    };
    let _lock = ControlLock::open(paths)?;
    let mut record = matching_session(paths, &session.nonce)?;
    record.child_exit = exit_code;
    record.failure = merge_failure(record.failure, failure)?;
    record.last_progress = record.last_progress.max(progress);
    let detail = if let Some(failure) = &record.failure {
        Some(failure_detail(failure, record.host_status, exit_code))
    } else if !ready || !success {
        let code = if !ready {
            "child_exit_before_first_draw_verification"
        } else {
            "child_exit_failed"
        };
        Some(format!(
            "遊戲啟動失敗：stage=child code={code} child_exit={}",
            exit_detail(exit_code)
        ))
    } else {
        None
    };
    let detail = detail.map(|detail| with_progress(detail, record.last_progress));
    if let Some(detail) = &detail {
        record.phase = Phase::Error;
        record.detail = detail.clone();
    }
    write_json(&paths.session(), &record)?;
    detail.map_or(Ok(()), Err)
}

fn merge_failure(persisted: Option<Failure>, observed: Option<Failure>) -> Result<Option<Failure>> {
    if let (Some(persisted), Some(observed)) = (&persisted, &observed) {
        if persisted != observed {
            return Err("lazer 失敗紀錄互相衝突；不會接受啟動結果".into());
        }
    }
    Ok(observed.or(persisted))
}

fn capture_closed_child(
    paths: &Paths,
    nonce: &str,
    client: Client,
    exit: Option<i32>,
    result: &mut Result<()>,
) -> Result<()> {
    // A startup-hook failure may have been published after an earlier timeout,
    // or the first read may have raced its create/write. Re-read only after the
    // owned child has exited, and persist the strict safe record before cleanup.
    let expected_session = matching_session(paths, nonce)?;
    let mut rejected = false;
    let observed = if client == Client::Lazer {
        match read_failure(
            paths,
            nonce,
            expected_session.endpoint(),
            expected_session.version < 3,
        ) {
            Ok(failure) => failure,
            Err(error) => {
                *result = Err(error);
                rejected = true;
                None
            }
        }
    } else {
        None
    };
    let progress = if client == Client::Lazer {
        match read_progress(paths, nonce, expected_session.endpoint()) {
            Ok(progress) => progress,
            Err(error) => {
                *result = Err(error);
                rejected = true;
                None
            }
        }
    } else {
        None
    };
    let _lock = ControlLock::open(paths)?;
    let mut record = matching_session(paths, nonce)?;
    match merge_failure(record.failure.clone(), observed) {
        Ok(failure) => record.failure = failure,
        Err(error) => {
            *result = Err(error);
            rejected = true;
        }
    }
    record.child_exit = exit;
    record.last_progress = record.last_progress.max(progress);
    if rejected {
        record.detail = result.as_ref().unwrap_err().clone();
    } else if let Some(failure) = &record.failure {
        let detail = failure_detail(failure, record.host_status, exit);
        record.detail = detail.clone();
        *result = Err(detail);
    }
    if let Err(detail) = result {
        let mut detail = detail.clone();
        if !detail.contains(" child_exit=") {
            detail.push_str(&format!(" child_exit={}", exit_detail(exit)));
        }
        detail = with_progress(detail, record.last_progress);
        record.detail = detail.clone();
        *result = Err(detail);
    }
    write_json(&paths.session(), &record)
}

fn exit_detail(code: Option<i32>) -> String {
    code.map(|code| code.to_string())
        .unwrap_or_else(|| "unavailable".into())
}

fn failure_detail(failure: &Failure, host_status: Option<i32>, exit: Option<i32>) -> String {
    let mut detail = format!(
        "lazer 啟動失敗：stage={} code={} exception={}",
        failure.stage, failure.code, failure.exception
    );
    if let Some(status) = host_status {
        detail.push_str(&format!(" runtime_status={status}"));
    }
    if let Some(exit) = exit {
        detail.push_str(&format!(" child_exit={exit}"));
    }
    detail
}

fn run_lazer_host(paths: &Paths, nonce: &str) -> Result<i32> {
    if std::env::var("ILLYAVERSE_LAZER_SESSION").ok().as_deref() != Some(nonce) {
        return Err("缺少受管理的 lazer 工作階段環境".into());
    }
    let session = {
        let _lock = ControlLock::open(paths)?;
        let mut session = matching_session(paths, nonce)?;
        if session.client != Client::Lazer
            || session.stop_requested
            || session.phase != Phase::Preparing
            || session.child.is_some()
        {
            return Err("lazer 工作階段已取消或已被使用".into());
        }
        let guardian = session.guardian.as_ref().ok_or("lazer 缺少清理守護程序")?;
        if process::state(guardian)? != process::State::Live {
            return Err("清理守護程序已結束；不會進入遊戲 Main".into());
        }
        let own = process::current_identity()?;
        if own.executable != session.launcher_executable
            || own.executable_identity != session.launcher_identity
        {
            return Err("lazer 主機程序身分不符".into());
        }
        session.child = Some(own);
        // Persist identity BEFORE executing any managed startup hook or Main.
        write_json(&paths.session(), &session)?;
        session
    };
    let preflight = (|| -> Result<(PathBuf, PathBuf)> {
        let selected = validate_client(&session.selected_executable, Client::Lazer)?;
        verify_lazer_compatibility(paths, &selected)?;
        let install = selected.parent().ok_or("lazer 缺少安裝資料夾")?;
        let library = session
            .library
            .as_ref()
            .ok_or("lazer 缺少原有圖譜資料夾紀錄")?;
        library.validate()?;
        refuse_other_lazer_instances()?;
        validate_child_environment(paths, nonce, install, library, session.endpoint())?;
        if path_exists(&paths.failure(nonce))? {
            return Err("lazer 工作階段已有失敗訊號；不會進入 Main".into());
        }
        for stage in ProgressStage::ALL {
            if path_exists(&paths.progress(nonce, stage))? {
                return Err("lazer 工作階段已有進度訊號；不會進入 Main".into());
            }
        }
        if !progress_temporary_paths(paths, nonce)?.is_empty() {
            return Err("lazer 工作階段已有進度暫存訊號；不會進入 Main".into());
        }
        let (_, hash) = verify_bridge(paths)?;
        let bridge = library.rulesets().join(RULESET_FILENAME);
        if artifact_sha256(&bridge)? != hash {
            return Err("已安裝的連線模組與發行雜湊不符；不會進入 Main".into());
        }
        Ok((install.to_path_buf(), bridge))
    })();
    let (install, bridge) = match preflight {
        Ok(paths) => paths,
        Err(_) => return persist_host_failure(paths, nonce, "host_preflight_failed", None),
    };
    match host::run_managed(
        &install.join("osu!.dll"),
        &install.join("hostfxr.dll"),
        &bridge,
    ) {
        Ok(code) => Ok(code),
        Err(failure) => persist_host_failure(paths, nonce, failure.code, failure.status),
    }
}

fn persist_host_failure(
    paths: &Paths,
    nonce: &str,
    code: &str,
    status: Option<i32>,
) -> Result<i32> {
    // Reached only after the host persisted its validated current identity.
    let _lock = ControlLock::open(paths)?;
    let mut session = matching_session(paths, nonce)?;
    if session.child.as_ref() != Some(&process::current_identity()?) {
        return Err("lazer 失敗紀錄缺少受管理的主機程序身分".into());
    }
    let failure = Failure {
        protocol: 1,
        session: nonce.to_owned(),
        stage: "host".into(),
        code: code.to_owned(),
        exception: "native_host".into(),
        endpoint: FailureEndpoint::Selected(session.endpoint().as_str().to_owned()),
        fixture: false,
    };
    validate_failure(&failure, nonce, session.endpoint(), session.version < 3)?;
    let file = paths.failure(nonce);
    check_components(&paths.control)?;
    let mut writer = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(file)
        .map_err(|_| "無法安全建立 lazer 主機失敗紀錄")?;
    let bytes = serde_json::to_vec(&failure).map_err(|_| "無法編碼 lazer 主機失敗紀錄")?;
    writer
        .write_all(&bytes)
        .and_then(|()| writer.sync_all())
        .map_err(|_| "無法同步 lazer 主機失敗紀錄")?;
    session.failure = Some(failure);
    session.host_status = status;
    session.detail = failure_detail(
        session.failure.as_ref().expect("failure exists"),
        status,
        None,
    );
    write_json(&paths.session(), &session)?;
    Err(session.detail)
}

fn validate_child_environment(
    paths: &Paths,
    nonce: &str,
    install: &Path,
    library: &Library,
    endpoint: &Endpoint,
) -> Result<()> {
    for (name, expected) in [
        ("ILLYAVERSE_LAZER_INSTALL_ROOT", install.to_path_buf()),
        ("ILLYAVERSE_LAZER_DATA_ROOT", library.root.clone()),
        ("ILLYAVERSE_LAZER_CONTROL_ROOT", paths.control.clone()),
        ("ILLYAVERSE_LAZER_READY_FILE", paths.ready(nonce)),
    ] {
        if std::env::var_os(name).map(PathBuf::from).as_ref() != Some(&expected) {
            return Err("lazer 工作階段路徑環境不符".into());
        }
    }
    if std::env::var("ILLYAVERSE_LAZER_SERVER_ENDPOINT")
        .ok()
        .as_deref()
        != Some(endpoint.as_str())
    {
        return Err("lazer 工作階段伺服器環境不符；不會進入 Main".into());
    }
    Ok(())
}

fn update_session(paths: &Paths, nonce: &str, update: impl FnOnce(&mut Session)) -> Result<()> {
    let _lock = ControlLock::open(paths)?;
    let mut session = matching_session(paths, nonce)?;
    update(&mut session);
    write_json(&paths.session(), &session)
}

fn matching_session(paths: &Paths, nonce: &str) -> Result<Session> {
    let session = read_session(paths)?.ok_or("缺少工作階段復原紀錄")?;
    if session.nonce != nonce {
        return Err("工作階段識別碼不符".into());
    }
    Ok(session)
}

fn read_preferences(paths: &Paths) -> Result<Preferences> {
    let preferences: Preferences = read_json(&paths.prefs())?.unwrap_or_default();
    for path in [&preferences.stable, &preferences.lazer]
        .into_iter()
        .flatten()
    {
        if !path.is_absolute()
            || path.as_os_str().len() > 32768
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err("安裝路徑設定含有無效路徑".into());
        }
    }
    Ok(preferences)
}

fn read_session(paths: &Paths) -> Result<Option<Session>> {
    let mut session: Option<Session> = read_json(&paths.session())?;
    if let Some(session) = &mut session {
        if !matches!(session.version, 1..=3)
            || !valid_nonce(&session.nonce)
            || session.detail.len() > 4096
            || !session.selected_executable.is_absolute()
            || !session.launcher_executable.is_absolute()
            || session.selected_identity.len() > 128
            || session.launcher_identity.len() > 128
            || !session
                .selected_executable
                .file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("osu!.exe"))
            || session
                .selected_executable
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err("工作階段復原紀錄無效；現有檔案已保留".into());
        }
        match (&session.endpoint, session.version) {
            (None, 1 | 2) => session.endpoint = Some(Endpoint::default()),
            (Some(endpoint), 1 | 2) if endpoint == &Endpoint::default() => {}
            (Some(_), 3) => {}
            _ => return Err("工作階段缺少一致的伺服器紀錄；現有檔案已保留".into()),
        }
        if session.client == Client::Stable {
            session.endpoint().stable_domain()?;
        }
        // The recorded launcher belongs to the historical session. Moving or
        // upgrading the current package must not prevent status or recovery;
        // guardian/host entry and live process ownership still match that record.
        if session.version == 1 {
            if session.library.is_some() {
                return Err("舊版工作階段不得指定另一個圖譜資料夾".into());
            }
            if session.client == Client::Lazer && path_exists(&paths.legacy_data)? {
                session.library = Some(Library::open(&paths.legacy_data)?);
            } else if session.install_session.is_some() {
                return Err("舊版私有圖譜資料夾已不見；復原紀錄已保留".into());
            } else if session.client == Client::Lazer {
                // No historical root, owned install or process means the old
                // guardian never reached installation. Permit journal recovery.
                if session.child.is_some() || session.guardian.is_some() {
                    return Err("舊版遊戲工作階段缺少原有資料夾；紀錄已保留".into());
                }
                session.phase = Phase::Error;
            }
            session.legacy_ready = session.client == Client::Lazer;
            session.version = 2;
        }
        if session.legacy_ready
            && (session.client != Client::Lazer
                || session.library.as_ref().is_some_and(|library| {
                    library.root != fs::canonicalize(&paths.legacy_data).unwrap_or_default()
                }))
        {
            return Err("舊版啟動訊號不得指定另一個資料夾".into());
        }
        match (&session.client, &session.library) {
            (Client::Lazer, Some(library)) => {
                library.validate_record()?;
            }
            (Client::Lazer, None)
                if session.install_session.is_none()
                    && matches!(session.phase, Phase::Idle | Phase::Error) => {}
            (Client::Stable, None) => {}
            _ => return Err("工作階段缺少一致的圖譜資料夾紀錄；檔案已保留".into()),
        }
        for (identity, guardian) in [(&session.guardian, true), (&session.child, false)] {
            let Some(identity) = identity else {
                continue;
            };
            if identity.pid == 0
                || identity.creation_time == 0
                || identity.executable_identity.len() > 128
                || if guardian || session.client == Client::Lazer {
                    identity.executable != session.launcher_executable
                        || identity.executable_identity != session.launcher_identity
                } else {
                    identity.executable != session.selected_executable
                        || identity.executable_identity != session.selected_identity
                }
            {
                return Err("工作階段程序所有權紀錄無效".into());
            }
        }
        if let Some(failure) = &session.failure {
            validate_failure(
                failure,
                &session.nonce,
                session.endpoint(),
                session.version < 3,
            )?;
            if session.client != Client::Lazer {
                return Err("lazer 失敗紀錄不得屬於另一種遊戲工作階段".into());
            }
        }
        if session.last_progress.is_some() && session.client != Client::Lazer {
            return Err("lazer 進度紀錄不得屬於另一種遊戲工作階段".into());
        }
        if session.host_status.is_some()
            && session
                .failure
                .as_ref()
                .is_none_or(|failure| failure.stage != "host")
        {
            return Err("原生主機狀態缺少一致的失敗紀錄".into());
        }
    }
    Ok(session)
}

fn read_ready(paths: &Paths, nonce: &str) -> Result<Option<Ready>> {
    if !valid_nonce(nonce) {
        return Err("lazer 安全啟動訊號的工作階段無效".into());
    }
    read_json(&paths.ready(nonce))
}
fn validate_ready(signal: &Ready, nonce: &str, endpoint: &Endpoint) -> Result<()> {
    if !valid_nonce(nonce)
        || signal.protocol != 1
        || signal.session != nonce
        || signal.status != "ready"
        || signal.endpoint != endpoint.as_str()
        || signal.fixture
    {
        return Err("lazer 安全啟動訊號不符合正式私服契約".into());
    }
    Ok(())
}

fn validate_progress(
    signal: &Progress,
    nonce: &str,
    stage: ProgressStage,
    endpoint: &Endpoint,
) -> Result<()> {
    if !valid_nonce(nonce)
        || signal.protocol != 1
        || signal.session != nonce
        || signal.stage != stage
        || signal.endpoint != endpoint.as_str()
        || signal.fixture
    {
        return Err("lazer 進度訊號不符合正式工作階段契約；現有檔案已保留".into());
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
enum ProgressRead {
    Absent,
    Publishing,
    Complete,
}

fn progress_io_pending(error: &std::io::Error) -> bool {
    cfg!(windows) && matches!(error.raw_os_error(), Some(32 | 33))
}

fn read_progress_file(
    path: &Path,
    nonce: &str,
    stage: ProgressStage,
    endpoint: &Endpoint,
) -> Result<ProgressRead> {
    if !valid_nonce(nonce) {
        return Err("lazer 進度紀錄的工作階段無效".into());
    }
    let invalid = "lazer 進度紀錄無效；不會接受啟動結果，現有檔案已保留";
    check_components(path.parent().ok_or(invalid)?)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProgressRead::Absent)
        }
        Err(error) if progress_io_pending(&error) => return Ok(ProgressRead::Publishing),
        Err(_) => return Err(invalid.into()),
    };
    check_metadata(&metadata)?;
    if !metadata.is_file() {
        return Err(invalid.into());
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if progress_io_pending(&error) => return Ok(ProgressRead::Publishing),
        Err(_) => return Err(invalid.into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_PROGRESS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid)?;
    if bytes.len() as u64 > MAX_PROGRESS_BYTES {
        return Err(invalid.into());
    }
    let signal: Progress = serde_json::from_slice(&bytes).map_err(|_| invalid)?;
    validate_progress(&signal, nonce, stage, endpoint)?;
    Ok(ProgressRead::Complete)
}

fn read_progress(paths: &Paths, nonce: &str, endpoint: &Endpoint) -> Result<Option<ProgressStage>> {
    if !valid_nonce(nonce) {
        return Err("lazer 進度紀錄的工作階段無效".into());
    }
    let mut latest = None;
    // Immutable milestones are seven exact paths, never paths from payloads.
    for stage in ProgressStage::ALL {
        if read_progress_file(&paths.progress(nonce, stage), nonce, stage, endpoint)?
            == ProgressRead::Complete
        {
            latest = Some(stage);
        }
    }
    Ok(latest)
}

fn progress_temporary_paths(paths: &Paths, nonce: &str) -> Result<Vec<(PathBuf, ProgressStage)>> {
    if !valid_nonce(nonce) {
        return Err("lazer 進度暫存紀錄的工作階段無效".into());
    }
    check_components(&paths.control)?;
    let prefix = format!("session-progress-{nonce}-");
    let mut files = Vec::new();
    for entry in fs::read_dir(&paths.control).map_err(|_| "無法檢查工作階段進度暫存紀錄")?
    {
        let entry = entry.map_err(|_| "無法檢查工作階段進度暫存紀錄")?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(suffix) = name
            .strip_prefix(&prefix)
            .and_then(|name| name.strip_suffix(".tmp"))
        else {
            continue;
        };
        let candidate = ProgressStage::ALL.into_iter().find_map(|stage| {
            suffix
                .strip_prefix(stage.name())
                .and_then(|suffix| suffix.strip_prefix('-'))
                .filter(|random| {
                    random.len() == 32
                        && random
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
                .map(|_| stage)
        });
        let stage = candidate.ok_or("lazer 進度暫存檔名無效；現有檔案已保留")?;
        files.push((paths.control.join(name), stage));
    }
    Ok(files)
}

fn cleanup_progress(paths: &Paths, nonce: &str, endpoint: &Endpoint) -> Result<()> {
    if !valid_nonce(nonce) {
        return Err("lazer 進度清理的工作階段無效".into());
    }
    let mut files = Vec::new();
    for stage in ProgressStage::ALL {
        let path = paths.progress(nonce, stage);
        match read_progress_file(&path, nonce, stage, endpoint)? {
            ProgressRead::Complete => files.push(path),
            ProgressRead::Publishing => return Err("lazer 進度紀錄尚未關閉；現有檔案已保留".into()),
            ProgressRead::Absent => {}
        }
    }
    for (path, stage) in progress_temporary_paths(paths, nonce)? {
        match read_progress_file(&path, nonce, stage, endpoint)? {
            ProgressRead::Complete => files.push(path),
            ProgressRead::Publishing => {
                return Err("lazer 進度暫存紀錄尚未關閉；現有檔案已保留".into())
            }
            ProgressRead::Absent => {}
        }
    }
    // Validate all candidate evidence before deleting any one of these files.
    // A partial, foreign or linked file remains available for inspection.
    for path in files {
        ordinary_file(&path)?;
        fs::remove_file(path).map_err(|_| "無法移除工作階段進度訊號")?;
    }
    Ok(())
}

fn read_failure(
    paths: &Paths,
    nonce: &str,
    endpoint: &Endpoint,
    allow_legacy: bool,
) -> Result<Option<Failure>> {
    if !valid_nonce(nonce) {
        return Err("lazer 失敗紀錄的工作階段無效".into());
    }
    let failure = read_json_limit(&paths.failure(nonce), MAX_FAILURE_BYTES)
        .map_err(|_| "lazer 失敗紀錄無效；不會接受啟動結果，現有檔案已保留")?;
    if let Some(failure) = &failure {
        validate_failure(failure, nonce, endpoint, allow_legacy)?;
    }
    Ok(failure)
}

fn validate_failure(
    failure: &Failure,
    nonce: &str,
    endpoint: &Endpoint,
    allow_legacy: bool,
) -> Result<()> {
    let code_allowed = match failure.stage.as_str() {
        "bootstrap" => matches!(
            failure.code.as_str(),
            "bootstrap_failed"
                | "server_endpoint_missing"
                | "server_endpoint_invalid"
                | "session_missing"
                | "install_root_missing"
                | "data_root_missing"
                | "control_root_missing"
                | "ready_file_missing"
                | "session_invalid"
                | "path_not_absolute"
                | "path_not_local"
                | "directory_missing"
                | "fixture_data_path_invalid"
                | "fixture_control_path_invalid"
                | "control_path_invalid"
                | "control_data_overlap"
                | "data_reparse_point"
                | "ready_path_invalid"
                | "ready_file_exists"
        ),
        "assemblies" => matches!(
            failure.code.as_str(),
            "assembly_load_failed"
                | "client_assembly_missing"
                | "client_assembly_location_mismatch"
        ),
        "contracts" => matches!(
            failure.code.as_str(),
            "contract_validation_failed"
                | "client_version_unverified"
                | "client_assembly_unverified"
                | "runtime_version_unverified"
                | "ruleset_api_incompatible"
                | "endpoint_contract_incompatible"
                | "endpoint_factory_incompatible"
                | "startup_progress_contract_incompatible"
        ),
        "network_guards" => failure.code == "network_guard_install_failed",
        "client_guards" => failure.code == "client_guard_install_failed",
        "relax_guards" => failure.code == "relax_guard_install_failed",
        "ready" => matches!(
            failure.code.as_str(),
            "ready_publish_failed" | "patch_not_installed"
        ),
        "host" => matches!(
            failure.code.as_str(),
            "host_preflight_failed"
                | "host_path_invalid"
                | "host_library_load_failed"
                | "host_api_missing"
                | "host_runtime_initialize_failed"
                | "host_startup_hook_configuration_failed"
                | "host_platform_unsupported"
        ),
        _ => false,
    };
    let exception_allowed = if failure.stage == "host" {
        failure.exception == "native_host"
    } else {
        matches!(
            failure.exception.as_str(),
            "TransfigurationStartupException"
                | "InvalidOperationException"
                | "MissingMethodException"
                | "MissingFieldException"
                | "TypeLoadException"
                | "FileNotFoundException"
                | "FileLoadException"
                | "BadImageFormatException"
                | "IOException"
                | "UnauthorizedAccessException"
                | "NotSupportedException"
                | "ArgumentException"
                | "HarmonyException"
                | "Exception"
        )
    };
    let endpoint_allowed = match &failure.endpoint {
        FailureEndpoint::Selected(origin) => origin == endpoint.as_str(),
        FailureEndpoint::Unavailable => {
            failure.stage == "bootstrap"
                && matches!(
                    failure.code.as_str(),
                    "server_endpoint_missing" | "server_endpoint_invalid"
                )
        }
        FailureEndpoint::LegacyMissing => allow_legacy && endpoint == &Endpoint::default(),
    };
    if !valid_nonce(nonce)
        || failure.protocol != 1
        || failure.session != nonce
        || failure.fixture
        || !code_allowed
        || !exception_allowed
        || !endpoint_allowed
    {
        return Err("lazer 失敗紀錄不符合正式工作階段契約；現有檔案已保留".into());
    }
    Ok(())
}

fn cleanup_failure(paths: &Paths, nonce: &str) -> Result<()> {
    let session = matching_session(paths, nonce)?;
    if let Some(failure) = read_failure(paths, nonce, session.endpoint(), session.version < 3)? {
        merge_failure(session.failure, Some(failure))?;
        let path = paths.failure(nonce);
        ordinary_file(&path)?;
        fs::remove_file(path).map_err(|_| "無法移除工作階段失敗訊號")?;
    }
    Ok(())
}
fn cleanup_ready(paths: &Paths, nonce: &str, endpoint: &Endpoint) -> Result<()> {
    if let Some(ready) = read_ready(paths, nonce)? {
        validate_ready(&ready, nonce, endpoint)?;
        let path = paths.ready(nonce);
        ordinary_file(&path)?;
        fs::remove_file(path).map_err(|_| "無法移除工作階段啟動訊號")?;
    }
    Ok(())
}

fn cleanup_session_ready(paths: &Paths, session: &Session) -> Result<()> {
    if !session.legacy_ready {
        return cleanup_ready(paths, &session.nonce, session.endpoint());
    }
    let path = paths
        .legacy_data
        .join(format!("session-ready-{}.json", session.nonce));
    if let Some(ready) = read_json::<Ready>(&path)? {
        validate_ready(&ready, &session.nonce, session.endpoint())?;
        ordinary_file(&path)?;
        fs::remove_file(path).map_err(|_| "無法移除舊版工作階段啟動訊號")?;
    }
    Ok(())
}

fn verify_bridge(paths: &Paths) -> Result<(PathBuf, String)> {
    let hash = option_env!("ILLYA_BRIDGE_SHA256")
        .ok_or("此啟動器缺少正式連線模組發行雜湊，暫不支援 lazer")?;
    verify_bridge_with_hash(paths, hash)
}
fn verify_bridge_with_hash(paths: &Paths, hash: &str) -> Result<(PathBuf, String)> {
    let artifact = paths.bridge()?;
    verify_bridge_artifact(&artifact, hash)?;
    Ok((artifact, hash.to_owned()))
}
fn verify_bridge_artifact(artifact: &Path, hash: &str) -> Result<()> {
    if !valid_nonce(hash) {
        return Err("正式連線模組雜湊格式無效".into());
    }
    if artifact_sha256(artifact)? != hash {
        return Err("連線模組缺失或與正式發行雜湊不符".into());
    }
    Ok(())
}

fn verify_lazer_compatibility(paths: &Paths, selected: &Path) -> Result<()> {
    verify_bridge(paths)?;
    let directory = selected.parent().ok_or("lazer 缺少安裝資料夾")?;
    for (filename, expected) in [
        ("osu.Game.dll", SUPPORTED_GAME_SHA256),
        ("osu.Framework.dll", SUPPORTED_FRAMEWORK_SHA256),
        ("osu!.dll", SUPPORTED_ENTRY_SHA256),
    ] {
        if artifact_sha256(directory.join(filename))? != expected {
            return Err("這個 lazer 版本尚未通過相容性驗證；不會啟動或切回官方伺服器".into());
        }
    }
    Ok(())
}

fn validate_client(path: &Path, client: Client) -> Result<PathBuf> {
    let path = canonical_file(path)?;
    if !path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("osu!.exe"))
    {
        return Err("請選擇 osu!.exe，而不是其他執行檔或捷徑".into());
    }
    validate_pe(&path, client)?;
    if client == Client::Lazer {
        let directory = path.parent().ok_or("lazer 缺少安裝資料夾")?;
        for filename in [
            "osu!.dll",
            "osu!.deps.json",
            "osu!.runtimeconfig.json",
            "osu.Game.dll",
            "osu.Framework.dll",
            "hostfxr.dll",
        ] {
            canonical_file(&directory.join(filename))
                .map_err(|_| format!("lazer 安裝資料夾缺少安全可讀取的 {filename}"))?;
        }
        validate_pe(&directory.join("hostfxr.dll"), Client::Lazer)?;
    }
    Ok(path)
}

fn validate_pe(path: &Path, client: Client) -> Result<()> {
    let mut file = File::open(path).map_err(|_| "無法讀取遊戲 PE 標頭")?;
    let mut dos = [0u8; 64];
    file.read_exact(&mut dos)
        .map_err(|_| "執行檔缺少 DOS/PE 標頭")?;
    if &dos[..2] != b"MZ" {
        return Err("選擇的檔案不是 Windows PE 執行檔".into());
    }
    let offset = u32::from_le_bytes(dos[60..64].try_into().expect("four-byte PE offset"));
    if !(64..=1024 * 1024).contains(&offset) {
        return Err("Windows PE 標頭位置無效".into());
    }
    file.seek(SeekFrom::Start(u64::from(offset)))
        .map_err(|_| "無法讀取 PE 標頭")?;
    let mut pe = [0u8; 6];
    file.read_exact(&mut pe)
        .map_err(|_| "Windows PE 標頭已截斷")?;
    let machine = u16::from_le_bytes([pe[4], pe[5]]);
    if &pe[..4] != b"PE\0\0"
        || !matches!(machine, 0x14c | 0x8664)
        || client == Client::Lazer && machine != 0x8664
    {
        return Err("目前啟動器需要 x86 stable 或 x64 lazer 執行檔".into());
    }
    Ok(())
}

fn fresh_nonce() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "無法建立安全的工作階段識別碼")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn valid_nonce(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn display_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
}

fn hidden_child(command: &mut Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW; independent process lifetime.
    }
}

fn sanitize_injection_environment(command: &mut Command) {
    // Remove case-insensitive inherited CLR/profiler hooks. The mandatory host
    // sets its own STARTUP_HOOKS runtime property before running the application.
    for (name, _) in std::env::vars_os() {
        if is_injection_environment_name(&name.to_string_lossy()) {
            command.env_remove(name);
        }
    }
    command.env("DOTNET_ENABLEDIAGNOSTICS", "0");
}
fn is_injection_environment_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.starts_with("COMPLUS_")
        || upper.starts_with("CORECLR_")
        || upper.starts_with("COR_")
        || upper.starts_with("DOTNET_ROOT")
        || matches!(
            upper.as_str(),
            "DOTNET_STARTUP_HOOKS"
                | "DOTNET_ADDITIONAL_DEPS"
                | "DOTNET_SHARED_STORE"
                | "DOTNET_DIAGNOSTICPORTS"
                | "DOTNET_MODIFIABLE_ASSEMBLIES"
                | "DOTNET_ENABLEDIAGNOSTICS"
                | "DOTNET_MULTILEVEL_LOOKUP"
                | "DOTNET_ROLL_FORWARD"
                | "DOTNET_ROLL_FORWARD_TO_PRERELEASE"
        )
}

struct ControlLock(File);
impl ControlLock {
    fn open(paths: &Paths) -> Result<Self> {
        let file = lock_file(&paths.control.join(".control.lock"))?;
        FileExt::lock_exclusive(&file).map_err(|_| "無法取得啟動器紀錄鎖")?;
        Ok(Self(file))
    }
}
impl Drop for ControlLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
fn exclusive_file(path: &Path) -> Result<File> {
    let file = lock_file(path)?;
    FileExt::try_lock_exclusive(&file).map_err(|_| "另一個啟動器正在更新工作階段，請稍後重試")?;
    Ok(file)
}
fn lock_file(path: &Path) -> Result<File> {
    if path_exists(path)? {
        ordinary_file(path)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| "無法開啟啟動器鎖定檔")?;
    check_metadata(&file.metadata().map_err(|error| error.to_string())?)?;
    // Control records are held only for brief atomic transitions. Guardian lock
    // uses the same API but is intentionally held for the whole session.
    Ok(file)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    read_json_limit(path, MAX_RECORD_BYTES)
}

fn read_json_limit<T: DeserializeOwned>(path: &Path, max_bytes: u64) -> Result<Option<T>> {
    if !path_exists(path)? {
        return Ok(None);
    }
    ordinary_file(path)?;
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| "無法開啟啟動器紀錄")?
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "無法讀取啟動器紀錄")?;
    if bytes.len() as u64 > max_bytes {
        return Err("啟動器紀錄超過大小限制；現有檔案已保留".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "啟動器紀錄格式無效；現有檔案已保留".into())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let directory = path.parent().ok_or("紀錄缺少父資料夾")?;
    check_components(directory)?;
    if path_exists(path)? {
        ordinary_file(path)?;
    }
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err("啟動器紀錄超過大小限制".into());
    }
    let temporary = directory.join(format!(
        ".illyaverse-record-{}.tmp",
        Uuid::new_v4().hyphenated()
    ));
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|_| "無法建立啟動器紀錄暫存檔")?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| "無法同步啟動器紀錄")?;
    drop(file);
    replace_record(&temporary, path).map_err(|_| "無法原子更新啟動器紀錄")?;
    #[cfg(unix)]
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|_| "無法同步紀錄資料夾")?;
    Ok(())
}

#[cfg(not(windows))]
fn replace_record(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}
#[cfg(windows)]
fn replace_record(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<_> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0x1 | 0x8) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn path_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("無法檢查啟動器路徑".into()),
    }
}
fn check_metadata(metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() {
        return Err("啟動器不接受 symlink 路徑".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("啟動器不接受 Windows reparse point 路徑".into());
        }
    }
    Ok(())
}
fn check_components(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("啟動器路徑必須是沒有 traversal 的絕對路徑".into());
    }
    for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        check_metadata(&fs::symlink_metadata(ancestor).map_err(|_| "無法檢查啟動器路徑組件")?)?;
    }
    Ok(())
}
fn ordinary_file(path: &Path) -> Result<()> {
    check_components(path)?;
    if !fs::symlink_metadata(path)
        .map_err(|_| "無法檢查檔案")?
        .is_file()
    {
        return Err("啟動器需要一般檔案".into());
    }
    Ok(())
}
fn canonical_file(path: &Path) -> Result<PathBuf> {
    ordinary_file(path)?;
    fs::canonicalize(path).map_err(|_| "無法解析一般檔案路徑".into())
}
fn ensure_directory(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("資料資料夾路徑必須是絕對路徑".into());
    }
    for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        if !path_exists(ancestor)? {
            fs::create_dir(ancestor).map_err(|_| "無法建立 Illyaverse 私有資料夾")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(ancestor, fs::Permissions::from_mode(0o700))
                    .map_err(|error| error.to_string())?;
            }
        }
        let metadata = fs::symlink_metadata(ancestor).map_err(|_| "無法檢查資料夾")?;
        check_metadata(&metadata)?;
        if !metadata.is_dir() {
            return Err("Illyaverse 資料路徑不是資料夾".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fixture {
        directory: PathBuf,
        paths: Paths,
        client: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "kaleidoscope-runtime-test-{}",
                Uuid::new_v4().hyphenated()
            ));
            fs::create_dir(&directory).unwrap();
            let directory = fs::canonicalize(directory).unwrap();
            let control = directory.join("control");
            fs::create_dir(&control).unwrap();
            let package = directory.join("package");
            fs::create_dir(&package).unwrap();
            let launcher = package.join("kaleidoscope.fixture.exe");
            fs::write(&launcher, b"launcher identity fixture").unwrap();
            let client_root = directory.join("game");
            fs::create_dir(&client_root).unwrap();
            let client = client_root.join("osu!.exe");
            write_pe(&client, 0x8664);
            let paths = Paths {
                control,
                local: directory.join("local"),
                roaming: directory.join("roaming"),
                legacy_data: directory.join("private-lazer"),
                launcher,
            };
            fs::create_dir(&paths.roaming).unwrap();
            fs::create_dir(paths.roaming.join("osu")).unwrap();
            fs::create_dir(&paths.local).unwrap();
            Self {
                directory,
                paths,
                client,
            }
        }
        fn bridge(&self) -> (PathBuf, String) {
            let bridge = self.paths.bridge().unwrap();
            fs::write(&bridge, b"owned startup bridge fixture").unwrap();
            let hash = artifact_sha256(&bridge).unwrap();
            (bridge, hash)
        }
        fn upgraded_paths(&self) -> Paths {
            let mut paths = self.paths.clone();
            let package = self.directory.join("upgraded-package");
            fs::create_dir(&package).unwrap();
            paths.launcher = package.join("kaleidoscope.fixture.exe");
            fs::write(&paths.launcher, b"upgraded launcher identity fixture").unwrap();
            paths
        }
        fn layout(&self) {
            let root = self.client.parent().unwrap();
            for filename in ["osu!.dll", "osu.Game.dll", "osu.Framework.dll"] {
                fs::write(root.join(filename), b"managed assembly fixture").unwrap();
            }
            for filename in ["osu!.deps.json", "osu!.runtimeconfig.json"] {
                fs::write(root.join(filename), b"{}").unwrap();
            }
            write_pe(&root.join("hostfxr.dll"), 0x8664);
        }
        #[cfg(windows)]
        fn live_paths(&self) -> Paths {
            let mut paths = self.paths.clone();
            paths.launcher = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
            paths
        }
        fn session(&self, paths: &Paths) -> Session {
            Session {
                version: 3,
                nonce: fresh_nonce().unwrap(),
                client: Client::Lazer,
                endpoint: Some(Endpoint::default()),
                selected_executable: self.client.clone(),
                selected_identity: "fixture-selected-id".into(),
                launcher_executable: paths.launcher.clone(),
                launcher_identity: "fixture-launcher-id".into(),
                guardian: None,
                child: None,
                install_session: None,
                library: Some(resolve_library(paths, &self.client).unwrap()),
                legacy_ready: false,
                phase: Phase::Preparing,
                detail: "fixture preparing".into(),
                stop_requested: false,
                failure: None,
                host_status: None,
                child_exit: None,
                last_progress: None,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let directory = fs::canonicalize(&self.directory).unwrap();
            let temporary = fs::canonicalize(std::env::temp_dir()).unwrap();
            assert_eq!(directory.parent(), Some(temporary.as_path()));
            assert!(directory
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("kaleidoscope-runtime-test-"));
            fs::remove_dir_all(directory).unwrap();
        }
    }
    fn write_pe(path: &Path, machine: u16) {
        let mut bytes = [0u8; 128];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[68..70].copy_from_slice(&machine.to_le_bytes());
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn snapshot_contract_has_exact_public_fields_and_active_endpoint() {
        let fixture = Fixture::new();
        let snapshot = snapshot_at(&fixture.paths).unwrap();
        assert_eq!(
            serde_json::to_value(snapshot).unwrap(),
            json!({
                "stable":{"path":null,"running":false},
                "lazer":{"path":null,"running":false,"compatibility":"unknown"},
                "phase":"idle","detail":"未找到 stable 預設安裝位置，請選擇遊戲安裝路徑。","pluginPresent":false,"activeEndpoint":null
            })
        );
        assert_eq!(
            serde_json::to_value(Phase::CleanupFailed).unwrap(),
            "cleanup_failed"
        );
        assert_eq!(serde_json::to_value(Client::Lazer).unwrap(), "lazer");
        assert!(
            handle_internal(&["__host-lazer".into(), "../../outside".into()])
                .unwrap()
                .is_err()
        );
        assert!(handle_internal(&["help".into()]).is_none());
    }

    #[test]
    fn stable_discovery_populates_snapshot_without_creating_preferences() {
        let fixture = Fixture::new();
        let installed = fixture.paths.local.join("osu!");
        fs::create_dir(&installed).unwrap();
        let executable = installed.join("osu!.exe");
        write_pe(&executable, 0x14c);
        let unrelated = installed.join("osu!.fixture.cfg");
        fs::write(&unrelated, b"unrelated game preferences fixture").unwrap();

        let snapshot = snapshot_at(&fixture.paths).unwrap();
        assert_eq!(
            snapshot.stable.path,
            Some(display_path(&fs::canonicalize(&executable).unwrap()))
        );
        assert!(!snapshot.stable.running);
        assert_eq!(snapshot.detail, "遊戲位置已確認，可以啟動。");
        assert!(snapshot.lazer.path.is_none());
        assert!(!fixture.paths.prefs().exists());
        assert!(!fixture.paths.session().exists());
        assert_eq!(
            fs::read(unrelated).unwrap(),
            b"unrelated game preferences fixture"
        );
    }

    #[test]
    fn stable_discovery_preserves_explicit_selection_even_when_it_is_missing() {
        let fixture = Fixture::new();
        let installed = fixture.paths.local.join("osu!");
        fs::create_dir(&installed).unwrap();
        write_pe(&installed.join("osu!.exe"), 0x14c);
        let explicit = fixture.directory.join("moved-install").join("osu!.exe");
        let preferences = Preferences {
            stable: Some(explicit.clone()),
            ..Preferences::default()
        };
        write_json(&fixture.paths.prefs(), &preferences).unwrap();
        let before = fs::read(fixture.paths.prefs()).unwrap();

        assert_eq!(
            selected_client_path(&fixture.paths, &preferences, Client::Stable),
            Some(explicit.clone())
        );
        assert_eq!(
            snapshot_at(&fixture.paths).unwrap().stable.path,
            Some(display_path(&explicit))
        );
        assert!(start_session(&fixture.paths, Client::Stable).is_err());
        assert_eq!(fs::read(fixture.paths.prefs()).unwrap(), before);
        assert!(!fixture.paths.session().exists());
    }

    #[test]
    fn stable_discovery_ignores_absent_directory_and_invalid_executables() {
        let fixture = Fixture::new();
        let preferences = Preferences::default();
        let discover = || selected_client_path(&fixture.paths, &preferences, Client::Stable);
        assert!(discover().is_none());
        let installed = fixture.paths.local.join("osu!");
        fs::create_dir(&installed).unwrap();
        let executable = installed.join("osu!.exe");
        fs::write(&executable, b"not a Windows executable").unwrap();
        assert!(discover().is_none());
        write_pe(&executable, 0xaa64);
        assert!(discover().is_none());
        fs::remove_file(&executable).unwrap();
        fs::create_dir(&executable).unwrap();
        assert!(discover().is_none());
        assert!(snapshot_at(&fixture.paths).unwrap().stable.path.is_none());
        assert!(!fixture.paths.prefs().exists());
    }

    #[test]
    fn endpoint_setting_changes_only_next_launch_and_invalid_input_preserves_records() {
        let fixture = Fixture::new();
        assert_eq!(settings_at(&fixture.paths).unwrap().endpoint, SERVER);
        let first = save_endpoint_at(&fixture.paths, "HTTPS://First.Example:443/").unwrap();
        assert_eq!(first.endpoint, "https://first.example");
        let mut session = fixture.session(&fixture.paths);
        session.endpoint = Some(read_preferences(&fixture.paths).unwrap().endpoint);
        write_json(&fixture.paths.session(), &session).unwrap();
        let before_session = fs::read(fixture.paths.session()).unwrap();
        let next = save_endpoint_at(&fixture.paths, "https://next.example:8443").unwrap();
        assert_eq!(next.endpoint, "https://next.example:8443");
        assert_eq!(fs::read(fixture.paths.session()).unwrap(), before_session);
        assert_eq!(
            snapshot_at(&fixture.paths)
                .unwrap()
                .active_endpoint
                .as_deref(),
            Some("https://first.example")
        );
        let before_preferences = fs::read(fixture.paths.prefs()).unwrap();
        let error =
            save_endpoint_at(&fixture.paths, "https://name:fixture-password@ppy.sh").unwrap_err();
        assert!(!error.contains("fixture-password"));
        assert_eq!(fs::read(fixture.paths.prefs()).unwrap(), before_preferences);
        assert_eq!(fs::read(fixture.paths.session()).unwrap(), before_session);
    }

    #[test]
    fn historical_sessions_default_only_to_original_endpoint_and_v3_requires_one() {
        let fixture = Fixture::new();
        write_json(&fixture.paths.prefs(), &json!({"stable":null,"lazer":null})).unwrap();
        assert_eq!(
            read_preferences(&fixture.paths).unwrap().endpoint,
            Endpoint::default()
        );
        let mut value = serde_json::to_value(fixture.session(&fixture.paths)).unwrap();
        value.as_object_mut().unwrap().remove("endpoint");
        write_json(&fixture.paths.session(), &value).unwrap();
        assert!(read_session(&fixture.paths).is_err());
        value["version"] = json!(2);
        write_json(&fixture.paths.session(), &value).unwrap();
        let historical = read_session(&fixture.paths).unwrap().unwrap();
        assert_eq!(historical.endpoint(), &Endpoint::default());
        value["endpoint"] = json!("https://another.example");
        write_json(&fixture.paths.session(), &value).unwrap();
        assert!(read_session(&fixture.paths).is_err());
        value["version"] = json!(3);
        write_json(&fixture.paths.session(), &value).unwrap();
        assert_eq!(
            read_session(&fixture.paths)
                .unwrap()
                .unwrap()
                .endpoint()
                .as_str(),
            "https://another.example"
        );
    }

    #[test]
    fn stable_custom_port_refuses_before_any_session_or_process_action() {
        let fixture = Fixture::new();
        let preferences = Preferences {
            stable: Some(fixture.client.clone()),
            lazer: None,
            endpoint: Endpoint::parse("https://selected.example:8443").unwrap(),
        };
        write_json(&fixture.paths.prefs(), &preferences).unwrap();
        let before = fs::read(fixture.paths.prefs()).unwrap();
        assert!(start_session(&fixture.paths, Client::Stable)
            .unwrap_err()
            .contains("HTTPS port 443"));
        assert!(!fixture.paths.session().exists());
        assert_eq!(fs::read(fixture.paths.prefs()).unwrap(), before);
    }

    #[test]
    fn ready_and_first_draw_are_bound_to_captured_endpoint_including_port() {
        let fixture = Fixture::new();
        let nonce = fresh_nonce().unwrap();
        let endpoint = Endpoint::parse("https://selected.example:8443").unwrap();
        let ready = json!({"protocol":1,"session":nonce,"status":"ready","endpoint":endpoint.as_str(),"fixture":false});
        write_json(&fixture.paths.ready(&nonce), &ready).unwrap();
        let signal = read_ready(&fixture.paths, &nonce).unwrap().unwrap();
        validate_ready(&signal, &nonce, &endpoint).unwrap();
        assert!(validate_ready(&signal, &nonce, &Endpoint::default()).is_err());
        assert!(cleanup_ready(&fixture.paths, &nonce, &Endpoint::default()).is_err());
        assert!(fixture.paths.ready(&nonce).exists());
        let stage = ProgressStage::FirstSuccessfulDraw;
        let mut progress = progress_signal(&nonce, stage);
        progress.endpoint = endpoint.as_str().to_owned();
        write_json(&fixture.paths.progress(&nonce, stage), &progress).unwrap();
        assert_eq!(
            read_progress(&fixture.paths, &nonce, &endpoint).unwrap(),
            Some(stage)
        );
        for wrong in [
            Endpoint::default(),
            Endpoint::parse("https://selected.example").unwrap(),
        ] {
            assert!(read_progress(&fixture.paths, &nonce, &wrong).is_err());
            assert!(cleanup_progress(&fixture.paths, &nonce, &wrong).is_err());
        }
        assert!(fixture.paths.progress(&nonce, stage).exists());
        cleanup_ready(&fixture.paths, &nonce, &endpoint).unwrap();
        cleanup_progress(&fixture.paths, &nonce, &endpoint).unwrap();
    }

    #[test]
    fn failure_endpoint_requires_match_or_only_typed_unavailable_bootstrap() {
        let nonce = fresh_nonce().unwrap();
        let endpoint = Endpoint::parse("https://selected.example:8443").unwrap();
        let mut failure = managed_failure(&nonce);
        failure.endpoint = FailureEndpoint::Selected(endpoint.as_str().to_owned());
        validate_failure(&failure, &nonce, &endpoint, false).unwrap();
        assert!(validate_failure(&failure, &nonce, &Endpoint::default(), false).is_err());
        failure.endpoint = FailureEndpoint::Unavailable;
        assert!(validate_failure(&failure, &nonce, &endpoint, false).is_err());
        failure.stage = "bootstrap".into();
        for code in ["server_endpoint_missing", "server_endpoint_invalid"] {
            failure.code = code.into();
            validate_failure(&failure, &nonce, &endpoint, false).unwrap();
        }
        let mut value = serde_json::to_value(&failure).unwrap();
        assert!(value["endpoint"].is_null());
        value.as_object_mut().unwrap().remove("endpoint");
        let missing: Failure = serde_json::from_value(value).unwrap();
        assert!(validate_failure(&missing, &nonce, &endpoint, false).is_err());
        let mut old = serde_json::to_value(managed_failure(&nonce)).unwrap();
        old.as_object_mut().unwrap().remove("endpoint");
        let old: Failure = serde_json::from_value(old).unwrap();
        validate_failure(&old, &nonce, &Endpoint::default(), true).unwrap();
        assert!(validate_failure(&old, &nonce, &Endpoint::default(), false).is_err());
        assert!(validate_failure(&old, &nonce, &endpoint, true).is_err());
    }

    #[test]
    fn cli_wait_observes_terminal_outcomes_and_has_a_bounded_timeout() {
        let fixture = Fixture::new();
        for terminal in [
            Phase::Running,
            Phase::Error,
            Phase::CleanupFailed,
            Phase::Idle,
        ] {
            let mut session = fixture.session(&fixture.paths);
            let mut reads = 0;
            wait_for_start(
                || {
                    reads += 1;
                    if reads > 1 {
                        session.phase = terminal;
                    }
                    Ok(session.clone())
                },
                Duration::from_secs(1),
            )
            .unwrap();
            assert_eq!(reads, 2);
        }
        let session = fixture.session(&fixture.paths);
        write_json(&fixture.paths.session(), &session).unwrap();
        let before = fs::read(fixture.paths.session()).unwrap();
        let error = wait_for_start(
            || matching_session(&fixture.paths, &session.nonce),
            Duration::ZERO,
        )
        .unwrap_err();
        assert!(error.contains("逾時"));
        assert_eq!(fs::read(fixture.paths.session()).unwrap(), before);
    }

    #[test]
    fn cli_waits_through_transient_error_until_cleanup_is_finalized() {
        let fixture = Fixture::new();
        let mut session = fixture.session(&fixture.paths);
        session.install_session = Some(Uuid::new_v4());
        session.guardian = Some(process::Identity {
            pid: 42,
            creation_time: 1,
            executable: session.launcher_executable.clone(),
            executable_identity: session.launcher_identity.clone(),
        });
        session.child = session.guardian.clone();
        let mut reads = 0;
        wait_for_start(
            || {
                reads += 1;
                session.phase = if reads == 2 {
                    Phase::Cleaning
                } else {
                    Phase::Error
                };
                if reads == 3 {
                    session.guardian = None;
                    session.child = None;
                    session.install_session = None;
                }
                Ok(session.clone())
            },
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(reads, 3);
    }

    fn managed_failure(nonce: &str) -> Failure {
        Failure {
            protocol: 1,
            session: nonce.into(),
            stage: "assemblies".into(),
            code: "client_assembly_location_mismatch".into(),
            exception: "TransfigurationStartupException".into(),
            endpoint: FailureEndpoint::Selected(SERVER.into()),
            fixture: false,
        }
    }

    fn progress_signal(nonce: &str, stage: ProgressStage) -> Progress {
        Progress {
            protocol: 1,
            session: nonce.into(),
            stage,
            endpoint: SERVER.into(),
            fixture: false,
        }
    }

    #[cfg(windows)]
    #[test]
    fn lazer_guard_and_intermediate_progress_wait_for_verified_first_draw() {
        let fixture = Fixture::new();
        let paths = fixture.live_paths();
        let own = process::current_identity().unwrap();
        let mut session = fixture.session(&paths);
        session.launcher_identity = own.executable_identity.clone();
        session.child = Some(own.clone());
        let live = Some(process::state(&own).unwrap());
        assert!(!apply_lazer_progress(&mut session, true, None, live).unwrap());
        assert_eq!(session.phase, Phase::Preparing);
        assert!(session.detail.contains("正在等待首個遊戲畫面"));
        for stage in ProgressStage::ALL
            .into_iter()
            .filter(|stage| *stage != ProgressStage::FirstSuccessfulDraw)
        {
            assert!(!apply_lazer_progress(&mut session, true, Some(stage), live).unwrap());
            assert_eq!(session.phase, Phase::Preparing);
            assert_eq!(session.last_progress, Some(stage));
        }
        let mut unguarded = session.clone();
        assert!(!apply_lazer_progress(
            &mut unguarded,
            false,
            Some(ProgressStage::FirstSuccessfulDraw),
            live
        )
        .unwrap());
        assert_eq!(unguarded.phase, Phase::Preparing);
        for state in [
            None,
            Some(process::State::Gone),
            Some(process::State::Ambiguous),
        ] {
            let mut closed = session.clone();
            assert!(apply_lazer_progress(
                &mut closed,
                true,
                Some(ProgressStage::FirstSuccessfulDraw),
                state
            )
            .is_err());
            assert_eq!(closed.phase, Phase::Preparing);
        }
        assert!(apply_lazer_progress(
            &mut session,
            true,
            Some(ProgressStage::FirstSuccessfulDraw),
            live
        )
        .unwrap());
        assert_eq!(session.phase, Phase::Running);
        assert!(session
            .detail
            .contains("last_progress=first_successful_draw"));
        write_json(&paths.session(), &session).unwrap();
        assert_eq!(
            matching_session(&paths, &session.nonce).unwrap().phase,
            Phase::Running
        );
    }

    #[test]
    fn progress_reader_requires_exact_schema_nonce_stage_and_fixed_endpoint() {
        let fixture = Fixture::new();
        let nonce = fresh_nonce().unwrap();
        let stage = ProgressStage::RendererInitializing;
        let path = fixture.paths.progress(&nonce, stage);
        let normal = serde_json::to_value(progress_signal(&nonce, stage)).unwrap();
        write_json(&path, &normal).unwrap();
        assert_eq!(
            read_progress(&fixture.paths, &nonce, &Endpoint::default()).unwrap(),
            Some(stage)
        );
        for (field, value) in [
            ("protocol", json!(2)),
            ("session", json!("0".repeat(64))),
            ("stage", json!("first_successful_draw")),
            ("stage", json!("private_path=fixture-secret")),
            ("endpoint", json!("https://osu.ppy.sh")),
            ("fixture", json!(true)),
            (
                "message",
                json!("password=fixture-secret&token=fixture-token"),
            ),
        ] {
            let mut wrong = normal.clone();
            wrong[field] = value;
            write_json(&path, &wrong).unwrap();
            let error = read_progress(&fixture.paths, &nonce, &Endpoint::default()).unwrap_err();
            assert!(!error.contains("fixture-secret"));
            assert!(!error.contains("fixture-token"));
            assert!(cleanup_progress(&fixture.paths, &nonce, &Endpoint::default()).is_err());
            assert!(path.exists());
        }
        let duplicate = format!(
            "{{\"protocol\":1,{}",
            serde_json::to_string(&normal)
                .unwrap()
                .trim_start_matches('{')
        );
        fs::write(&path, duplicate).unwrap();
        assert!(read_progress(&fixture.paths, &nonce, &Endpoint::default()).is_err());
        fs::write(&path, vec![b' '; MAX_PROGRESS_BYTES as usize + 1]).unwrap();
        assert!(read_progress(&fixture.paths, &nonce, &Endpoint::default()).is_err());
        assert!(read_progress(&fixture.paths, "../../foreign", &Endpoint::default()).is_err());
        assert!(cleanup_progress(&fixture.paths, "../../foreign", &Endpoint::default()).is_err());
    }

    #[test]
    fn native_crash_keeps_last_verified_progress_after_owned_cleanup() {
        let fixture = Fixture::new();
        let mut session = fixture.session(&fixture.paths);
        let library = session.library.as_ref().unwrap();
        ensure_directory(&library.rulesets()).unwrap();
        let (bridge, hash) = fixture.bridge();
        let mut managed = ManagedRuleset::open(library.rulesets()).unwrap();
        let receipt = managed.install(bridge, &hash).unwrap();
        session.install_session = Some(receipt.session_id);
        write_json(&fixture.paths.session(), &session).unwrap();
        for stage in [
            ProgressStage::OriginalMainEntered,
            ProgressStage::RendererInitializing,
        ] {
            write_json(
                &fixture.paths.progress(&session.nonce, stage),
                &progress_signal(&session.nonce, stage),
            )
            .unwrap();
        }
        let ready = json!({"protocol":1,"session":session.nonce,"status":"ready","endpoint":SERVER,"fixture":false});
        write_json(&fixture.paths.ready(&session.nonce), &ready).unwrap();
        let mut result =
            finish_child_exit(&fixture.paths, &session, false, false, Some(-1073741819));
        assert!(result
            .as_ref()
            .unwrap_err()
            .contains("last_progress=renderer_initializing"));
        assert!(result
            .as_ref()
            .unwrap_err()
            .contains("child_exit=-1073741819"));
        capture_closed_child(
            &fixture.paths,
            &session.nonce,
            Client::Lazer,
            Some(-1073741819),
            &mut result,
        )
        .unwrap();
        managed.cleanup().unwrap();
        cleanup_ready(&fixture.paths, &session.nonce, &Endpoint::default()).unwrap();
        cleanup_progress(&fixture.paths, &session.nonce, &Endpoint::default()).unwrap();
        update_session(&fixture.paths, &session.nonce, |record| {
            record.install_session = None
        })
        .unwrap();
        let record = matching_session(&fixture.paths, &session.nonce).unwrap();
        assert_eq!(record.phase, Phase::Error);
        assert_eq!(
            record.last_progress,
            Some(ProgressStage::RendererInitializing)
        );
        assert_eq!(record.child_exit, Some(-1073741819));
        assert!(record
            .detail
            .contains("last_progress=renderer_initializing"));
        assert!(!receipt.installed_path.exists());
        assert_eq!(
            read_progress(&fixture.paths, &session.nonce, &Endpoint::default()).unwrap(),
            None
        );
    }

    #[test]
    fn guard_without_draw_cli_timeout_leaves_process_record_and_artifact_untouched() {
        let fixture = Fixture::new();
        let mut session = fixture.session(&fixture.paths);
        let library = session.library.as_ref().unwrap();
        ensure_directory(&library.rulesets()).unwrap();
        let (bridge, hash) = fixture.bridge();
        let mut managed = ManagedRuleset::open(library.rulesets()).unwrap();
        let receipt = managed.install(bridge, &hash).unwrap();
        session.install_session = Some(receipt.session_id);
        session.child = Some(process::Identity {
            pid: 42,
            creation_time: 1,
            executable: session.launcher_executable.clone(),
            executable_identity: session.launcher_identity.clone(),
        });
        assert!(!apply_lazer_progress(
            &mut session,
            true,
            Some(ProgressStage::GameHostCreated),
            Some(process::State::Live)
        )
        .unwrap());
        write_json(&fixture.paths.session(), &session).unwrap();
        let before = fs::read(fixture.paths.session()).unwrap();
        assert!(wait_for_start(
            || matching_session(&fixture.paths, &session.nonce),
            Duration::ZERO
        )
        .unwrap_err()
        .contains("逾時"));
        assert_eq!(fs::read(fixture.paths.session()).unwrap(), before);
        assert_eq!(
            fs::read(&receipt.installed_path).unwrap(),
            b"owned startup bridge fixture"
        );
        assert!(
            !matching_session(&fixture.paths, &session.nonce)
                .unwrap()
                .stop_requested
        );
    }

    #[test]
    fn post_exit_progress_is_captured_without_promoting_running() {
        let fixture = Fixture::new();
        let session = fixture.session(&fixture.paths);
        write_json(&fixture.paths.session(), &session).unwrap();
        write_json(
            &fixture
                .paths
                .progress(&session.nonce, ProgressStage::OriginalMainEntered),
            &progress_signal(&session.nonce, ProgressStage::OriginalMainEntered),
        )
        .unwrap();
        let mut result = finish_child_exit(&fixture.paths, &session, false, true, Some(0));
        write_json(
            &fixture
                .paths
                .progress(&session.nonce, ProgressStage::FirstSuccessfulDraw),
            &progress_signal(&session.nonce, ProgressStage::FirstSuccessfulDraw),
        )
        .unwrap();
        capture_closed_child(
            &fixture.paths,
            &session.nonce,
            Client::Lazer,
            Some(0),
            &mut result,
        )
        .unwrap();
        cleanup_progress(&fixture.paths, &session.nonce, &Endpoint::default()).unwrap();
        let record = matching_session(&fixture.paths, &session.nonce).unwrap();
        assert_eq!(record.phase, Phase::Error);
        assert_eq!(
            record.last_progress,
            Some(ProgressStage::FirstSuccessfulDraw)
        );
        assert!(result
            .unwrap_err()
            .contains("last_progress=first_successful_draw"));
        assert_eq!(
            read_progress(&fixture.paths, &session.nonce, &Endpoint::default()).unwrap(),
            None
        );
    }

    #[test]
    fn post_draw_normal_exit_retains_progress_without_an_error() {
        let fixture = Fixture::new();
        let mut session = fixture.session(&fixture.paths);
        session.phase = Phase::Running;
        session.last_progress = Some(ProgressStage::FirstSuccessfulDraw);
        write_json(&fixture.paths.session(), &session).unwrap();
        let sidecar = fixture
            .paths
            .progress(&session.nonce, ProgressStage::FirstSuccessfulDraw);
        write_json(
            &sidecar,
            &progress_signal(&session.nonce, ProgressStage::FirstSuccessfulDraw),
        )
        .unwrap();
        let mut result = finish_child_exit(&fixture.paths, &session, true, true, Some(0));
        capture_closed_child(
            &fixture.paths,
            &session.nonce,
            Client::Lazer,
            Some(0),
            &mut result,
        )
        .unwrap();
        cleanup_progress(&fixture.paths, &session.nonce, &Endpoint::default()).unwrap();
        assert!(result.is_ok());
        let record = matching_session(&fixture.paths, &session.nonce).unwrap();
        assert_eq!(
            record.last_progress,
            Some(ProgressStage::FirstSuccessfulDraw)
        );
        assert_eq!(record.child_exit, Some(0));
        assert!(!sidecar.exists());
    }

    #[test]
    fn progress_staging_cleanup_validates_all_before_deleting_and_preserves_other_sessions() {
        let fixture = Fixture::new();
        let nonce = fresh_nonce().unwrap();
        let stage = ProgressStage::OriginalMainEntered;
        let final_path = fixture.paths.progress(&nonce, stage);
        write_json(&final_path, &progress_signal(&nonce, stage)).unwrap();
        let temporary = fixture.paths.control.join(format!(
            "session-progress-{nonce}-{}-{}.tmp",
            stage.name(),
            "a".repeat(32)
        ));
        fs::write(&temporary, b"{partial owned fixture").unwrap();
        let other_nonce = fresh_nonce().unwrap();
        let foreign = fixture.paths.control.join(format!(
            "session-progress-{other_nonce}-{}-{}.tmp",
            stage.name(),
            "b".repeat(32)
        ));
        fs::write(&foreign, b"unrelated staging fixture").unwrap();
        assert_eq!(
            read_progress(&fixture.paths, &nonce, &Endpoint::default()).unwrap(),
            Some(stage)
        );
        assert!(cleanup_progress(&fixture.paths, &nonce, &Endpoint::default()).is_err());
        assert!(final_path.exists());
        assert_eq!(fs::read(&temporary).unwrap(), b"{partial owned fixture");
        write_json(&temporary, &progress_signal(&nonce, stage)).unwrap();
        cleanup_progress(&fixture.paths, &nonce, &Endpoint::default()).unwrap();
        assert!(!temporary.exists());
        assert!(!final_path.exists());
        assert_eq!(fs::read(foreign).unwrap(), b"unrelated staging fixture");
    }

    #[test]
    fn cleanup_failure_after_zero_exit_retains_first_draw_and_numeric_exit_detail() {
        let fixture = Fixture::new();
        let mut session = fixture.session(&fixture.paths);
        session.last_progress = Some(ProgressStage::FirstSuccessfulDraw);
        session.child_exit = Some(0);
        let temporary = fixture.paths.control.join(format!(
            "session-progress-{}-{}-{}.tmp",
            session.nonce,
            ProgressStage::FirstSuccessfulDraw.name(),
            "a".repeat(32)
        ));
        fs::write(&temporary, b"{partial owned fixture").unwrap();
        let error =
            cleanup_progress(&fixture.paths, &session.nonce, &Endpoint::default()).unwrap_err();
        session.phase = Phase::CleanupFailed;
        session.detail = retained_cleanup_detail(error, &session);
        write_json(&fixture.paths.session(), &session).unwrap();
        let record = matching_session(&fixture.paths, &session.nonce).unwrap();
        assert!(record
            .detail
            .contains("last_progress=first_successful_draw"));
        assert!(record.detail.contains("child_exit=0"));
        assert_eq!(fs::read(temporary).unwrap(), b"{partial owned fixture");
    }

    #[cfg(windows)]
    #[test]
    fn progress_sharing_during_atomic_publication_is_pending_but_cleanup_refuses_it() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        let nonce = fresh_nonce().unwrap();
        let previous = fixture
            .paths
            .progress(&nonce, ProgressStage::RendererInitialized);
        write_json(
            &previous,
            &progress_signal(&nonce, ProgressStage::RendererInitialized),
        )
        .unwrap();
        let final_path = fixture
            .paths
            .progress(&nonce, ProgressStage::FirstSuccessfulDraw);
        write_json(
            &final_path,
            &progress_signal(&nonce, ProgressStage::FirstSuccessfulDraw),
        )
        .unwrap();
        let held = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&final_path)
            .unwrap();
        assert_eq!(
            read_progress_file(
                &final_path,
                &nonce,
                ProgressStage::FirstSuccessfulDraw,
                &Endpoint::default()
            )
            .unwrap(),
            ProgressRead::Publishing
        );
        assert_eq!(
            read_progress(&fixture.paths, &nonce, &Endpoint::default()).unwrap(),
            Some(ProgressStage::RendererInitialized)
        );
        assert!(cleanup_progress(&fixture.paths, &nonce, &Endpoint::default()).is_err());
        assert!(previous.exists());
        drop(held);
        assert_eq!(
            read_progress(&fixture.paths, &nonce, &Endpoint::default()).unwrap(),
            Some(ProgressStage::FirstSuccessfulDraw)
        );
        cleanup_progress(&fixture.paths, &nonce, &Endpoint::default()).unwrap();
        assert!(!final_path.exists());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn linked_progress_final_and_temp_records_are_never_accepted_or_deleted() {
        let fixture = Fixture::new();
        let nonce = fresh_nonce().unwrap();
        let stage = ProgressStage::FirstSuccessfulDraw;
        let target = fixture.directory.join("foreign-progress.json");
        write_json(&target, &progress_signal(&nonce, stage)).unwrap();
        let path = fixture.paths.progress(&nonce, stage);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &path).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_file(&target, &path).is_err() {
            eprintln!("SKIPPED progress symlink fixture: missing Windows symlink privilege");
            return;
        }
        let before = fs::read(&target).unwrap();
        assert!(read_progress(&fixture.paths, &nonce, &Endpoint::default()).is_err());
        assert!(cleanup_progress(&fixture.paths, &nonce, &Endpoint::default()).is_err());
        fs::remove_file(&path).unwrap();
        let temporary = fixture.paths.control.join(format!(
            "session-progress-{nonce}-{}-{}.tmp",
            stage.name(),
            "a".repeat(32)
        ));
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &temporary).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &temporary).unwrap();
        assert!(cleanup_progress(&fixture.paths, &nonce, &Endpoint::default()).is_err());
        assert_eq!(fs::read(target).unwrap(), before);
    }

    #[test]
    fn failure_reader_is_strict_bounded_and_never_exposes_raw_diagnostics() {
        let fixture = Fixture::new();
        let nonce = fresh_nonce().unwrap();
        let normal = serde_json::to_value(managed_failure(&nonce)).unwrap();
        let file = fixture.paths.failure(&nonce);
        write_json(&file, &normal).unwrap();
        let accepted = read_failure(&fixture.paths, &nonce, &Endpoint::default(), false)
            .unwrap()
            .unwrap();
        assert!(
            failure_detail(&accepted, None, Some(2)).contains("client_assembly_location_mismatch")
        );
        for (field, value) in [
            ("protocol", json!(2)),
            ("session", json!("0".repeat(64))),
            ("fixture", json!(true)),
            ("stage", json!("foreign")),
            ("code", json!("password=fixture-secret&token=fixture-token")),
            ("exception", json!("path=/fixture/private.ini")),
            ("stage", json!("ready")),
            ("message", json!("private game exception")),
            ("endpoint", json!("https://osu.ppy.sh")),
        ] {
            let mut wrong = normal.clone();
            wrong[field] = value;
            write_json(&file, &wrong).unwrap();
            let error =
                read_failure(&fixture.paths, &nonce, &Endpoint::default(), false).unwrap_err();
            assert!(!error.contains("fixture-secret"));
            assert!(!error.contains("fixture-token"));
            assert!(!error.contains("private.ini"));
            assert!(!error.contains("private game exception"));
            assert!(file.exists());
        }
        let duplicate = format!(
            "{{\"protocol\":1,{}",
            serde_json::to_string(&normal)
                .unwrap()
                .trim_start_matches('{')
        );
        fs::write(&file, duplicate).unwrap();
        assert!(read_failure(&fixture.paths, &nonce, &Endpoint::default(), false).is_err());
        fs::write(&file, vec![b' '; MAX_FAILURE_BYTES as usize + 1]).unwrap();
        assert!(read_failure(&fixture.paths, &nonce, &Endpoint::default(), false).is_err());
        assert!(
            read_failure(&fixture.paths, "../../outside", &Endpoint::default(), false).is_err()
        );
        assert!(read_ready(&fixture.paths, "../../outside").is_err());
    }

    #[test]
    fn child_exit_zero_requires_prior_verified_readiness() {
        let fixture = Fixture::new();
        let session = fixture.session(&fixture.paths);
        write_json(&fixture.paths.session(), &session).unwrap();
        let error = finish_child_exit(&fixture.paths, &session, false, true, Some(0)).unwrap_err();
        assert!(error.contains("child_exit_before_first_draw_verification"));
        assert!(error.contains("child_exit=0"));
        assert_eq!(
            matching_session(&fixture.paths, &session.nonce)
                .unwrap()
                .phase,
            Phase::Error
        );
        write_json(&fixture.paths.session(), &session).unwrap();
        finish_child_exit(&fixture.paths, &session, true, true, Some(0)).unwrap();
        assert_eq!(
            matching_session(&fixture.paths, &session.nonce)
                .unwrap()
                .child_exit,
            Some(0)
        );
    }

    #[test]
    fn safe_failure_survives_exact_owned_artifact_and_sidecar_cleanup() {
        let fixture = Fixture::new();
        let mut session = fixture.session(&fixture.paths);
        let library = session.library.as_ref().unwrap();
        ensure_directory(&library.rulesets()).unwrap();
        let foreign = library.rulesets().join("osu.Game.Rulesets.Other.dll");
        fs::write(&foreign, b"unrelated fixture").unwrap();
        let (bridge, hash) = fixture.bridge();
        let mut managed = ManagedRuleset::open(library.rulesets()).unwrap();
        let receipt = managed.install(bridge, &hash).unwrap();
        session.install_session = Some(receipt.session_id);
        write_json(&fixture.paths.session(), &session).unwrap();
        write_json(
            &fixture.paths.failure(&session.nonce),
            &managed_failure(&session.nonce),
        )
        .unwrap();
        let error = finish_child_exit(&fixture.paths, &session, false, false, Some(2)).unwrap_err();
        assert!(error.contains("stage=assemblies code=client_assembly_location_mismatch"));
        assert!(error.contains("child_exit=2"));
        managed.cleanup().unwrap();
        cleanup_failure(&fixture.paths, &session.nonce).unwrap();
        update_session(&fixture.paths, &session.nonce, |record| {
            record.install_session = None
        })
        .unwrap();
        let persisted = matching_session(&fixture.paths, &session.nonce).unwrap();
        assert_eq!(persisted.phase, Phase::Error);
        assert_eq!(persisted.detail, error);
        assert_eq!(
            persisted.failure.unwrap().code,
            "client_assembly_location_mismatch"
        );
        assert!(!receipt.installed_path.exists());
        assert!(!fixture.paths.failure(&session.nonce).exists());
        assert_eq!(fs::read(foreign).unwrap(), b"unrelated fixture");
    }

    #[test]
    fn post_exit_read_preserves_failure_published_after_an_earlier_rejection() {
        let fixture = Fixture::new();
        let session = fixture.session(&fixture.paths);
        write_json(&fixture.paths.session(), &session).unwrap();
        let sidecar = fixture.paths.failure(&session.nonce);
        fs::write(&sidecar, b"{\"protocol\":").unwrap();
        let mut result =
            Err(
                read_failure(&fixture.paths, &session.nonce, &Endpoint::default(), false)
                    .unwrap_err(),
            );
        write_json(&sidecar, &managed_failure(&session.nonce)).unwrap();
        capture_closed_child(
            &fixture.paths,
            &session.nonce,
            Client::Lazer,
            Some(2),
            &mut result,
        )
        .unwrap();
        cleanup_failure(&fixture.paths, &session.nonce).unwrap();
        let record = matching_session(&fixture.paths, &session.nonce).unwrap();
        assert_eq!(
            record.failure.unwrap().code,
            "client_assembly_location_mismatch"
        );
        assert!(record.detail.contains("child_exit=2"));
        assert!(result
            .unwrap_err()
            .contains("client_assembly_location_mismatch"));
        assert!(!sidecar.exists());
    }

    #[cfg(windows)]
    #[test]
    fn native_host_failure_and_numeric_status_survive_an_absent_sidecar() {
        let fixture = Fixture::new();
        let paths = fixture.live_paths();
        let mut session = fixture.session(&paths);
        let own = process::current_identity().unwrap();
        session.launcher_identity = own.executable_identity.clone();
        session.guardian = Some(own.clone());
        session.child = Some(own);
        let library = session.library.as_ref().unwrap();
        ensure_directory(&library.rulesets()).unwrap();
        let (bridge, hash) = fixture.bridge();
        let mut managed = ManagedRuleset::open(library.rulesets()).unwrap();
        let receipt = managed.install(bridge, &hash).unwrap();
        session.install_session = Some(receipt.session_id);
        write_json(&paths.session(), &session).unwrap();
        let error = persist_host_failure(
            &paths,
            &session.nonce,
            "host_runtime_initialize_failed",
            Some(-1234),
        )
        .unwrap_err();
        assert!(error.contains("runtime_status=-1234"));
        fs::remove_file(paths.failure(&session.nonce)).unwrap();
        let error = finish_child_exit(&paths, &session, false, false, Some(2)).unwrap_err();
        assert!(error.contains("code=host_runtime_initialize_failed"));
        assert!(error.contains("runtime_status=-1234 child_exit=2"));
        let persisted = matching_session(&paths, &session.nonce).unwrap();
        assert_eq!(persisted.host_status, Some(-1234));
        assert_eq!(persisted.failure.unwrap().stage, "host");
        // This test owns only synthetic ruleset bytes; no game process exists.
        managed.cleanup().unwrap();
        assert!(!receipt.installed_path.exists());
        assert!(matching_session(&paths, &session.nonce).is_ok());
    }

    #[test]
    fn incompatible_safe_failure_records_are_rejected() {
        let nonce = fresh_nonce().unwrap();
        let first = managed_failure(&nonce);
        let mut second = first.clone();
        second.code = "assembly_load_failed".into();
        assert!(merge_failure(Some(first.clone()), Some(second))
            .unwrap_err()
            .contains("衝突"));
        assert_eq!(
            merge_failure(Some(first.clone()), None).unwrap(),
            Some(first)
        );
    }

    #[test]
    fn post_exit_rejection_preserves_conflicting_sidecar_and_prior_failure() {
        let fixture = Fixture::new();
        let mut session = fixture.session(&fixture.paths);
        let first = managed_failure(&session.nonce);
        session.failure = Some(first.clone());
        write_json(&fixture.paths.session(), &session).unwrap();
        let mut second = first.clone();
        second.code = "assembly_load_failed".into();
        let sidecar = fixture.paths.failure(&session.nonce);
        write_json(&sidecar, &second).unwrap();
        let before = fs::read(&sidecar).unwrap();
        let mut result = Ok(());
        capture_closed_child(
            &fixture.paths,
            &session.nonce,
            Client::Lazer,
            Some(2),
            &mut result,
        )
        .unwrap();
        assert!(result.unwrap_err().contains("衝突"));
        let record = matching_session(&fixture.paths, &session.nonce).unwrap();
        assert!(record.detail.contains("衝突"));
        assert_eq!(record.failure, Some(first));
        assert!(cleanup_failure(&fixture.paths, &session.nonce)
            .unwrap_err()
            .contains("衝突"));
        assert_eq!(fs::read(&sidecar).unwrap(), before);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn linked_failure_sidecar_is_preserved_and_never_accepted() {
        let fixture = Fixture::new();
        let nonce = fresh_nonce().unwrap();
        let target = fixture.directory.join("foreign-failure.json");
        write_json(&target, &managed_failure(&nonce)).unwrap();
        let before = fs::read(&target).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, fixture.paths.failure(&nonce)).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_file(&target, fixture.paths.failure(&nonce)).is_err() {
            eprintln!("SKIPPED failure symlink fixture: missing Windows symlink privilege");
            return;
        }
        assert!(read_failure(&fixture.paths, &nonce, &Endpoint::default(), false).is_err());
        assert!(cleanup_failure(&fixture.paths, &nonce).is_err());
        assert_eq!(fs::read(target).unwrap(), before);
    }

    #[test]
    fn production_ready_contract_rejects_test_fixture_and_cross_server_signals() {
        let nonce = fresh_nonce().unwrap();
        assert!(valid_nonce(&nonce));
        assert_ne!(nonce, fresh_nonce().unwrap());
        let normal = json!({"protocol":1,"session":nonce,"status":"ready","endpoint":SERVER,"fixture":false});
        let signal: Ready = serde_json::from_value(normal.clone()).unwrap();
        validate_ready(&signal, &nonce, &Endpoint::default()).unwrap();
        for (key, value) in [
            ("fixture", json!(true)),
            ("protocol", json!(2)),
            ("endpoint", json!("https://osu.ppy.sh")),
            ("session", json!("0".repeat(64))),
            ("status", json!("pending")),
        ] {
            let mut wrong = normal.clone();
            wrong[key] = value;
            assert!(validate_ready(
                &serde_json::from_value(wrong).unwrap(),
                &nonce,
                &Endpoint::default()
            )
            .is_err());
        }
        let mut unknown = normal;
        unknown["extra"] = json!(true);
        assert!(serde_json::from_value::<Ready>(unknown).is_err());
    }

    #[test]
    fn recovery_uses_recorded_library_even_after_preferences_and_selector_change() {
        let fixture = Fixture::new();
        let paths = &fixture.paths;
        let library = resolve_library(paths, &fixture.client).unwrap();
        ensure_directory(&library.rulesets()).unwrap();
        fs::write(library.root.join("client.realm"), b"realm marker").unwrap();
        fs::create_dir(library.root.join("files")).unwrap();
        fs::write(library.root.join("files/map.fixture"), b"resource marker").unwrap();
        fs::write(library.root.join("game.ini"), b"official config marker").unwrap();
        let (bridge, hash) = fixture.bridge();
        let mut manager = ManagedRuleset::open(library.rulesets()).unwrap();
        let receipt = manager.install(bridge, &hash).unwrap();
        drop(manager);
        let mut record = fixture.session(paths);
        record.install_session = Some(receipt.session_id);
        write_json(&paths.session(), &record).unwrap();
        let different = fixture.directory.join("different-library");
        fs::create_dir(&different).unwrap();
        fs::create_dir(different.join("rulesets")).unwrap();
        let foreign = different.join("rulesets").join(RULESET_FILENAME);
        fs::write(&foreign, b"foreign DLL must remain").unwrap();
        fs::write(
            library.root.join("storage.ini"),
            format!("FullPath = {}", different.display()),
        )
        .unwrap();
        write_json(&paths.prefs(), &Preferences::default()).unwrap();
        recover_at(paths).unwrap();
        assert!(!receipt.installed_path.exists());
        assert_eq!(fs::read(foreign).unwrap(), b"foreign DLL must remain");
        assert_eq!(
            fs::read(library.root.join("client.realm")).unwrap(),
            b"realm marker"
        );
        assert_eq!(
            fs::read(library.root.join("files/map.fixture")).unwrap(),
            b"resource marker"
        );
        assert_eq!(
            fs::read(library.root.join("game.ini")).unwrap(),
            b"official config marker"
        );
        assert_eq!(read_session(paths).unwrap().unwrap().phase, Phase::Idle);
    }

    #[test]
    fn recovery_preserves_owned_artifact_when_library_parent_identity_changes() {
        let fixture = Fixture::new();
        let paths = &fixture.paths;
        let library = resolve_library(paths, &fixture.client).unwrap();
        ensure_directory(&library.rulesets()).unwrap();
        let (bridge, hash) = fixture.bridge();
        let mut manager = ManagedRuleset::open(library.rulesets()).unwrap();
        let receipt = manager.install(bridge, &hash).unwrap();
        drop(manager);
        let mut record = fixture.session(paths);
        record.install_session = Some(receipt.session_id);
        write_json(&paths.session(), &record).unwrap();
        let displaced = fixture.directory.join("displaced-library");
        fs::rename(&library.root, &displaced).unwrap();
        fs::create_dir(&library.root).unwrap();
        // Even preserving the inner rulesets directory ID cannot authorize a
        // replacement library root. The separate session root ID must win.
        fs::rename(displaced.join("rulesets"), library.rulesets()).unwrap();
        assert!(recover_at(paths).unwrap_err().contains("身分已變更"));
        assert!(receipt.installed_path.exists());
        assert!(receipt.journal_path.exists());
        assert!(paths.session().exists());
    }

    #[test]
    fn cleanup_recovers_install_publication_before_receipt_persistence() {
        let fixture = Fixture::new();
        let paths = &fixture.paths;
        let record = fixture.session(paths);
        write_json(&paths.session(), &record).unwrap();
        let library = record.library.as_ref().unwrap();
        ensure_directory(&library.rulesets()).unwrap();
        let (bridge, hash) = fixture.bridge();
        let mut manager = ManagedRuleset::open(library.rulesets()).unwrap();
        let receipt = manager.install(bridge, &hash).unwrap();
        drop(manager);
        // Crash happened before the receipt UUID reached the session journal.
        assert!(record.install_session.is_none());
        recover_at(paths).unwrap();
        assert!(!receipt.installed_path.exists());
        assert!(!receipt.journal_path.exists());
    }

    #[test]
    fn legacy_v1_recovery_is_fixed_to_original_private_library_and_ready_path() {
        let fixture = Fixture::new();
        let paths = &fixture.paths;
        fs::create_dir(&paths.legacy_data).unwrap();
        fs::create_dir(paths.legacy_data.join("rulesets")).unwrap();
        let (bridge, hash) = fixture.bridge();
        let mut manager = ManagedRuleset::open(paths.legacy_data.join("rulesets")).unwrap();
        let receipt = manager.install(bridge, &hash).unwrap();
        drop(manager);
        let mut record = fixture.session(paths);
        record.version = 1;
        record.library = None;
        record.install_session = Some(receipt.session_id);
        write_json(&paths.session(), &record).unwrap();
        let signal = json!({"protocol":1,"session":record.nonce,"status":"ready","endpoint":SERVER,"fixture":false});
        let legacy_ready = paths
            .legacy_data
            .join(format!("session-ready-{}.json", record.nonce));
        write_json(&legacy_ready, &signal).unwrap();
        let modern_root = resolve_library(paths, &fixture.client).unwrap();
        ensure_directory(&modern_root.rulesets()).unwrap();
        let untouched = modern_root.rulesets().join(RULESET_FILENAME);
        fs::write(&untouched, b"foreign shared library DLL").unwrap();
        write_json(
            &paths.prefs(),
            &Preferences {
                stable: None,
                lazer: Some(fixture.client.clone()),
                endpoint: Endpoint::default(),
            },
        )
        .unwrap();
        recover_at(paths).unwrap();
        assert!(!receipt.installed_path.exists());
        assert!(!legacy_ready.exists());
        assert_eq!(fs::read(untouched).unwrap(), b"foreign shared library DLL");
        let migrated = read_session(paths).unwrap().unwrap();
        assert_eq!(migrated.version, 2);
        assert!(!migrated.legacy_ready);
        assert_eq!(migrated.phase, Phase::Idle);
    }

    #[test]
    fn new_ready_path_is_control_only_and_completed_records_allow_library_moves() {
        let fixture = Fixture::new();
        let paths = &fixture.paths;
        let mut record = fixture.session(paths);
        let library = record.library.as_ref().unwrap();
        let signal = json!({"protocol":1,"session":record.nonce,"status":"ready","endpoint":SERVER,"fixture":false});
        assert_eq!(
            paths.ready(&record.nonce).parent(),
            Some(paths.control.as_path())
        );
        write_json(&paths.ready(&record.nonce), &signal).unwrap();
        fs::write(
            library.root.join("session-ready-unrelated.json"),
            b"unrelated marker",
        )
        .unwrap();
        cleanup_session_ready(paths, &record).unwrap();
        assert!(!paths.ready(&record.nonce).exists());
        assert_eq!(
            fs::read(library.root.join("session-ready-unrelated.json")).unwrap(),
            b"unrelated marker"
        );
        record.phase = Phase::Idle;
        write_json(&paths.session(), &record).unwrap();
        fs::rename(&library.root, fixture.directory.join("moved-library")).unwrap();
        recover_at(paths).unwrap();
        assert_eq!(read_session(paths).unwrap().unwrap().phase, Phase::Idle);
    }

    #[test]
    fn completed_idle_session_allows_package_move_for_status_and_recovery() {
        for client in [Client::Stable, Client::Lazer] {
            let fixture = Fixture::new();
            let original = &fixture.paths;
            let mut record = fixture.session(original);
            record.client = client;
            record.phase = Phase::Idle;
            if client == Client::Stable {
                record.library = None;
            }
            write_json(&original.session(), &record).unwrap();
            let upgraded = fixture.upgraded_paths();
            fs::remove_file(&original.launcher).unwrap();

            assert_eq!(snapshot_at(&upgraded).unwrap().phase, Phase::Idle);
            recover_at(&upgraded).unwrap();
            let retained = matching_session(&upgraded, &record.nonce).unwrap();
            assert_eq!(retained.phase, Phase::Idle);
            assert_eq!(retained.launcher_executable, original.launcher);
            assert_eq!(retained.launcher_identity, record.launcher_identity);
            assert!(session_completed(&retained));
        }
    }

    #[test]
    fn upgraded_package_recovers_v2_owned_install_and_control_ready_signal() {
        let fixture = Fixture::new();
        let original = &fixture.paths;
        let mut record = fixture.session(original);
        let library = record.library.as_ref().unwrap();
        ensure_directory(&library.rulesets()).unwrap();
        fs::write(library.root.join("client.realm"), b"shared realm marker").unwrap();
        fs::write(library.root.join("game.ini"), b"official config marker").unwrap();
        let foreign = library.rulesets().join("foreign.ruleset.dll");
        fs::write(&foreign, b"foreign ruleset marker").unwrap();
        let (bridge, hash) = fixture.bridge();
        let mut managed = ManagedRuleset::open(library.rulesets()).unwrap();
        let receipt = managed.install(bridge, &hash).unwrap();
        drop(managed);
        record.install_session = Some(receipt.session_id);
        record.phase = Phase::CleanupFailed;
        write_json(&original.session(), &record).unwrap();
        let signal = json!({"protocol":1,"session":record.nonce,"status":"ready","endpoint":SERVER,"fixture":false});
        write_json(&original.ready(&record.nonce), &signal).unwrap();
        let upgraded = fixture.upgraded_paths();
        fs::remove_file(&original.launcher).unwrap();

        assert!(snapshot_at(&upgraded).unwrap().plugin_present);
        recover_at(&upgraded).unwrap();
        assert!(!receipt.installed_path.exists());
        assert!(!receipt.journal_path.exists());
        assert!(!original.ready(&record.nonce).exists());
        assert_eq!(fs::read(foreign).unwrap(), b"foreign ruleset marker");
        assert_eq!(
            fs::read(library.root.join("client.realm")).unwrap(),
            b"shared realm marker"
        );
        assert_eq!(
            fs::read(library.root.join("game.ini")).unwrap(),
            b"official config marker"
        );
        assert_eq!(read_session(&upgraded).unwrap().unwrap().phase, Phase::Idle);
    }

    #[test]
    fn upgraded_package_recovers_v1_only_from_its_historical_library() {
        let fixture = Fixture::new();
        let original = &fixture.paths;
        ensure_directory(&original.legacy_data.join("rulesets")).unwrap();
        let (bridge, hash) = fixture.bridge();
        let mut managed = ManagedRuleset::open(original.legacy_data.join("rulesets")).unwrap();
        let receipt = managed.install(bridge, &hash).unwrap();
        drop(managed);
        let mut record = fixture.session(original);
        let shared = record.library.as_ref().unwrap();
        ensure_directory(&shared.rulesets()).unwrap();
        let untouched = shared.rulesets().join(RULESET_FILENAME);
        fs::write(&untouched, b"foreign shared library DLL").unwrap();
        record.version = 1;
        record.library = None;
        record.install_session = Some(receipt.session_id);
        write_json(&original.session(), &record).unwrap();
        let signal = json!({"protocol":1,"session":record.nonce,"status":"ready","endpoint":SERVER,"fixture":false});
        let ready = original
            .legacy_data
            .join(format!("session-ready-{}.json", record.nonce));
        write_json(&ready, &signal).unwrap();
        let upgraded = fixture.upgraded_paths();
        fs::remove_file(&original.launcher).unwrap();

        recover_at(&upgraded).unwrap();
        assert!(!receipt.installed_path.exists());
        assert!(!receipt.journal_path.exists());
        assert!(!ready.exists());
        assert_eq!(fs::read(untouched).unwrap(), b"foreign shared library DLL");
        let migrated = read_session(&upgraded).unwrap().unwrap();
        assert_eq!(migrated.version, 2);
        assert_eq!(migrated.phase, Phase::Idle);
        assert_eq!(migrated.launcher_executable, original.launcher);
        assert!(!migrated.legacy_ready);
    }

    #[test]
    fn malformed_library_and_cross_library_install_session_preserve_artifacts() {
        let fixture = Fixture::new();
        let paths = &fixture.paths;
        let mut record = fixture.session(paths);
        let library = record.library.as_ref().unwrap();
        ensure_directory(&library.rulesets()).unwrap();
        let (bridge, hash) = fixture.bridge();
        let mut manager = ManagedRuleset::open(library.rulesets()).unwrap();
        let receipt = manager.install(bridge, &hash).unwrap();
        drop(manager);
        record.install_session = Some(Uuid::new_v4());
        write_json(&paths.session(), &record).unwrap();
        assert!(recover_at(paths).unwrap_err().contains("工作階段身分不符"));
        assert!(receipt.installed_path.exists());
        let original = serde_json::to_value(&record).unwrap();
        for invalid in [
            json!({"root":"relative", "identity":"unknown"}),
            json!({"root":library.root, "identity":""}),
            json!({"root":library.root,"identity":library.identity,"deletePath":fixture.directory}),
        ] {
            let mut value = original.clone();
            value["library"] = invalid;
            write_json(&paths.session(), &value).unwrap();
            assert!(read_session(paths).is_err());
            assert!(receipt.installed_path.exists());
        }
        let mut legacy = original;
        legacy["version"] = json!(1);
        write_json(&paths.session(), &legacy).unwrap();
        assert!(read_session(paths).is_err());
    }

    #[test]
    fn legacy_preinstallation_crash_can_recover_but_new_session_requires_library() {
        let fixture = Fixture::new();
        let mut record = fixture.session(&fixture.paths);
        record.library = None;
        write_json(&fixture.paths.session(), &record).unwrap();
        assert!(read_session(&fixture.paths).is_err());
        record.version = 1;
        write_json(&fixture.paths.session(), &record).unwrap();
        recover_at(&fixture.paths).unwrap();
        assert_eq!(
            read_session(&fixture.paths).unwrap().unwrap().phase,
            Phase::Idle
        );
        assert!(!fixture.paths.legacy_data.exists());
    }

    #[test]
    fn preferences_and_session_parsing_are_bounded_and_cannot_add_commands_or_paths() {
        let fixture = Fixture::new();
        write_json(
            &fixture.paths.prefs(),
            &json!({"stable":null,"lazer":null,"server":"https://elsewhere.test"}),
        )
        .unwrap();
        assert!(read_preferences(&fixture.paths).is_err());
        fs::write(
            fixture.paths.prefs(),
            vec![b' '; MAX_RECORD_BYTES as usize + 1],
        )
        .unwrap();
        assert!(read_preferences(&fixture.paths).is_err());
        let mut session = serde_json::to_value(fixture.session(&fixture.paths)).unwrap();
        session["launcherExecutable"] = json!("relative-launcher.exe");
        write_json(&fixture.paths.session(), &session).unwrap();
        assert!(read_session(&fixture.paths).is_err());
        session = serde_json::to_value(fixture.session(&fixture.paths)).unwrap();
        session["selectedExecutable"] = json!(fixture.directory.join("foreign.exe"));
        write_json(&fixture.paths.session(), &session).unwrap();
        assert!(read_session(&fixture.paths).is_err());
        assert!(fixture.paths.session().exists());
    }

    #[test]
    fn executable_pe_architecture_layout_and_release_hash_are_required() {
        let fixture = Fixture::new();
        validate_client(&fixture.client, Client::Stable).unwrap();
        assert!(validate_client(&fixture.client, Client::Lazer).is_err());
        fixture.layout();
        validate_client(&fixture.client, Client::Lazer).unwrap();
        let (bridge, hash) = fixture.bridge();
        verify_bridge_with_hash(&fixture.paths, &hash).unwrap();
        assert!(verify_bridge_with_hash(&fixture.paths, &"0".repeat(64)).is_err());
        assert!(verify_bridge_with_hash(&fixture.paths, "not-a-hash").is_err());
        assert!(verify_lazer_compatibility(&fixture.paths, &fixture.client).is_err());
        fs::write(&bridge, b"tampered bridge").unwrap();
        assert!(verify_bridge_with_hash(&fixture.paths, &hash).is_err());
        write_pe(&fixture.client, 0x14c);
        validate_client(&fixture.client, Client::Stable).unwrap();
        assert!(validate_client(&fixture.client, Client::Lazer).is_err());
        fs::write(&fixture.client, b"not PE").unwrap();
        assert!(validate_client(&fixture.client, Client::Stable).is_err());
    }

    #[test]
    fn an_install_journal_without_process_ownership_is_preserved() {
        let fixture = Fixture::new();
        write_json(
            &fixture.paths.prefs(),
            &Preferences {
                stable: None,
                lazer: Some(fixture.client.clone()),
                endpoint: Endpoint::default(),
            },
        )
        .unwrap();
        let (bridge, hash) = fixture.bridge();
        ensure_directory(
            &resolve_library(&fixture.paths, &fixture.client)
                .unwrap()
                .rulesets(),
        )
        .unwrap();
        let mut installation = ManagedRuleset::open(
            resolve_library(&fixture.paths, &fixture.client)
                .unwrap()
                .rulesets(),
        )
        .unwrap();
        let receipt = installation.install(bridge, &hash).unwrap();
        drop(installation);
        assert!(recover_at(&fixture.paths)
            .unwrap_err()
            .contains("程序所有權"));
        assert!(receipt.installed_path.exists());
        assert!(receipt.journal_path.exists());
    }

    #[cfg(windows)]
    #[test]
    fn upgraded_launcher_refuses_live_child_and_handles_pid_reuse_without_closing_it() {
        let fixture = Fixture::new();
        let paths = fixture.live_paths();
        let (bridge, hash) = fixture.bridge();
        ensure_directory(&resolve_library(&paths, &fixture.client).unwrap().rulesets()).unwrap();
        let mut managed =
            ManagedRuleset::open(resolve_library(&paths, &fixture.client).unwrap().rulesets())
                .unwrap();
        let receipt = managed.install(bridge, &hash).unwrap();
        drop(managed);
        let own = process::current_identity().unwrap();
        let mut record = fixture.session(&paths);
        record.launcher_identity = own.executable_identity.clone();
        record.child = Some(own.clone());
        record.install_session = Some(receipt.session_id);
        record.phase = Phase::Running;
        write_json(&paths.session(), &record).unwrap();
        let upgraded = fixture.upgraded_paths();
        assert_eq!(snapshot_at(&upgraded).unwrap().phase, Phase::Running);
        assert!(recover_at(&upgraded).unwrap_err().contains("仍在執行"));
        assert!(receipt.installed_path.exists());
        // Same PID, different creation time proves the recorded child is gone.
        record.child.as_mut().unwrap().creation_time += 1;
        write_json(&paths.session(), &record).unwrap();
        recover_at(&upgraded).unwrap();
        assert!(!receipt.installed_path.exists());
        assert_eq!(read_session(&upgraded).unwrap().unwrap().phase, Phase::Idle);
        assert_eq!(process::state(&own).unwrap(), process::State::Live);
    }

    #[cfg(windows)]
    #[test]
    fn upgraded_launcher_preserves_live_guardian_and_ambiguous_process_identity() {
        let fixture = Fixture::new();
        let paths = fixture.live_paths();
        let own = process::current_identity().unwrap();
        let mut record = fixture.session(&paths);
        record.launcher_identity = own.executable_identity.clone();
        record.guardian = Some(own);
        write_json(&paths.session(), &record).unwrap();
        let upgraded = fixture.upgraded_paths();
        assert!(recover_at(&upgraded).unwrap_err().contains("仍在執行"));
        // An internally consistent record with a changed image identity remains ambiguous.
        record.launcher_identity.push_str("-changed");
        record.guardian.as_mut().unwrap().executable_identity = record.launcher_identity.clone();
        write_json(&paths.session(), &record).unwrap();
        assert!(recover_at(&upgraded).unwrap_err().contains("無法確認"));
        assert!(paths.session().exists());
    }

    #[cfg(windows)]
    #[test]
    fn upgraded_package_cannot_take_over_a_previous_guardian_launch() {
        let fixture = Fixture::new();
        let record = fixture.session(&fixture.paths);
        write_json(&fixture.paths.session(), &record).unwrap();
        let before = fs::read(fixture.paths.session()).unwrap();
        let upgraded = fixture.upgraded_paths();
        assert!(matching_session(&upgraded, &record.nonce).is_ok());
        assert!(run_guardian(&upgraded, &record.nonce)
            .unwrap_err()
            .contains("守護程序並非此工作階段的啟動器"));
        assert_eq!(fs::read(fixture.paths.session()).unwrap(), before);
        assert!(read_session(&upgraded).unwrap().unwrap().guardian.is_none());
    }

    #[test]
    fn control_lock_serializes_concurrent_record_transitions() {
        let fixture = Fixture::new();
        let paths = fixture.paths.clone();
        let lock = ControlLock::open(&paths).unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let other = thread::spawn(move || {
            let _lock = ControlLock::open(&paths).unwrap();
            send.send(()).unwrap();
        });
        assert!(receive.recv_timeout(Duration::from_millis(30)).is_err());
        drop(lock);
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        other.join().unwrap();
    }

    #[test]
    fn inherited_clr_hooks_are_removed_case_insensitively() {
        for name in [
            "DOTNET_STARTUP_HOOKS",
            "dotnet_startup_hooks",
            "COR_ENABLE_PROFILING",
            "CORECLR_PROFILER_PATH",
            "COMPlus_ReadyToRun",
            "DOTNET_ROOT_X64",
            "DOTNET_ADDITIONAL_DEPS",
            "DOTNET_DiagnosticPorts",
        ] {
            assert!(is_injection_environment_name(name));
        }
        for name in [
            "LOCALAPPDATA",
            "PATH",
            "ILLYAVERSE_LAZER_SESSION",
            "OSU_DISABLE_ERROR_REPORTING",
        ] {
            assert!(!is_injection_environment_name(name));
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn linked_preferences_and_profile_roots_are_refused() {
        let fixture = Fixture::new();
        let target = fixture.directory.join("foreign.json");
        fs::write(&target, b"{\"stable\":null,\"lazer\":null}").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, fixture.paths.prefs()).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_file(&target, fixture.paths.prefs()).is_err() {
            eprintln!("SKIPPED runtime symlink fixture: missing Windows symlink privilege");
            return;
        }
        assert!(read_preferences(&fixture.paths).is_err());
        assert_eq!(
            fs::read(target).unwrap(),
            b"{\"stable\":null,\"lazer\":null}"
        );
        let profile_link = fixture.directory.join("profile-link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&fixture.paths.control, &profile_link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&fixture.paths.control, &profile_link).unwrap();
        assert!(ensure_directory(&profile_link.join("rulesets")).is_err());
        assert!(!fixture.paths.control.join("rulesets").exists());
    }
}
