fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    compile_windows_resources();
}

#[cfg(windows)]
fn compile_windows_resources() {
    use tauri_winres::{VersionInfo, WindowsResource};

    let [major, minor, patch] = [
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH"),
    ]
    .map(|component| {
        component
            .parse::<u16>()
            .expect("Kaleidoscope VERSIONINFO components must fit in 16 bits")
    });
    let version = format!("{major}.{minor}.{patch}.0");
    let fixed_version =
        (u64::from(major) << 48) | (u64::from(minor) << 32) | (u64::from(patch) << 16);

    // Native libraries inspect the current process image's RT_VERSION resource.
    // Describe our own launcher; never impersonate the client we host.
    WindowsResource::new()
        .set("ProductName", "Kaleidoscope")
        .set("FileDescription", "Illyaverse launcher")
        .set("InternalName", "kaleidoscope")
        .set("OriginalFilename", "Kaleidoscope.exe")
        .set("FileVersion", &version)
        .set("ProductVersion", &version)
        .set_version_info(VersionInfo::FILEVERSION, fixed_version)
        .set_version_info(VersionInfo::PRODUCTVERSION, fixed_version)
        // Only the CLI executable hosts the client. Do not attach this resource
        // to libraries, runtime probes, test fixtures, or the separate Tauri UI.
        .compile_for(&["kaleidoscope"])
        .expect("Kaleidoscope Windows VERSIONINFO is required; install the Windows SDK resource compiler");
}

#[cfg(not(windows))]
fn compile_windows_resources() {
    // Platform-specific build dependencies are selected for the host. Refuse a
    // cross-host Windows build instead of producing an EXE without RT_VERSION.
    panic!("Kaleidoscope Windows binaries require a Windows build host and Windows SDK resource compiler for VERSIONINFO");
}
