//! Real hostfxr check for the Linux host. It needs a .NET runtime, a trivial
//! framework-dependent app and a startup-hook assembly, so it is ignored by
//! default. `tests/host-linux.sh` builds the fixtures and runs it in a container.
#![cfg(unix)]

use std::path::PathBuf;

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is not set")))
}

#[test]
#[ignore = "needs a .NET runtime; run tests/host-linux.sh"]
fn hosts_an_application_with_the_mandatory_startup_hook() {
    let hostfxr = env_path("KALEIDOSCOPE_TEST_HOSTFXR");
    let application = env_path("KALEIDOSCOPE_TEST_APP");
    let hook = env_path("KALEIDOSCOPE_TEST_HOOK");
    let marker = env_path("KALEIDOSCOPE_TEST_MARKER");
    let _ = std::fs::remove_file(&marker);
    let code = kaleidoscope::launcher::host::run_managed(&application, &hostfxr, &hook)
        .expect("the managed application runs under the native host");
    // The fixture app exits with 7; the hook must have run before Main.
    assert_eq!(code, 7);
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().trim(),
        "hook-before-main"
    );
}

#[test]
fn refuses_missing_or_relative_paths_without_loading_anything() {
    let absent = PathBuf::from("/nonexistent/libhostfxr.so");
    let result = kaleidoscope::launcher::host::run_managed(&absent, &absent, &absent);
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("host_path_invalid"));
}
