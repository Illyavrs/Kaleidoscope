# Signed GitHub updates

Kaleidoscope is an independent Git repository and an Illyaverse submodule. Its
Rust core and desktop interface share that repository. Transfiguration remains
an independently versioned sibling submodule. There are two signed update feeds:
a complete Kaleidoscope release includes its launcher and bundled bridge, while
a Transfiguration component release updates the bridge and its associated files
without rebuilding Kaleidoscope. Both preserve the existing submodule structure.

This implementation provides a release format and an operator publishing
workflow. It does not create a GitHub repository, signing key, tag, or release.
Until the operator configures a published channel and its signing key, automatic
updates remain unavailable. Package integrity does not prove native login,
gameplay, or cleanup; preserve those separate verification results.

## Configure the channel

Build the launcher with these explicit settings:

| Setting | Value |
| --- | --- |
| `KALEIDOSCOPE_UPDATE_REPOSITORY` | The controlled GitHub repository, `Illyavrs/Kaleidoscope` |
| `KALEIDOSCOPE_UPDATE_PUBLIC_KEY` | The Ed25519 public key's raw 32 bytes, as 64 lowercase hexadecimal characters |
| `KALEIDOSCOPE_UPDATE_VERSION` | The package's semantic version, for example `0.1.0` |
| `TRANSFIGURATION_BUNDLED_VERSION` | The exact bundled bridge version, for example the current base `0.1.0` |

Both feeds accept canonical stable `major.minor.patch` versions only. Prerelease
suffixes, build metadata, leading zeroes, and numeric components above the native
64-bit semantic-version limit are rejected.

The public key and repository are compiled trust anchors. They are not obtained
from downloaded update metadata or exposed as a player-selectable trust setting.
Keep the corresponding private signing key outside source control, package
payloads, release directories, logs, and client installations. Key generation
and custody are operator responsibilities. This workflow never generates keys.

The publisher accepts exactly one signing-key source:
`KALEIDOSCOPE_UPDATE_SIGNING_KEY_PEM`, containing an Ed25519 PKCS#8 PEM, or
`KALEIDOSCOPE_UPDATE_SIGNING_KEY_FILE`, pointing to that PEM outside the package
and output directories. The key must match the configured public key. Only the
public key is included in a launcher build.

## Prepare a release

Build through the complete Illyaverse parent checkout. Its lockfiles, shared
crates, and pinned Transfiguration checkout are required; the child repository
does not provide a standalone launcher build. The existing GUI package builder
uses verified installed stock assemblies without redistributing an osu! client.

Commit both components and pin them in the parent before building. Record the
exact commits and package hashes in the build receipt. The receipt must establish
that these bytes were built from those commits; a manifest signature alone
cannot establish that provenance. Build the launcher with the intended release
version, bundled bridge version, repository and public key. The publisher checks
all four against `--verify-update-config`; its six fields are `protocol`,
`configured`, `repository`, `public_key`, `version`, and `bridge_version`. This
diagnostic exits before player storage or game initialization.

Use the existing `scripts/package-launcher.ps1` and GUI package verifier. The
operator supplies a complete package directory and its matching ZIP. CLI-only
packages, synthetic fixtures, unexpected files, linked paths, an incomplete
package, or changed bytes are rejected. The publisher executes the existing
bounded PowerShell verifier and the package's diagnostic before signing. Only
use a package produced by your reviewed build; this step executes its launcher
diagnostic. Signing keys and GitHub tokens are excluded from child-process
environments, and verifier output is not logged.

From the parent checkout, with the channel environment already configured:

```powershell
node tools/kaleidoscope/scripts/create-update-release.mjs `
  --version 0.1.0 `
  --bridge-version 0.1.0 `
  --repository Illyavrs/Kaleidoscope `
  --package-directory .artifacts/packages/your-verified-gui-package `
  --archive .artifacts/packages/your-verified-gui-package.zip `
  --output-directory .artifacts/update-releases/0.1.0 `
  --kaleidoscope-commit <actual-40-character-commit> `
  --transfiguration-commit <actual-40-character-commit>
```

