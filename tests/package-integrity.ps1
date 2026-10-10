param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../..')).Path
Import-Module (Join-Path $PSScriptRoot '../scripts/launcher-package.psm1') -Force
$identity = [guid]::NewGuid().ToString('N')
$fixtureRoot = Assert-PackageWorkspacePath (Join-Path $repo ('data-kaleidoscope-package-fixtures-' + $identity)) $repo
$checks = 0

function Write-Fixture([string]$Path, [string]$Text) {
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path)) | Out-Null
    [IO.File]::WriteAllText($Path, $Text, [Text.UTF8Encoding]::new($false))
}
function Write-FixtureJson([string]$Path, $Value) { Write-Fixture $Path ($Value | ConvertTo-Json -Depth 20) }
function Assert-Fixture([bool]$Condition, [string]$Message) { if (!$Condition) { throw $Message } }
function Expect-Refusal([string]$Name, [scriptblock]$Action, [string]$MessagePattern = '*') {
    try { $null = & $Action } catch {
        if ($_.Exception.Message -notlike $MessagePattern) { throw "Unexpected failure in ${Name}: $($_.Exception.Message)" }
        $script:checks++
        Write-Output "PASS $Name"
        return
    }
    throw "Integrity boundary did not refuse: $Name"
}
function New-FixtureSource([string]$Name) {
    $source = Join-Path $fixtureRoot ('source-' + $Name)
    Copy-Item -LiteralPath (Join-Path $fixtureRoot 'source-baseline') -Destination $source -Recurse
    return $source
}
function New-FixturePackage([string]$Source, [string]$Name, [hashtable]$Evidence) {
    if (!$Evidence) { $Evidence = @{ kind = 'synthetic_package_integrity'; launcher_sha256 = (Get-FileHash -LiteralPath (Join-Path $Source 'Kaleidoscope.exe') -Algorithm SHA256).Hash; exit_code = $null } }
    return New-KaleidoscopePackage -WorkspaceRoot $repo -LauncherPath (Join-Path $Source 'Kaleidoscope.exe') -BridgeDirectory (Join-Path $Source 'bridge') -LicensePath (Join-Path $Source 'LICENSE') -ExpectedCompatibilityPath (Join-Path $fixtureRoot 'expected-compatibility.json') -OutputDirectory (Join-Path $fixtureRoot ('output-' + $Name)) -UiDirectory (Join-Path $Source 'ui') -InterfaceEvidence $Evidence
}
function Copy-FixturePackage([string]$Name) {
    $copy = Join-Path $fixtureRoot ('tamper-' + $Name)
    Copy-Item -LiteralPath $script:valid.Directory -Destination $copy -Recurse
    return $copy
}

function Test-SourceBindingsJson([string]$Text) {
    $assemblyHashes = @{
        'osu.Game.dll' = @{ sha256 = ('1' * 64) }
        'osu.Framework.dll' = @{ sha256 = ('2' * 64) }
        'osu!.dll' = @{ sha256 = ('3' * 64) }
    }
    & (Get-Module launcher-package) {
        param($Json, $Compatibility, $Hash)
        $bindings = Read-PackageJson $Json 'synthetic CLI bindings'
        Assert-CliPackageBindings $bindings $Compatibility $Hash
    } $Text @{ assemblies = $assemblyHashes } ('a' * 64)
}

function New-SourceBindingsFixture {
    return [ordered]@{
        protocol = 1; artifact_kind = 'cli_connection_test'; default_endpoint = 'https://illya.test'
        endpoint_mode = 'https_origin_per_session'; configurable_endpoint = $true
        bridge_sha256 = ('a' * 64); bridge_verified = $true; official_fallback = $false
        shared_existing_library = $true; private_auth_filename = 'auth.illyaverse.json'; shared_game_preferences = $true
        supported_assemblies = @{'osu.Game.dll' = ('1' * 64); 'osu.Framework.dll' = ('2' * 64); 'osu!.dll' = ('3' * 64)}
    }
}

