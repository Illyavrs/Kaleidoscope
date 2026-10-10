<h1 align="center">Kaleidoscope</h1>

<p align="center">
  <strong>A Windows launcher for osu! stable and lazer, with a Rust core, CLI and Tauri / Next.js desktop interface.</strong>
</p>

Kaleidoscope connects installed clients to Illyaverse: stable uses
`-devserver <hostname>`, while lazer loads the project's
[Transfiguration bridge](https://github.com/Illyavrs/Transfiguration) before startup.

## Launch a client

1. Register on the operator's Illyaverse website.
2. Extract the complete operator-provided GUI package and open `Kaleidoscope.exe`.
3. Select the client's `osu!.exe`. Without a saved selection, stable detects
   `%LOCALAPPDATA%\osu!\osu!.exe`; lazer and custom installations need a selection.
4. Save the server's HTTPS origin in Settings, launch, and sign in with the
   website account.

The default endpoint is `https://illya.test`. Saved settings apply to the next
launch; the current session keeps its original endpoint. Public servers need
DNS and trusted HTTPS. The development endpoint needs
[local DNS and CA setup](https://github.com/Illyavrs/Illyaverse/blob/main/apps/docs/content/docs/clients/local-client-testing.md).
See the [launcher guide](https://github.com/Illyavrs/Illyaverse/blob/main/apps/docs/content/docs/clients/kaleidoscope-launcher.md)
for endpoint rules, restart and recovery.

lazer reuses the existing beatmap library and shared `game.ini` preferences.
Private accounts and sessions are isolated by endpoint in `auth.illyaverse.json`;
official account fields are preserved. Unsupported client files or uncertain
storage stop startup, and failed connections never fall back to official servers.
After exit, a guardian removes only the unchanged bridge DLL installed by that
session. Keep the package in place until cleanup finishes. See
[connection requirements](https://github.com/Illyavrs/Illyaverse/blob/main/apps/docs/content/docs/clients/connection.md)
and the [exact compatibility manifest](https://github.com/Illyavrs/Transfiguration/blob/main/compatibility.json).

## CLI

These commands use the separate CLI package; use desktop controls in the GUI
package.

```powershell
.\Kaleidoscope.exe launcher set-endpoint "https://your-illyaverse.example"
.\Kaleidoscope.exe launcher start lazer "C:\Games\osu!lazer\osu!.exe"
.\Kaleidoscope.exe launcher status
.\Kaleidoscope.exe launcher recover
```

## Develop and package

This independently versioned repository is an Illyaverse submodule. Build from
the complete parent checkout, which supplies workspace manifests, lockfiles,
shared source and the pinned bridge.

Use Node.js 22 or newer and Rust 1.96. Native Windows packaging also needs MSVC,
.NET 10 SDK and a supported installed lazer client. From the parent directory:

```powershell
npm ci
npm run dev --workspace @illyaverse/kaleidoscope
cargo run -p kaleidoscope -- --help
pwsh -NoProfile -File tools/kaleidoscope/scripts/package-launcher.ps1 -BuildProfile Debug
```

The interface runs on port 3010. Packaging builds the bridge, production interface
and native launcher into a directory and ZIP, without bundling osu!. See the
launcher guide for custom paths and verification.

The manual parent CI producer builds verified Release packages on a dedicated
Windows runner. See [Release builds](https://github.com/Illyavrs/Illyaverse/blob/main/apps/docs/content/docs/deploy/launcher-release-builds.md)
for runner configuration, source receipts and separate publishing. Actual CI
Release builds remain pending.

## Updates and status

Signed updates have independent feeds for complete launcher packages from
`Illyavrs/Kaleidoscope` and bridge components from `Illyavrs/Transfiguration`.
Each requires its own compiled trust key. Publication and trust keys are not
configured, so automatic updates remain unavailable. See [update setup](docs/updates.md).

Local GUI packages are unsigned Debug test builds. Native client verification
remains pending, and production Release packaging currently encounters Windows
error 5. See the
[current integration record](https://github.com/Illyavrs/Illyaverse/blob/main/apps/docs/content/docs/reference/release-readiness.md).

Licensed under the [MIT License](LICENSE). Packages include dependency notices
and licenses.