The output's parent directory must exist. Choose a new output directory for
every attempt: existing releases are never overwritten. The source checkouts
must be clean and their actual HEAD commits must equal the supplied commits.
The publisher emits only `updates.json` and
`Kaleidoscope-0.1.0-windows-x64.zip`. Failure after staging may leave an incomplete
new directory for inspection; it is not a publishable release.

## Protocol

`updates.json` is a UTF-8 JSON envelope with exactly two fields:

```json
{"manifest":"BASE64_OF_EXACT_UTF8_JSON_BYTES","signature":"BASE64_OF_64_BYTE_ED25519_SIGNATURE"}
```

The signature covers the decoded manifest bytes, without parsing or
reserializing them. The signed inner JSON has exactly these fields:

```json
{
  "protocol": 1,
  "version": "0.1.0",
  "platform": "windows-x64",
  "repository": "Illyavrs/Kaleidoscope",
  "tag": "v0.1.0",
  "asset": "Kaleidoscope-0.1.0-windows-x64.zip",
  "bytes": 12345,
  "sha256": "64_lowercase_hex_characters",
  "launcher_sha256": "64_lowercase_hex_characters",
  "bridge_sha256": "64_lowercase_hex_characters",
  "bridge_version": "0.1.0",
  "kaleidoscope_commit": "40_lowercase_hex_characters",
  "transfiguration_commit": "40_lowercase_hex_characters",
  "files": {"every_exact_GUI_payload_name_including_package.json": "64_lowercase_hex_characters"}
}
```

The example's hash placeholders are illustrative, not valid values. `files`
contains exactly the current 17 GUI package files, including licensing files and
`package.json`. It contains no updater helper or extra nested directory. The
ZIP is flat at its root, with only the existing `licenses/` subdirectory. The
archive and total uncompressed payload are limited to 512 MiB; individual files
are limited to 256 MiB and package JSON to 1 MiB. Launcher and bridge hashes must
equal their `files` entries. The release tag, repository, and exact asset name
bind the manifest to the configured GitHub channel. The launcher-feed signature
covers its complete package. The separate bridge feed signs all ten component
files before they are composed into an owned bundle. The signed `bridge_version`
must match the new launcher's compiled bundled version; a complete launcher update
with an older bridge than the authenticated installed component is rejected.
The launcher feed does not silently downgrade an independently updated bridge.

## Publish through GitHub Actions

The child workflow `.github/workflows/publish-update.yml` is manually dispatched
in the published Kaleidoscope repository. Configure the
`kaleidoscope-updates` GitHub environment with:

- Variables `KALEIDOSCOPE_UPDATE_REPOSITORY` and `KALEIDOSCOPE_UPDATE_PUBLIC_KEY`.
- Secret `KALEIDOSCOPE_UPDATE_SIGNING_KEY_PEM`.
- If the parent or its submodules are private, secret
  `ILLYAVERSE_ARTIFACT_READ_TOKEN`, limited to reading those repositories and
  their Actions artifacts. The release itself uses the child repository's
  scoped workflow token.

Publish all parent submodule remotes first; local bare paths in a development
`.gitmodules` cannot be cloned by a GitHub runner. Create the reviewed
`v<version>` child tag. Provide the exact parent commit and successful parent
build run ID and exact bundled bridge version when dispatching. The selected
build artifact must contain only
`package/` and `package.zip`, representing the verified complete GUI package.
The parent build must preserve its provenance receipt separately.

The workflow checks the build run's repository, success state and immutable HEAD,
checks that the parent's Kaleidoscope submodule equals the release tag, and uses
the actual Transfiguration sibling pin. It runs publisher unit tests, validates
the compiled channel and package, then signs. Publication creates a new release
with both assets; an existing release or tag mismatch fails rather than replacing
assets. This workflow consumes an approved build artifact; it does not provide
or claim a stock-client build job.

## Configure the independent Transfiguration feed