function Test-CompatibilityJson([string]$Text) {
    & (Get-Module launcher-package) {
        param($Json)
        $hash = 'a' * 64
        $artifact = [ordered]@{
            protocol = 1; artifact = 'osu.Game.Rulesets.Transfiguration.dll'; sha256 = $hash
            default_endpoint = 'https://illya.test'; endpoint_mode = 'https_origin_per_session'; configurable_endpoint = $true
            private_auth_filename = 'auth.illyaverse.json'; shared_game_preferences = $true; fixture = $false
        }
        $compatibility = Read-PackageJson $Json 'synthetic compatibility'
        Assert-BridgePackageMetadata $artifact $compatibility $hash
    } $Text
}

try {
    # In-memory JSON exercises the actual private parser/validator. These hashes
    # are fixtures; no package, executable, player settings or network is read.
    $bindingsText = New-SourceBindingsFixture | ConvertTo-Json -Depth 10
    Test-SourceBindingsJson $bindingsText
    $checks++
    Write-Output 'PASS auth-only and configurable-endpoint source bindings JSON roundtrip'
    foreach ($case in @('prefs-false', 'prefs-string', 'prefs-null', 'auth-official-file', 'auth-obsolete-file', 'auth-array', 'missing-auth', 'missing-prefs', 'obsolete-replaces-auth', 'obsolete-added', 'unknown-field', 'wrong-case-auth', 'fixed-server-shape', 'endpoint-mode', 'configurable-false', 'configurable-string')) {
        $value = New-SourceBindingsFixture
        switch ($case) {
            'prefs-false' { $value.shared_game_preferences = $false }
            'prefs-string' { $value.shared_game_preferences = 'true' }
            'prefs-null' { $value.shared_game_preferences = $null }
            'auth-official-file' { $value.private_auth_filename = 'game.ini' }
            'auth-obsolete-file' { $value.private_auth_filename = 'game.illyaverse.ini' }
            'auth-array' { $value.private_auth_filename = @('auth.illyaverse.json') }
            'missing-auth' { $value.Remove('private_auth_filename') }
            'missing-prefs' { $value.Remove('shared_game_preferences') }
            'obsolete-replaces-auth' { $value.Remove('private_auth_filename'); $value.private_config_filename = 'game.illyaverse.ini' }
            'obsolete-added' { $value.private_config_filename = 'game.illyaverse.ini' }
            'unknown-field' { $value.unknown_policy = $true }
            'wrong-case-auth' { $value.Remove('private_auth_filename'); $value.PRIVATE_AUTH_FILENAME = 'auth.illyaverse.json' }
            'fixed-server-shape' { $value.Remove('default_endpoint'); $value.server = 'https://illya.test' }
            'endpoint-mode' { $value.endpoint_mode = 'fixed' }
            'configurable-false' { $value.configurable_endpoint = $false }
            'configurable-string' { $value.configurable_endpoint = 'true' }
        }
        $text = $value | ConvertTo-Json -Depth 10
        Expect-Refusal "source bindings reject $case" { Test-SourceBindingsJson $text }
    }
    $duplicate = $bindingsText.Replace('"shared_game_preferences": true', '"shared_game_preferences": true, "shared_game_preferences": true')
    Assert-Fixture ($duplicate -cne $bindingsText) 'Duplicate source binding fixture was not created.'
    Expect-Refusal 'source bindings reject duplicate auth policy field' { Test-SourceBindingsJson $duplicate } '*Invalid package JSON*'

    # Exercise metadata validation directly so a changed source-file hash cannot
    # mask a malformed verification value. All artifact bytes/hashes are fake.
    $compatibility = Get-Content -LiteralPath (Join-Path $repo 'tools/transfiguration/compatibility.json') -Raw
    $legacy = $compatibility | ConvertFrom-Json -AsHashtable
    $legacy.verification.Remove('explicit_ui_current_session_revocation')
    Assert-Fixture ($legacy.verification.Count -eq 15 -and $legacy.assemblies.Count -eq 5) 'Legacy compatibility fixture changed its verification or assembly boundary.'
    $legacyText = $legacy | ConvertTo-Json -Depth 20
    Test-CompatibilityJson $legacyText
    $checks++
    Write-Output 'PASS legacy 15-field compatibility verification with original five assembly bindings'
    $current = $legacyText | ConvertFrom-Json -AsHashtable
    $current.verification.explicit_ui_current_session_revocation = 'pending_human_test'
    Assert-Fixture ($current.verification.Count -eq 16) 'Current compatibility fixture did not add exactly one field.'
    $currentText = $current | ConvertTo-Json -Depth 20
    Test-CompatibilityJson $currentText
    $checks++
    Write-Output 'PASS current 16-field compatibility verification keeps explicit logout pending human test'
    foreach ($case in @('unknown-field', 'wrong-case-logout', 'logout-passed', 'logout-false', 'logout-true', 'logout-null', 'logout-array', 'verification-boolean', 'verification-array')) {
        $value = $currentText | ConvertFrom-Json -AsHashtable
        switch ($case) {
            'unknown-field' { $value.verification.unknown_policy = 'pending_human_test' }
            'wrong-case-logout' { $value.verification.Remove('explicit_ui_current_session_revocation'); $value.verification.EXPLICIT_UI_CURRENT_SESSION_REVOCATION = 'pending_human_test' }
            'logout-passed' { $value.verification.explicit_ui_current_session_revocation = 'passed' }
            'logout-false' { $value.verification.explicit_ui_current_session_revocation = $false }
            'logout-true' { $value.verification.explicit_ui_current_session_revocation = $true }
            'logout-null' { $value.verification.explicit_ui_current_session_revocation = $null }
            'logout-array' { $value.verification.explicit_ui_current_session_revocation = @('pending_human_test') }
            'verification-boolean' { $value.verification = $false }
            'verification-array' { $value.verification = @('pending_human_test') }
        }
        $text = $value | ConvertTo-Json -Depth 20
        Expect-Refusal "compatibility verification rejects $case" { Test-CompatibilityJson $text } '*compatibility.json.verification*'
    }
    [IO.Directory]::CreateDirectory($fixtureRoot) | Out-Null
    $owner = Join-Path $fixtureRoot '.owner.json'
    $ownerStream = [IO.File]::Open($owner, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $bytes = [Text.Encoding]::UTF8.GetBytes((@{ identity = $identity; root = $fixtureRoot } | ConvertTo-Json))
        $ownerStream.Write($bytes, 0, $bytes.Length)
    } finally { $ownerStream.Dispose() }
    $baseline = Join-Path $fixtureRoot 'source-baseline'
    # These bytes are deliberately not a runnable EXE or DLL. No process is started.
    Write-Fixture (Join-Path $baseline 'Kaleidoscope.exe') 'synthetic launcher bytes; not executable'
    Write-Fixture (Join-Path $baseline 'bridge/osu.Game.Rulesets.Transfiguration.dll') 'synthetic bridge bytes; not executable'
    Write-Fixture (Join-Path $baseline 'LICENSE') 'synthetic repository license text'
    Write-Fixture (Join-Path $baseline 'bridge/README.md') 'synthetic bridge documentation'
    Write-Fixture (Join-Path $baseline 'bridge/THIRD-PARTY-NOTICES.md') 'synthetic third-party notices'
    foreach ($name in @('DotNet.txt', 'Harmony.txt', 'Iced.txt', 'Mono.Cecil.txt', 'MonoMod.txt')) { Write-Fixture (Join-Path $baseline "bridge/licenses/$name") "synthetic license $name" }
    Write-Fixture (Join-Path $baseline 'ui/THIRD-PARTY-NOTICES.md') 'synthetic GUI font notices'
    foreach ($name in @('Nunito.txt', 'NotoSansTC.txt')) { Write-Fixture (Join-Path $baseline "ui/licenses/$name") "synthetic GUI font license $name" }
    Write-Fixture (Join-Path $baseline 'bridge/compatibility.json') $compatibility
    Write-Fixture (Join-Path $fixtureRoot 'expected-compatibility.json') $compatibility
    Write-FixtureJson (Join-Path $baseline 'bridge/artifact.json') @{ protocol = 1; artifact = 'osu.Game.Rulesets.Transfiguration.dll'; sha256 = (Get-FileHash -LiteralPath (Join-Path $baseline 'bridge/osu.Game.Rulesets.Transfiguration.dll') -Algorithm SHA256).Hash.ToLowerInvariant(); default_endpoint = 'https://illya.test'; endpoint_mode = 'https_origin_per_session'; configurable_endpoint = $true; private_auth_filename = 'auth.illyaverse.json'; shared_game_preferences = $true; fixture = $false }

    $script:valid = New-FixturePackage $baseline 'valid'
    $manifest = Test-KaleidoscopePackage -Directory $valid.Directory -AllowSyntheticFixtures
    $null = Test-KaleidoscopePackageArchive -ArchivePath $valid.Archive -Directory $valid.Directory -AllowSyntheticFixtures
    $directoryCli = & (Join-Path $PSScriptRoot '../scripts/verify-launcher-package.ps1') -Directory $valid.Directory -AllowSyntheticFixtures
    $archiveCli = & (Join-Path $PSScriptRoot '../scripts/verify-launcher-package.ps1') -ArchivePath $valid.Archive -AllowSyntheticFixtures
    Assert-Fixture ($directoryCli -like 'PASS complete package integrity*' -and $archiveCli -like 'PASS complete package integrity*') 'Package verifier CLI did not pass both parameter sets.'
    Assert-Fixture ($manifest.verification_kind -ceq 'synthetic_package_integrity' -and $manifest.embedded_interface_verified -eq $false -and $manifest.native_gui_verified -eq $false) 'Synthetic package claimed native evidence.'
    Assert-Fixture ($manifest.build_profile -ceq 'synthetic') 'Synthetic package claimed a compiled build profile.'
    $checks++
    Write-Output 'PASS complete synthetic directory and matching ZIP; all required files hashed'
    Expect-Refusal 'synthetic package is refused by production verifier' { Test-KaleidoscopePackage -Directory $valid.Directory } '*Synthetic*'

    # Cache and package workspaces are siblings within this invocation's owned
    # fixture root. The cache is external to the package workspace; no EXE runs.
    $packageWorkspace = Join-Path $fixtureRoot 'shared-target-package-workspace'
    $externalCache = Join-Path $fixtureRoot 'shared-target-cargo-cache'
    [IO.Directory]::CreateDirectory($packageWorkspace) | Out-Null
    $ownedSource = Join-Path $packageWorkspace 'source'
    Copy-Item -LiteralPath $baseline -Destination $ownedSource -Recurse
    $externalLauncher = Join-Path $externalCache 'release/kaleidoscope-desktop.exe'
    Write-Fixture $externalLauncher 'synthetic known Cargo output; never executable'
    $previousFixtureTarget = [Environment]::GetEnvironmentVariable('CARGO_TARGET_DIR', 'Process')
    try {
        [Environment]::SetEnvironmentVariable('CARGO_TARGET_DIR', $null, 'Process')
        Assert-Fixture ((Resolve-KaleidoscopeCargoTarget -WorkspaceRoot $packageWorkspace) -ceq (Join-Path $packageWorkspace 'target')) 'Default Cargo target changed.'
        $checks++
        Write-Output 'PASS Cargo target defaults to the package workspace target'
        [Environment]::SetEnvironmentVariable('CARGO_TARGET_DIR', $externalCache, 'Process')
        Assert-Fixture ((Resolve-KaleidoscopeCargoTarget -WorkspaceRoot $packageWorkspace) -ceq $externalCache) 'Shared CARGO_TARGET_DIR was ignored.'
        $checks++
        Write-Output 'PASS explicit shared CARGO_TARGET_DIR is resolved'
        Assert-Fixture ((Resolve-KaleidoscopeCargoTarget -WorkspaceRoot $packageWorkspace -TargetDirectory 'selected-target') -ceq (Join-Path $packageWorkspace 'selected-target')) 'Named relative Cargo target did not override the environment.'
        $checks++
        Write-Output 'PASS named Cargo target overrides the environment and resolves against the source workspace'
    } finally { [Environment]::SetEnvironmentVariable('CARGO_TARGET_DIR', $previousFixtureTarget, 'Process') }
    Expect-Refusal 'Cargo target cannot be a file' { Resolve-KaleidoscopeCargoTarget -WorkspaceRoot $packageWorkspace -TargetDirectory $externalLauncher } '*must be a directory*'
    Expect-Refusal 'Cargo target cannot be a filesystem root' { Resolve-KaleidoscopeCargoTarget -WorkspaceRoot $packageWorkspace -TargetDirectory ([IO.Path]::GetPathRoot($packageWorkspace)) } '*below a filesystem root*'
    $packageArguments = @{
        WorkspaceRoot = $packageWorkspace; LauncherPath = $externalLauncher; BridgeDirectory = (Join-Path $ownedSource 'bridge')
        LicensePath = (Join-Path $ownedSource 'LICENSE'); ExpectedCompatibilityPath = (Join-Path $ownedSource 'bridge/compatibility.json')
        OutputDirectory = (Join-Path $packageWorkspace 'complete-package'); UiDirectory = (Join-Path $ownedSource 'ui')
        InterfaceEvidence = @{ kind = 'synthetic_package_integrity'; launcher_sha256 = (Get-FileHash -LiteralPath $externalLauncher -Algorithm SHA256).Hash; exit_code = $null }
    }
    Expect-Refusal 'package assembly still refuses a direct external cache launcher' { New-KaleidoscopePackage @packageArguments } '*inside the repository*'
    $staged = Copy-KaleidoscopeLauncherArtifact -WorkspaceRoot $packageWorkspace -TargetDirectory $externalCache -BuildProfile Release
    $provenance = Get-Content -LiteralPath $staged.ProvenancePath -Raw | ConvertFrom-Json -AsHashtable
    Assert-Fixture ($provenance.kind -ceq 'launcher_output_copy' -and $provenance.source_launcher -ceq $externalLauncher -and $provenance.staged_launcher -ceq $staged.LauncherPath -and $provenance.source_sha256 -ceq $staged.Sha256 -and $provenance.staged_sha256 -ceq $staged.Sha256 -and $provenance.cargo_target_directory -ceq $externalCache) 'Shared target staging lost its exact source/hash provenance.'
    Assert-Fixture ((Get-FileHash -LiteralPath $externalLauncher -Algorithm SHA256).Hash -ieq (Get-FileHash -LiteralPath $staged.LauncherPath -Algorithm SHA256).Hash) 'Shared target staging changed the synthetic source.'
    Assert-Fixture (@(Get-ChildItem -LiteralPath ([IO.Path]::GetDirectoryName($staged.LauncherPath)) -File).Count -eq 2) 'Shared target staging copied unexpected cache files.'
    $checks++
    Write-Output 'PASS only the exact known launcher output is staged inside the package workspace with matching hash provenance'
    $packageArguments.LauncherPath = $staged.LauncherPath
    $sharedPackage = New-KaleidoscopePackage @packageArguments
    $null = Test-KaleidoscopePackage -Directory $sharedPackage.Directory -AllowSyntheticFixtures
    $null = Test-KaleidoscopePackageArchive -ArchivePath $sharedPackage.Archive -Directory $sharedPackage.Directory -AllowSyntheticFixtures
    $checks++
    Write-Output 'PASS staged external cache fixture completes the same directory and ZIP integrity gates'
    Expect-Refusal 'only the known launcher filename can be staged' { Copy-KaleidoscopeLauncherArtifact -WorkspaceRoot $packageWorkspace -TargetDirectory $externalCache -BuildProfile Debug }

    foreach ($required in @('artifact.json', 'THIRD-PARTY-NOTICES.md', 'README.md', 'licenses/Harmony.txt')) {
        $name = $required.Replace('/', '-').Replace('.', '-')
        $source = New-FixtureSource "missing-$name"
        Remove-Item -LiteralPath (Join-Path $source "bridge/$required")
        Expect-Refusal "missing required source $required" { New-FixturePackage $source "missing-$name" }
        Assert-Fixture (!(Test-Path -LiteralPath (Join-Path $fixtureRoot "output-missing-$name"))) 'Failed source gate created a final delivery.'
    }
    foreach ($required in @('THIRD-PARTY-NOTICES.md', 'licenses/Nunito.txt', 'licenses/NotoSansTC.txt')) {
        $name = $required.Replace('/', '-').Replace('.', '-')
        $source = New-FixtureSource "missing-gui-$name"
        Remove-Item -LiteralPath (Join-Path $source "ui/$required")
        Expect-Refusal "missing required GUI font source $required" { New-FixturePackage $source "missing-gui-$name" }
        Assert-Fixture (!(Test-Path -LiteralPath (Join-Path $fixtureRoot "output-missing-gui-$name"))) 'Missing GUI font source created a final delivery.'
    }
    foreach ($case in @('hash', 'fixture', 'endpoint', 'endpoint-mode', 'configurable-type', 'obsolete-fixed-endpoint', 'protocol-type')) {
        $source = New-FixtureSource "artifact-$case"
        $path = Join-Path $source 'bridge/artifact.json'
        $artifact = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json -AsHashtable
        switch ($case) {
            'hash' { $artifact.sha256 = ('0' * 64) }
            'fixture' { $artifact.fixture = $true }
            'endpoint' { $artifact.default_endpoint = 'https://osu.ppy.sh' }
            'endpoint-mode' { $artifact.endpoint_mode = 'fixed' }
            'configurable-type' { $artifact.configurable_endpoint = 'true' }
            'obsolete-fixed-endpoint' { $artifact.endpoint = 'https://illya.test' }
            'protocol-type' { $artifact.protocol = '1' }
        }
        Write-FixtureJson $path $artifact
        Expect-Refusal "artifact rejects $case" { New-FixturePackage $source "artifact-$case" }
    }
    foreach ($case in @('endpoint', 'assembly-hash', 'missing-assembly', 'runtime', 'contracts', 'missing-account-isolation-contract', 'missing-save-partition-contract', 'obsolete-whole-config-contract', 'missing-api-constructor-contract', 'missing-account-isolation-proof', 'missing-shared-preferences-proof', 'shared-preferences-overclaim', 'missing-endpoint-auth-proof', 'obsolete-whole-config-proof', 'account-proof-overclaim', 'worker-proof-overclaim', 'proof-state', 'fixture-type', 'source-mismatch')) {
        $source = New-FixtureSource "compatibility-$case"
        $path = Join-Path $source 'bridge/compatibility.json'
        $value = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json -AsHashtable
        switch ($case) {
            'endpoint' { $value.default_endpoint = 'https://illya.test:444' }
            'assembly-hash' { $value.assemblies['osu.Game.dll'].sha256 = 'invalid' }
            'missing-assembly' { $value.assemblies.Remove('System.Net.Http.dll') }
            'runtime' { $value.runtime = 'unknown' }
            'contracts' { $value.contracts = @('OsuGameBase.CreateEndpoints') }
            'missing-account-isolation-contract' { $value.contracts = @($value.contracts | Where-Object { $_ -cne 'IniConfigManager<OsuSetting>.PerformLoad:auth-only stream overlay' }) }
            'missing-save-partition-contract' { $value.contracts = @($value.contracts | Where-Object { $_ -cne 'IniConfigManager<OsuSetting>.PerformSave:shared preferences and private auth partition' }) }
            'obsolete-whole-config-contract' { $value.contracts += 'IniConfigManager<OsuSetting>.Filename(exact OsuConfigManager only)' }
            'missing-api-constructor-contract' { $value.contracts = @($value.contracts | Where-Object { $_ -cne 'APIAccess.ctor(OsuGameBase,OsuConfigManager,EndpointConfiguration,string)' }) }
            'missing-account-isolation-proof' { $value.verification.Remove('auth_only_before_api_constructor') }
            'missing-shared-preferences-proof' { $value.verification.Remove('shared_game_preferences') }
            'shared-preferences-overclaim' { $value.verification.shared_game_preferences = 'passed_native_login' }
            'missing-endpoint-auth-proof' { $value.verification.Remove('endpoint_bound_private_auth') }
            'obsolete-whole-config-proof' { $value.verification.private_configuration_before_api_constructor = 'passed_headless_real_config_constructor' }
            'account-proof-overclaim' { $value.verification.private_credential_save_logout_remember_flags = 'passed_native_login' }
            'worker-proof-overclaim' { $value.verification.real_api_worker_refresh_and_login = 'passed' }
            'proof-state' { $value.verification.native_gui_and_login = 'passed' }
            'fixture-type' { $value.fixture = 'false' }
            'source-mismatch' { $value.runtime = '10.0.13' }
        }
        Write-FixtureJson $path $value
        Expect-Refusal "compatibility rejects $case" { New-FixturePackage $source "compatibility-$case" }
    }
    $source = New-FixtureSource 'truncated-json'
    Write-Fixture (Join-Path $source 'bridge/artifact.json') '{"protocol":1,'
    Expect-Refusal 'truncated source JSON' { New-FixturePackage $source 'truncated-json' } '*Invalid package JSON*'
    $source = New-FixtureSource 'duplicate-json-field'
    $path = Join-Path $source 'bridge/artifact.json'
    $text = [IO.File]::ReadAllText($path).Replace('"protocol": 1', '"protocol": 1, "protocol": 1')
    Write-Fixture $path $text
    Expect-Refusal 'duplicate JSON field' { New-FixturePackage $source 'duplicate-json-field' } '*Invalid package JSON*'
    $source = New-FixtureSource 'endpoint-array'
    $path = Join-Path $source 'bridge/artifact.json'
    $value = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json -AsHashtable
    $value.default_endpoint = @('https://illya.test')
    Write-FixtureJson $path $value
    Expect-Refusal 'endpoint array instead of literal string' { New-FixturePackage $source 'endpoint-array' } '*Invalid package value*'
    Expect-Refusal 'changed launcher after verification' { New-FixturePackage $baseline 'changed-launcher' @{ kind = 'synthetic_package_integrity'; launcher_sha256 = ('0' * 64); exit_code = $null } } '*Launcher changed*'
    Expect-Refusal 'failed interface command evidence' { New-FixturePackage $baseline 'failed-proof' @{ kind = 'embedded_interface_command'; launcher_sha256 = (Get-FileHash -LiteralPath (Join-Path $baseline 'Kaleidoscope.exe') -Algorithm SHA256).Hash; exit_code = 7 } } '*did not pass*'

    $existing = Join-Path $fixtureRoot 'output-existing-directory'
    Write-Fixture (Join-Path $existing 'keep.txt') 'old delivery remains'
    Expect-Refusal 'existing output directory' { New-FixturePackage $baseline 'existing-directory' } '*already exists*'
    Assert-Fixture ([IO.File]::ReadAllText((Join-Path $existing 'keep.txt')) -ceq 'old delivery remains') 'Existing output changed.'
    Write-Fixture (Join-Path $fixtureRoot 'output-existing-zip.zip') 'old ZIP remains'
    Expect-Refusal 'existing output archive' { New-FixturePackage $baseline 'existing-zip' } '*already exists*'
    Assert-Fixture ([IO.File]::ReadAllText((Join-Path $fixtureRoot 'output-existing-zip.zip')) -ceq 'old ZIP remains') 'Existing archive changed.'
    Expect-Refusal 'output outside workspace' { Assert-PackageWorkspacePath (Join-Path ([IO.Path]::GetDirectoryName($repo)) 'outside-package-test') $repo } '*inside the repository*'

    foreach ($case in @('missing-license', 'missing-font-license', 'missing-gui-notice', 'missing-completion', 'truncated-completion', 'changed-launcher', 'unexpected-file')) {
        $copy = Copy-FixturePackage $case
        switch ($case) {
            'missing-license' { Remove-Item -LiteralPath (Join-Path $copy 'licenses/DotNet.txt') }
            'missing-font-license' { Remove-Item -LiteralPath (Join-Path $copy 'licenses/Nunito.txt') }
            'missing-gui-notice' { Remove-Item -LiteralPath (Join-Path $copy 'Kaleidoscope-THIRD-PARTY-NOTICES.md') }
            'missing-completion' { Remove-Item -LiteralPath (Join-Path $copy 'package.json') }
            'truncated-completion' { Write-Fixture (Join-Path $copy 'package.json') '{"status":' }
            'changed-launcher' { Write-Fixture (Join-Path $copy 'Kaleidoscope.exe') 'changed payload bytes' }
            'unexpected-file' { Write-Fixture (Join-Path $copy 'osu.Game.dll') 'official assembly must never be packaged' }
        }
        Expect-Refusal "assembled package rejects $case" { Test-KaleidoscopePackage -Directory $copy -AllowSyntheticFixtures }
    }

    foreach ($profile in @('debug', 'release', $true, $null)) {
        $copy = Copy-FixturePackage ('synthetic-profile-' + [guid]::NewGuid().ToString('N'))
        $path = Join-Path $copy 'package.json'
        $value = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json -AsHashtable
        $value.build_profile = $profile
        Write-FixtureJson $path $value
        Expect-Refusal 'synthetic evidence cannot claim a compiled GUI build profile' { Test-KaleidoscopePackage -Directory $copy -AllowSyntheticFixtures }
    }

    foreach ($case in @('changed-content', 'duplicate-entry', 'unsafe-entry', 'truncated-zip')) {
        $zipPath = Join-Path $fixtureRoot "tamper-$case.zip"
        Copy-Item -LiteralPath $valid.Archive -Destination $zipPath
        if ($case -ceq 'truncated-zip') {
            $file = [IO.File]::Open($zipPath, [IO.FileMode]::Open, [IO.FileAccess]::Write)
            try { $file.SetLength(32) } finally { $file.Dispose() }
        } else {
            $zip = [IO.Compression.ZipFile]::Open($zipPath, [IO.Compression.ZipArchiveMode]::Update)
            try {
                $entryName = 'LICENSE'
                if ($case -ceq 'changed-content') { $zip.GetEntry($entryName).Delete() }
                if ($case -ceq 'unsafe-entry') { $entryName = '../escape.txt' }
                $writer = [IO.StreamWriter]::new($zip.CreateEntry($entryName).Open())
                try { $writer.Write('tampered synthetic archive content') } finally { $writer.Dispose() }
            } finally { $zip.Dispose() }
        }
        Expect-Refusal "ZIP integrity rejects $case" { Test-KaleidoscopePackageArchive -ArchivePath $zipPath -AllowSyntheticFixtures }
    }
    $copy = Copy-FixturePackage 'zip-directory-mismatch'
    $path = Join-Path $copy 'package.json'
    $value = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json -AsHashtable
    $value.completed_at_utc = '2026-01-01T00:00:00.0000000Z'
    Write-FixtureJson $path $value
    Expect-Refusal 'ZIP completion metadata differs from directory' { Test-KaleidoscopePackageArchive -ArchivePath $valid.Archive -Directory $copy -AllowSyntheticFixtures } '*does not match assembled*'
    Write-Output "PASS $checks synthetic package integrity checks. No EXE/DLL executed; no embedded-interface or native gameplay proof claimed."
} finally {
    if (Test-Path -LiteralPath $fixtureRoot) {
        # Only this invocation's exact workspace-contained, identity-marked fixtures may be removed.
        $resolvedFixture = Assert-PackageWorkspacePath (Resolve-Path -LiteralPath $fixtureRoot).Path $repo
        $ownership = Get-Content -LiteralPath (Join-Path $resolvedFixture '.owner.json') -Raw | ConvertFrom-Json -AsHashtable
        if ([IO.Path]::GetDirectoryName($resolvedFixture) -ne $repo -or [IO.Path]::GetFileName($resolvedFixture) -cne ('data-kaleidoscope-package-fixtures-' + $identity) -or $ownership.identity -cne $identity -or $ownership.root -cne $fixtureRoot) { throw 'Refusing fixture cleanup: ownership identity differs.' }
        if (@(Get-ChildItem -LiteralPath $resolvedFixture -Recurse -Force | Where-Object { $_.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint) }).Count -ne 0) { throw 'Refusing fixture cleanup: a reparse point is present.' }
        Remove-Item -LiteralPath $resolvedFixture -Recurse -Force
    }
}