Compile these additional settings into Kaleidoscope to enable bridge-only
updates:

| Setting | Value |
| --- | --- |
| `TRANSFIGURATION_UPDATE_REPOSITORY` | The controlled GitHub repository, `Illyavrs/Transfiguration` |
| `TRANSFIGURATION_UPDATE_PUBLIC_KEY` | That feed's separate Ed25519 public key, as 64 lowercase hexadecimal characters |
| `TRANSFIGURATION_BUNDLED_VERSION` | The bridge version in the complete launcher package; the current base is `0.1.0` |

Keep the corresponding Transfiguration private key separate from the Kaleidoscope
signing key. Neither feed can supply or replace its own trusted public key. A
missing channel configuration leaves that feed unavailable; it does not authorize
unsigned downloads or an official-server fallback.

The component feed uses its own `updates.json` envelope with an Ed25519 signature
over the exact decoded UTF-8 manifest bytes. Its manifest binds the Transfiguration
repository, `v<version>` tag, `Transfiguration-<version>-windows-x64.zip`, archive
length and SHA-256, bridge SHA-256, source commit, `launcher_requirement`, and
exact hashes of these ten files:

- `osu.Game.Rulesets.Transfiguration.dll`
- `artifact.json`
- `compatibility.json`
- `THIRD-PARTY-NOTICES.md`
- `Transfiguration-README.md`
- `licenses/DotNet.txt`
- `licenses/Harmony.txt`
- `licenses/Iced.txt`
- `licenses/Mono.Cecil.txt`
- `licenses/MonoMod.txt`

The component ZIP contains the complete ten-file bundle, not a loose DLL. The
Kaleidoscope release ZIP remains exactly the existing 17-file GUI package; this
does not add nested repositories or change its publisher workflow.

## Publish and apply a bridge component release

Use the [Transfiguration repository's publisher instructions](../../transfiguration/README.md)
for the component release. Prepare a reviewed production bridge build and record
its actual source commit and hashes. Declare a semantic-version
`launcher_requirement` that includes every supported launcher version, sign the
complete component manifest with the Transfiguration key, and publish its
manifest and ZIP together in the configured repository. Bridge fixes and newly
verified exact stock-client builds can then be delivered without a new launcher
build when the declared launcher requirement remains satisfied.

Kaleidoscope verifies the component signature, configured channel, release
identity, every file hash, and the launcher version requirement before applying
it. The component is installed through the same guarded whole-bundle transaction
as the launcher package. The persisted `bridge-update.json` is authoritative
signed provenance for the installed component. Missing trust, an invalid signed
record, or changed component bytes fail closed; an existing invalid override is
not silently replaced with the bundled DLL.

An unsigned bundled bridge retains Kaleidoscope's compiled legacy native pins.
A correctly signed independent component can authorize a newly verified exact
triplet of `osu.Game.dll`, `osu.Framework.dll`, and `osu!.dll` hashes through its
typed `compatibility.json`. That metadata requires a precise `native_client_version`,
exact version/hash records for those three assemblies plus `System.Net.Http.dll`
and `System.Net.WebSockets.Client.dll`, and the required private-server policy and
native contracts. It accepts no wildcard or native-version range. The signature
does not remove startup compatibility checks; it supplies the exact authenticated
values those checks use.

Before publishing support for a newer stock client, the operator must retain
actual production and native verification records for that specific version.
No future-version test result follows from accepting the signed metadata format.
Changes outside the existing launcher contracts still require a new Kaleidoscope
release, and an unsatisfied `launcher_requirement` is rejected. Close active
client sessions before updating. The release format and synthetic tests alone
do not prove installation in a player's client.

## Verify this code

```powershell
node --test tools/kaleidoscope/scripts/create-update-release.test.mjs
```

These tests use synthetic file payloads and ephemeral in-memory keys. They check
the exact inventory, signature bytes, tampering, channel binding, size bounds and
linked-path rejection. They do not sign a real package, invoke a game, exercise
GitHub publication, or demonstrate native update installation.
