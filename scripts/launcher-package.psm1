Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:bridgeName = 'osu.Game.Rulesets.Transfiguration.dll'
$script:licenseNames = @('DotNet.txt', 'Harmony.txt', 'Iced.txt', 'Mono.Cecil.txt', 'MonoMod.txt')
$script:payloadNames = @('Kaleidoscope.exe', $script:bridgeName, 'artifact.json', 'compatibility.json', 'THIRD-PARTY-NOTICES.md', 'LICENSE', 'README.txt', 'Transfiguration-README.md') + @($script:licenseNames | ForEach-Object { "licenses/$_" })
$script:allNames = $script:payloadNames + @('package.json')
$script:guiPayloadNames = $script:payloadNames + @('Kaleidoscope-THIRD-PARTY-NOTICES.md', 'licenses/Nunito.txt', 'licenses/NotoSansTC.txt')
$script:guiAllNames = $script:guiPayloadNames + @('package.json')

function Assert-PackageWorkspacePath([string]$Path, [string]$WorkspaceRoot) {
    $full = [IO.Path]::GetFullPath($Path)
    $root = [IO.Path]::GetFullPath($WorkspaceRoot).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
    if (!$full.StartsWith($root + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Package target must be inside the repository workspace.' }
    $cursor = $full
    while ($cursor) {
        if ((Test-Path -LiteralPath $cursor) -and (Get-Item -LiteralPath $cursor -Force).Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { throw "Package path contains a reparse point: $cursor" }
        if ($cursor.Equals($root, [StringComparison]::OrdinalIgnoreCase)) { break }
        $cursor = [IO.Path]::GetDirectoryName($cursor)
    }
    return $full
}

function Assert-LauncherBuildPath([string]$Path) {
    $full = [IO.Path]::GetFullPath($Path)
    $cursor = $full
    while ($cursor) {
        if ((Test-Path -LiteralPath $cursor) -and (Get-Item -LiteralPath $cursor -Force).Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { throw "Launcher build path contains a reparse point: $cursor" }
        $cursor = [IO.Path]::GetDirectoryName($cursor)
    }
    return $full
}

function Resolve-KaleidoscopeCargoTarget {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$WorkspaceRoot,
        [string]$TargetDirectory
    )
    if ([string]::IsNullOrWhiteSpace($TargetDirectory)) { $TargetDirectory = [Environment]::GetEnvironmentVariable('CARGO_TARGET_DIR', 'Process') }
    if ([string]::IsNullOrWhiteSpace($TargetDirectory)) { $TargetDirectory = Join-Path $WorkspaceRoot 'target' }
    $full = Assert-LauncherBuildPath ([IO.Path]::GetFullPath($TargetDirectory, [IO.Path]::GetFullPath($WorkspaceRoot)))
    if ((Test-Path -LiteralPath $full) -and !(Test-Path -LiteralPath $full -PathType Container)) { throw 'Cargo target must be a directory.' }
    if ($full.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) -eq [IO.Path]::GetPathRoot($full).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)) { throw 'Cargo target must be below a filesystem root.' }
    return $full
}

function Copy-KaleidoscopeLauncherArtifact {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$WorkspaceRoot,
        [Parameter(Mandatory)][string]$TargetDirectory,
        [ValidateSet('Release', 'Debug')][string]$BuildProfile = 'Release'
    )
    $target = Resolve-KaleidoscopeCargoTarget -WorkspaceRoot $WorkspaceRoot -TargetDirectory $TargetDirectory
    # Only Cargo's exact known launcher output can cross this read boundary.
    $source = Assert-LauncherBuildPath (Join-Path $target ($BuildProfile.ToLowerInvariant() + '/kaleidoscope-desktop.exe'))
    $item = Get-Item -LiteralPath $source -Force
    if ($item.PSIsContainer -or $item.Length -eq 0) { throw 'Missing or invalid compiled launcher output.' }
    $sourceHash = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant()
    $identity = [guid]::NewGuid().ToString('N')
    $stage = Assert-PackageWorkspacePath (Join-Path $WorkspaceRoot ('.artifacts/launcher-builds/' + $BuildProfile.ToLowerInvariant() + '-' + [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss') + '-' + $identity)) $WorkspaceRoot
    if (Test-Path -LiteralPath $stage) { throw 'Launcher artifact staging directory already exists.' }
    [IO.Directory]::CreateDirectory($stage) | Out-Null
    $launcher = Assert-PackageWorkspacePath (Join-Path $stage 'kaleidoscope-desktop.exe') $WorkspaceRoot
    Copy-Item -LiteralPath $source -Destination $launcher
    $stagedHash = (Get-FileHash -LiteralPath $launcher -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($sourceHash -cne $stagedHash -or $sourceHash -cne (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant()) { throw 'Compiled launcher changed during artifact staging.' }
    $provenance = [ordered]@{
        version = 1; identity = $identity; kind = 'launcher_output_copy'; build_profile = $BuildProfile.ToLowerInvariant()
        cargo_target_directory = $target; source_launcher = $source; staged_launcher = $launcher
        source_sha256 = $sourceHash; staged_sha256 = $stagedHash; staged_at_utc = [DateTime]::UtcNow.ToString('o')
    }
    $provenancePath = Assert-PackageWorkspacePath (Join-Path $stage 'build-provenance.json') $WorkspaceRoot
    [IO.File]::WriteAllText($provenancePath, ($provenance | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
    return [pscustomobject]@{ LauncherPath = $launcher; Sha256 = $stagedHash; ProvenancePath = $provenancePath }
}

function Read-PackageJson([string]$Text, [string]$Name) {
    $document = $null
    try {
        $document = [Text.Json.JsonDocument]::Parse($Text)
        Assert-PackageJsonProperties $document.RootElement
    } catch { throw "Invalid package JSON: $Name" } finally { if ($document) { $document.Dispose() } }
    $options = @{ InputObject = $Text; AsHashtable = $true; ErrorAction = 'Stop' }
    if ((Get-Command ConvertFrom-Json).Parameters.ContainsKey('DateKind')) { $options.DateKind = 'String' }
    try { $value = ConvertFrom-Json @options } catch { throw "Invalid package JSON: $Name" }
    if ($value -isnot [System.Collections.IDictionary]) { throw "Package JSON must be an object: $Name" }
    return $value
}

function Assert-PackageJsonProperties([Text.Json.JsonElement]$Element) {
    if ($Element.ValueKind -eq [Text.Json.JsonValueKind]::Object) {
        $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($property in $Element.EnumerateObject()) {
            if (!$names.Add($property.Name)) { throw 'Duplicate package JSON field.' }
            Assert-PackageJsonProperties $property.Value
        }
    } elseif ($Element.ValueKind -eq [Text.Json.JsonValueKind]::Array) {
        foreach ($child in $Element.EnumerateArray()) { Assert-PackageJsonProperties $child }
    }
}

function Assert-PackageKeys($Value, [string[]]$Names, [string]$Name) {
    if ($Value -isnot [System.Collections.IDictionary] -or $Value.Count -ne $Names.Count) { throw "Invalid fields in $Name" }
    foreach ($key in $Names) { if (@($Value.Keys) -cnotcontains $key) { throw "Missing $Name field: $key" } }
}

function Assert-PackageLiteral($Value, [string]$Expected, [string]$Name) {
    if ($Value -isnot [string] -or $Value -cne $Expected) { throw "Invalid package value: $Name" }
}

function Assert-PackageBoolean($Value, [bool]$Expected, [string]$Name) {
    if ($Value -isnot [bool] -or $Value -ne $Expected) { throw "Invalid package boolean: $Name" }
}

function Assert-PackageHash($Value, [string]$Name) {
    if ($Value -isnot [string] -or $Value -cnotmatch '^[0-9a-fA-F]{64}$') { throw "Invalid SHA256: $Name" }
}

function Assert-PackageProtocol($Value, [string]$Name) {
    if (($Value -isnot [long] -and $Value -isnot [int]) -or $Value -ne 1) { throw "Unsupported package protocol: $Name" }
}

function Assert-PackageEndpointPolicy($Value, [string]$Name) {
    Assert-PackageLiteral $Value.default_endpoint 'https://illya.test' "$Name default_endpoint"
    Assert-PackageLiteral $Value.endpoint_mode 'https_origin_per_session' "$Name endpoint_mode"
    Assert-PackageBoolean $Value.configurable_endpoint $true "$Name configurable_endpoint"
}

function Assert-PackageAuthPolicy($Value, [string]$Name) {
    Assert-PackageLiteral $Value.private_auth_filename 'auth.illyaverse.json' "$Name private_auth_filename"
    Assert-PackageBoolean $Value.shared_game_preferences $true "$Name shared_game_preferences"
}

function Assert-BridgePackageMetadata($Artifact, $Compatibility, [string]$BridgeHash) {
    Assert-PackageKeys $Artifact @('protocol', 'artifact', 'sha256', 'default_endpoint', 'endpoint_mode', 'configurable_endpoint', 'private_auth_filename', 'shared_game_preferences', 'fixture') 'artifact.json'
    Assert-PackageProtocol $Artifact.protocol 'artifact.json'
    Assert-PackageBoolean $Artifact.fixture $false 'artifact.json.fixture'
    Assert-PackageHash $Artifact.sha256 'artifact.json.sha256'
    Assert-PackageLiteral $Artifact.artifact $script:bridgeName 'artifact.json.artifact'
    Assert-PackageEndpointPolicy $Artifact 'artifact.json'
    Assert-PackageAuthPolicy $Artifact 'artifact.json'
    if ($Artifact.sha256 -ine $BridgeHash) { throw 'Production bridge artifact metadata or hash does not match.' }
    Assert-PackageKeys $Compatibility @('protocol', 'artifact', 'default_endpoint', 'endpoint_mode', 'configurable_endpoint', 'private_auth_filename', 'shared_game_preferences', 'fixture', 'platform', 'runtime', 'ruleset_api', 'harmony', 'assemblies', 'contracts', 'verification') 'compatibility.json'
    Assert-PackageProtocol $Compatibility.protocol 'compatibility.json'
    Assert-PackageBoolean $Compatibility.fixture $false 'compatibility.json.fixture'
    Assert-PackageLiteral $Compatibility.artifact $script:bridgeName 'compatibility.json.artifact'
    Assert-PackageEndpointPolicy $Compatibility 'compatibility.json'
    Assert-PackageAuthPolicy $Compatibility 'compatibility.json'
    Assert-PackageLiteral $Compatibility.platform 'windows-x64' 'compatibility.json.platform'
    foreach ($key in @('runtime', 'ruleset_api', 'harmony')) {
        if ($Compatibility[$key] -isnot [string] -or $Compatibility[$key] -cnotmatch '^\d+\.\d+\.\d+$') { throw "Invalid compatibility version: $key" }
    }
    $assemblies = @('osu.Game.dll', 'osu.Framework.dll', 'osu!.dll', 'System.Net.Http.dll', 'System.Net.WebSockets.Client.dll')
    Assert-PackageKeys $Compatibility.assemblies $assemblies 'compatibility.json.assemblies'
    foreach ($key in $assemblies) {
        $assembly = $Compatibility.assemblies[$key]
        Assert-PackageKeys $assembly @('version', 'sha256') "compatibility assembly $key"
        if ($assembly.version -isnot [string] -or $assembly.version -cnotmatch '^\d+\.\d+\.\d+\.\d+$') { throw "Invalid assembly version: $key" }
        Assert-PackageHash $assembly.sha256 "compatibility assembly $key"
    }
    if ($Compatibility.contracts -isnot [array] -or $Compatibility.contracts.Count -eq 0) { throw 'Missing compatibility contracts.' }
    $contracts = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($contract in $Compatibility.contracts) {
        if ($contract -isnot [string] -or [string]::IsNullOrWhiteSpace($contract) -or !$contracts.Add($contract)) { throw 'Invalid or duplicate compatibility contract.' }
    }
    foreach ($obsolete in @('IniConfigManager<OsuSetting>.Filename(exact OsuConfigManager only)', 'OsuConfigManager.ctor(Storage):pre-load and post-load isolation')) {
        if ($contracts.Contains($obsolete)) { throw 'Obsolete whole-game configuration isolation contract.' }
    }
    foreach ($contract in @('OsuGameBase.CreateEndpoints', 'Host.GetSuitableDesktopHost', 'DesktopGameHost.GetDefaultGameStorage', 'IniConfigManager<OsuSetting>.PerformLoad:auth-only stream overlay', 'IniConfigManager<OsuSetting>.PerformSave:shared preferences and private auth partition', 'NativeStorage.GetStream:scoped game.ini auth filtering', 'Storage.CreateFileSafely:scoped native preference serialization', 'OsuConfigManager.ctor(Storage):shared preferences and private auth before API', 'APIAccess.ctor(OsuGameBase,OsuConfigManager,EndpointConfiguration,string)', 'RulesetStore.addRuleset', 'SocketsHttpHandler.SendAsync', 'ClientWebSocket.ConnectAsync(Uri,CancellationToken)')) {
        if (!$contracts.Contains($contract)) { throw "Missing mandatory compatibility contract: $contract" }
    }
    $verification = [ordered]@{
        native_metadata_and_endpoints = 'passed'; http_websocket_and_redirects = 'passed'; mandatory_bootstrap_before_headless_main = 'passed'; production_destination_policy = 'passed'; configurable_https_origin = 'passed_source_linked'
        rx_native_request_context = 'passed_headless'; rx_statistics_generation_cache = 'passed_headless'
        shared_existing_library_storage = 'passed_headless_path_and_marker_fixture'
        auth_only_before_api_constructor = 'passed_headless_real_config_constructor'
        shared_game_preferences = 'passed_headless_native_load_save'
        official_auth_byte_preservation = 'passed_headless_native_load_save_logout'
        endpoint_bound_private_auth = 'passed_source_linked_and_headless_real_config'
        private_credential_save_logout_remember_flags = 'passed_headless_native_handlers_worker_intercepted'
        real_api_worker_refresh_and_login = 'pending_human_test'; native_gui_and_login = 'pending_human_test'
    }
    Assert-PackageKeys $Compatibility.verification @($verification.Keys) 'compatibility.json.verification'
    foreach ($key in $verification.Keys) { Assert-PackageLiteral $Compatibility.verification[$key] $verification[$key] "compatibility.json.verification.$key" }
}

function Get-PackageFileRecords([string]$Directory, [string[]]$AllowedNames = $script:guiAllNames) {
    if (!(Test-Path -LiteralPath $Directory -PathType Container)) { throw 'Assembled package directory is missing.' }
    $root = [IO.Path]::GetFullPath($Directory)
    if ((Get-Item -LiteralPath $root -Force).Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { throw 'Package directory is a reparse point.' }
    $records = [ordered]@{}
    foreach ($item in Get-ChildItem -LiteralPath $root -Recurse -Force) {
        if ($item.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { throw 'Package contains a reparse point.' }
        if ($item.PSIsContainer) {
            if ([IO.Path]::GetRelativePath($root, $item.FullName).Replace('\', '/') -cne 'licenses') { throw 'Package contains an unexpected directory.' }
            continue
        }
        $name = [IO.Path]::GetRelativePath($root, $item.FullName).Replace('\', '/')
        if ($AllowedNames -cnotcontains $name -or $item.Length -eq 0) { throw "Unexpected or empty package file: $name" }
        $records[$name] = @{ sha256 = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant(); text = $null }
        if ($name.EndsWith('.json', [StringComparison]::Ordinal)) {
            if ($item.Length -gt 1MB) { throw 'Package JSON exceeds the integrity verifier size limit.' }
            $records[$name].text = [IO.File]::ReadAllText($item.FullName)
        }
    }
    return $records
}

function Assert-PackageRecords($Records, [switch]$AllowSyntheticFixtures) {
    Assert-PackageKeys $Records $script:guiAllNames 'assembled package files'
    $artifact = Read-PackageJson $Records['artifact.json'].text 'artifact.json'
    $compatibility = Read-PackageJson $Records['compatibility.json'].text 'compatibility.json'
    Assert-BridgePackageMetadata $artifact $compatibility $Records[$script:bridgeName].sha256
    $package = Read-PackageJson $Records['package.json'].text 'package.json'
    Assert-PackageKeys $package @('version', 'status', 'default_endpoint', 'endpoint_mode', 'configurable_endpoint', 'fixture', 'build_profile', 'bridge_sha256', 'embedded_interface_verified', 'native_gui_verified', 'verification_kind', 'completed_at_utc', 'content_sha256') 'package.json'
    Assert-PackageProtocol $package.version 'package.json'
    Assert-PackageBoolean $package.fixture $false 'package.json.fixture'
    Assert-PackageBoolean $package.native_gui_verified $false 'package.json.native_gui_verified'
    Assert-PackageLiteral $package.status 'complete' 'package.json.status'
    Assert-PackageEndpointPolicy $package 'package.json'
    Assert-PackageHash $package.bridge_sha256 'package.json.bridge_sha256'
    if ($package.bridge_sha256 -ine $Records[$script:bridgeName].sha256) { throw 'Package bridge hash does not match.' }
    if ($package.verification_kind -isnot [string]) { throw 'Unsupported package verification evidence.' }
    if ($package.verification_kind -ceq 'synthetic_package_integrity') {
        if (!$AllowSyntheticFixtures) { throw 'Synthetic package integrity evidence is not production interface evidence.' }
        Assert-PackageBoolean $package.embedded_interface_verified $false 'synthetic embedded_interface_verified'
        Assert-PackageLiteral $package.build_profile 'synthetic' 'synthetic build_profile'
    } elseif ($package.verification_kind -ceq 'embedded_interface_command') {
        Assert-PackageBoolean $package.embedded_interface_verified $true 'embedded_interface_verified'
        if ($package.build_profile -isnot [string] -or @('debug', 'release') -cnotcontains $package.build_profile) { throw 'Unsupported GUI build profile.' }
    } else { throw 'Unsupported package verification evidence.' }
    # Earlier PowerShell versions deserialize ISO timestamps as DateTime automatically.
    if ($package.completed_at_utc -is [DateTime] -and $package.completed_at_utc.Kind -eq [DateTimeKind]::Utc) { $package.completed_at_utc = $package.completed_at_utc.ToString('o') }
    if ($package.completed_at_utc -isnot [string] -or $package.completed_at_utc -cnotmatch '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{7}Z$') { throw 'Invalid package completion timestamp.' }
    $parsedTimestamp = [DateTime]::MinValue
    if (![DateTime]::TryParse($package.completed_at_utc, [ref]$parsedTimestamp)) { throw 'Invalid package completion timestamp.' }
    Assert-PackageKeys $package.content_sha256 $script:guiPayloadNames 'package.json.content_sha256'
    foreach ($name in $script:guiPayloadNames) {
        Assert-PackageHash $package.content_sha256[$name] "package content $name"
        if ($package.content_sha256[$name] -ine $Records[$name].sha256) { throw "Package content hash does not match: $name" }
    }
    return $package
}

function Test-KaleidoscopePackage {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Directory, [switch]$AllowSyntheticFixtures)
    return Assert-PackageRecords (Get-PackageFileRecords $Directory) -AllowSyntheticFixtures:$AllowSyntheticFixtures
}

function Get-PackageArchiveRecords([string]$ArchivePath, [string[]]$AllowedNames = $script:guiAllNames) {
    $records = [ordered]@{}
    $zip = [IO.Compression.ZipFile]::OpenRead([IO.Path]::GetFullPath($ArchivePath))
    try {
        $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName
            if ($AllowedNames -cnotcontains $name -or !$names.Add($name) -or $entry.Length -eq 0) { throw "Unexpected, duplicate or empty ZIP entry: $name" }
            if ($entry.Length -gt 256MB) { throw 'Package ZIP entry exceeds the integrity verifier size limit.' }
            $stream = $entry.Open()
            try {
                $hash = [Security.Cryptography.SHA256]::Create()
                try { $digest = [Convert]::ToHexString($hash.ComputeHash($stream)).ToLowerInvariant() } finally { $hash.Dispose() }
            } finally { $stream.Dispose() }
            $records[$name] = @{ sha256 = $digest; text = $null }
            if ($name.EndsWith('.json', [StringComparison]::Ordinal)) {
                if ($entry.Length -gt 1MB) { throw 'Package JSON exceeds the integrity verifier size limit.' }
                $reader = [IO.StreamReader]::new($entry.Open())
                try { $records[$name].text = $reader.ReadToEnd() } finally { $reader.Dispose() }
            }
        }
    } finally { $zip.Dispose() }
    return $records
}

function Test-KaleidoscopePackageArchive {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$ArchivePath, [string]$Directory, [switch]$AllowSyntheticFixtures)
    $records = Get-PackageArchiveRecords $ArchivePath
    $package = Assert-PackageRecords $records -AllowSyntheticFixtures:$AllowSyntheticFixtures
    if ($Directory) {
        $files = Get-PackageFileRecords $Directory
        $null = Assert-PackageRecords $files -AllowSyntheticFixtures:$AllowSyntheticFixtures
        foreach ($name in $script:guiAllNames) { if ($records[$name].sha256 -ine $files[$name].sha256) { throw "ZIP does not match assembled package: $name" } }
    }
    return $package
}

function Assert-CliPackageBindings($Bindings, $Compatibility, [string]$BridgeHash) {
    Assert-PackageKeys $Bindings @('protocol', 'artifact_kind', 'default_endpoint', 'endpoint_mode', 'configurable_endpoint', 'bridge_sha256', 'bridge_verified', 'official_fallback', 'shared_existing_library', 'private_auth_filename', 'shared_game_preferences', 'supported_assemblies') 'CLI package bindings'
    Assert-PackageProtocol $Bindings.protocol 'CLI package bindings'
    Assert-PackageLiteral $Bindings.artifact_kind 'cli_connection_test' 'CLI artifact_kind'
    Assert-PackageEndpointPolicy $Bindings 'CLI'
    Assert-PackageHash $Bindings.bridge_sha256 'CLI bridge_sha256'
    if ($Bindings.bridge_sha256 -cne $BridgeHash) { throw 'Compiled CLI bridge hash does not match the production bridge.' }
    Assert-PackageBoolean $Bindings.bridge_verified $true 'CLI bridge_verified'
    Assert-PackageBoolean $Bindings.official_fallback $false 'CLI official_fallback'
    Assert-PackageBoolean $Bindings.shared_existing_library $true 'CLI shared_existing_library'
    Assert-PackageLiteral $Bindings.private_auth_filename 'auth.illyaverse.json' 'CLI private authentication file'
    Assert-PackageBoolean $Bindings.shared_game_preferences $true 'CLI shared game preferences'
    Assert-PackageKeys $Bindings.supported_assemblies @('osu.Game.dll', 'osu.Framework.dll', 'osu!.dll') 'CLI supported assemblies'
    foreach ($name in $Bindings.supported_assemblies.Keys) {
        Assert-PackageHash $Bindings.supported_assemblies[$name] "CLI assembly $name"
        if ($Bindings.supported_assemblies[$name] -cne $Compatibility.assemblies[$name].sha256.ToLowerInvariant()) { throw "Compiled CLI assembly binding does not match compatibility: $name" }
    }
}

function Assert-CliPackageRecords($Records) {
    Assert-PackageKeys $Records $script:allNames 'CLI assembled package files'
    $artifact = Read-PackageJson $Records['artifact.json'].text 'artifact.json'
    $compatibility = Read-PackageJson $Records['compatibility.json'].text 'compatibility.json'
    Assert-BridgePackageMetadata $artifact $compatibility $Records[$script:bridgeName].sha256
    $package = Read-PackageJson $Records['package.json'].text 'package.json'
    Assert-PackageKeys $package @('version', 'artifact_kind', 'status', 'default_endpoint', 'endpoint_mode', 'configurable_endpoint', 'fixture', 'build_profile', 'bridge_sha256', 'native_gui_verified', 'native_login_verified', 'verification_kind', 'package_bindings', 'completed_at_utc', 'content_sha256') 'CLI package.json'
    Assert-PackageProtocol $package.version 'CLI package.json'
    Assert-PackageLiteral $package.artifact_kind 'cli_connection_test' 'CLI package artifact_kind'
    Assert-PackageLiteral $package.status 'ready_for_manual_connection_test' 'CLI package status'
    Assert-PackageEndpointPolicy $package 'CLI package'
    Assert-PackageLiteral $package.build_profile 'debug' 'CLI package build_profile'
    Assert-PackageLiteral $package.verification_kind 'cli_package_bindings_command' 'CLI verification_kind'
    Assert-PackageBoolean $package.fixture $false 'CLI package fixture'
    Assert-PackageBoolean $package.native_gui_verified $false 'CLI native_gui_verified'
    Assert-PackageBoolean $package.native_login_verified $false 'CLI native_login_verified'
    Assert-PackageHash $package.bridge_sha256 'CLI package bridge_sha256'
    if ($package.bridge_sha256 -cne $Records[$script:bridgeName].sha256) { throw 'CLI package bridge hash does not match.' }
    Assert-CliPackageBindings $package.package_bindings $compatibility $Records[$script:bridgeName].sha256
    if ($package.completed_at_utc -is [DateTime] -and $package.completed_at_utc.Kind -eq [DateTimeKind]::Utc) { $package.completed_at_utc = $package.completed_at_utc.ToString('o') }
    $timestamp = [DateTime]::MinValue
    if ($package.completed_at_utc -isnot [string] -or $package.completed_at_utc -cnotmatch '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{7}Z$' -or ![DateTime]::TryParse($package.completed_at_utc, [ref]$timestamp)) { throw 'Invalid CLI package completion timestamp.' }
    Assert-PackageKeys $package.content_sha256 $script:payloadNames 'CLI content_sha256'
    foreach ($name in $script:payloadNames) {
        Assert-PackageHash $package.content_sha256[$name] "CLI content $name"
        if ($package.content_sha256[$name] -cne $Records[$name].sha256) { throw "CLI package content hash does not match: $name" }
    }
    return $package
}

function Test-KaleidoscopeCliTestPackage {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Directory)
    return Assert-CliPackageRecords (Get-PackageFileRecords $Directory -AllowedNames $script:allNames)
}

function Test-KaleidoscopeCliTestPackageArchive {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$ArchivePath, [string]$Directory)
    $records = Get-PackageArchiveRecords $ArchivePath -AllowedNames $script:allNames
    $package = Assert-CliPackageRecords $records
    if ($Directory) {
        $files = Get-PackageFileRecords $Directory -AllowedNames $script:allNames
        $null = Assert-CliPackageRecords $files
        foreach ($name in $script:allNames) { if ($records[$name].sha256 -cne $files[$name].sha256) { throw "CLI ZIP does not match assembled package: $name" } }
    }
    return $package
}

function New-KaleidoscopeCliTestPackage {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$WorkspaceRoot,
        [Parameter(Mandatory)][string]$LauncherPath,
        [Parameter(Mandatory)][string]$BridgeDirectory,
        [Parameter(Mandatory)][string]$LicensePath,
        [Parameter(Mandatory)][string]$ExpectedCompatibilityPath,
        [Parameter(Mandatory)][string]$OutputDirectory
    )
    $output = Assert-PackageWorkspacePath $OutputDirectory $WorkspaceRoot
    $archive = Assert-PackageWorkspacePath ($output + '.zip') $WorkspaceRoot
    if ((Test-Path -LiteralPath $output) -or (Test-Path -LiteralPath $archive)) { throw 'CLI output or ZIP already exists; choose a new output directory.' }
    # Assembly may execute the staged diagnostic, so all sources must be owned
    # workspace paths with no linked ancestor, before any copy or process call.
    $LauncherPath = Assert-PackageWorkspacePath $LauncherPath $WorkspaceRoot
    $BridgeDirectory = Assert-PackageWorkspacePath $BridgeDirectory $WorkspaceRoot
    $LicensePath = Assert-PackageWorkspacePath $LicensePath $WorkspaceRoot
    $ExpectedCompatibilityPath = Assert-PackageWorkspacePath $ExpectedCompatibilityPath $WorkspaceRoot
    $sources = [ordered]@{
        'Kaleidoscope.exe' = $LauncherPath; $script:bridgeName = (Join-Path $BridgeDirectory $script:bridgeName)
        'artifact.json' = (Join-Path $BridgeDirectory 'artifact.json'); 'compatibility.json' = (Join-Path $BridgeDirectory 'compatibility.json')
        'THIRD-PARTY-NOTICES.md' = (Join-Path $BridgeDirectory 'THIRD-PARTY-NOTICES.md'); 'LICENSE' = $LicensePath
        'Transfiguration-README.md' = (Join-Path $BridgeDirectory 'README.md')
    }
    foreach ($name in $script:licenseNames) { $sources["licenses/$name"] = Join-Path $BridgeDirectory "licenses/$name" }
    foreach ($name in $sources.Keys) {
        $null = Assert-PackageWorkspacePath $sources[$name] $WorkspaceRoot
        $source = Get-Item -LiteralPath $sources[$name] -Force
        if ($source.PSIsContainer -or $source.Length -eq 0 -or $source.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { throw "Missing or invalid CLI package source: $name" }
    }
    $bridgeHash = (Get-FileHash -LiteralPath $sources[$script:bridgeName] -Algorithm SHA256).Hash.ToLowerInvariant()
    $compatibility = Read-PackageJson ([IO.File]::ReadAllText($sources['compatibility.json'])) 'compatibility.json'
    Assert-BridgePackageMetadata (Read-PackageJson ([IO.File]::ReadAllText($sources['artifact.json'])) 'artifact.json') $compatibility $bridgeHash
    $expectedHash = (Get-FileHash -LiteralPath $ExpectedCompatibilityPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ((Get-FileHash -LiteralPath $sources['compatibility.json'] -Algorithm SHA256).Hash.ToLowerInvariant() -cne $expectedHash) { throw 'CLI compatibility differs from the tested repository manifest.' }
    $parent = [IO.Path]::GetDirectoryName($output)
    if (!(Test-Path -LiteralPath $parent -PathType Container)) { throw 'CLI package parent must already exist.' }
    $stage = Assert-PackageWorkspacePath (Join-Path $parent ('.kaleidoscope-cli-staging-' + [guid]::NewGuid().ToString('N'))) $WorkspaceRoot
    $stageZip = Assert-PackageWorkspacePath ($stage + '.zip') $WorkspaceRoot
    [IO.Directory]::CreateDirectory((Join-Path $stage 'licenses')) | Out-Null
    foreach ($name in $sources.Keys) { Copy-Item -LiteralPath $sources[$name] -Destination (Join-Path $stage $name) }
    # Runs only the package diagnostic, before user storage or process APIs.
    $bindingText = & (Join-Path $stage 'Kaleidoscope.exe') --inspect-package-bindings
    if ($LASTEXITCODE -ne 0) { throw 'CLI compiled package binding verification failed; staging is retained.' }
    $bindings = Read-PackageJson ($bindingText -join "`n") 'CLI diagnostic output'
    Assert-CliPackageBindings $bindings $compatibility $bridgeHash
    $readme = @'
Kaleidoscope — CLI-only Illyaverse connection test (debug build)

This is a command-line interim test artifact, not the desktop GUI package.
From this directory in PowerShell, launch your installed stock lazer with:

  .\Kaleidoscope.exe launcher start lazer "$env:LOCALAPPDATA\osulazer\current\osu!.exe"

For a custom installation, replace that final path with its osu!.exe path.
Use the selected Illyaverse deployment's website account. The default endpoint
is https://illya.test; launcher settings accept a validated HTTPS origin for the
next launch. The illya.test setup needs the existing local DNS/CA. TLS remains
required and this package never falls back to official osu!.
To view or save the next-launch endpoint:
  .\Kaleidoscope.exe launcher settings
  .\Kaleidoscope.exe launcher set-endpoint "https://your-illyaverse.example"
Stable requires HTTPS port 443; lazer also accepts custom HTTPS ports.
Only lazer 2026.1005.1 with the exact compatibility.json assembly hashes is
accepted. Unsupported updates refuse to start; do not manually install the DLL.
The original default/custom/portable beatmap library is reused. Gameplay, UI,
audio and input preferences remain shared through the original game.ini.
Only account/session state and remember-login flags use auth.illyaverse.json in
that same library, with separate profiles for each canonical endpoint. Official
account fields are filtered before private API use and preserved when saving
shared preferences. The old game.illyaverse.ini is not copied or migrated.
Keep the package directory in place while the game and its guardian are running.
Close lazer normally. The guardian then removes only its unchanged owned DLL.
For interrupted cleanup, run: .\Kaleidoscope.exe launcher recover
To inspect launcher state: .\Kaleidoscope.exe launcher status
To request normal game shutdown: .\Kaleidoscope.exe launcher stop
Do not run recovery while another lazer instance is using the same library.

The package diagnostic only reads this package. It can be run without a game:
  .\Kaleidoscope.exe --inspect-package-bindings
Package hash validation does not prove login, gameplay, score upload or native
GUI behavior. Those remain user-operated testing. No client is redistributed.
See compatibility.json, Transfiguration-README.md and THIRD-PARTY-NOTICES.md.
'@
    [IO.File]::WriteAllText((Join-Path $stage 'README.txt'), $readme, [Text.UTF8Encoding]::new($false))
    $hashes = [ordered]@{}
    foreach ($name in $script:payloadNames) { $hashes[$name] = (Get-FileHash -LiteralPath (Join-Path $stage $name) -Algorithm SHA256).Hash.ToLowerInvariant() }
    if ($hashes['Kaleidoscope.exe'] -cne (Get-FileHash -LiteralPath $LauncherPath -Algorithm SHA256).Hash.ToLowerInvariant() -or $hashes[$script:bridgeName] -cne $bridgeHash -or $hashes['compatibility.json'] -cne $expectedHash) { throw 'CLI source changed during assembly.' }
    $manifest = [ordered]@{
        version = 1; artifact_kind = 'cli_connection_test'; status = 'ready_for_manual_connection_test'
        default_endpoint = 'https://illya.test'; endpoint_mode = 'https_origin_per_session'; configurable_endpoint = $true; fixture = $false; build_profile = 'debug'; bridge_sha256 = $bridgeHash
        native_gui_verified = $false; native_login_verified = $false; verification_kind = 'cli_package_bindings_command'
        package_bindings = $bindings; completed_at_utc = [DateTime]::UtcNow.ToString('o'); content_sha256 = $hashes
    }
    [IO.File]::WriteAllText((Join-Path $stage 'package.json'), ($manifest | ConvertTo-Json -Depth 12), [Text.UTF8Encoding]::new($false))
    $null = Test-KaleidoscopeCliTestPackage -Directory $stage
    [IO.Compression.ZipFile]::CreateFromDirectory($stage, $stageZip, [IO.Compression.CompressionLevel]::Optimal, $false)
    $null = Test-KaleidoscopeCliTestPackageArchive -ArchivePath $stageZip -Directory $stage
    $null = Assert-PackageWorkspacePath $stage $WorkspaceRoot
    $null = Assert-PackageWorkspacePath $output $WorkspaceRoot
    [IO.Directory]::Move($stage, $output)
    $null = Assert-PackageWorkspacePath $stageZip $WorkspaceRoot
    $null = Assert-PackageWorkspacePath $archive $WorkspaceRoot
    [IO.File]::Move($stageZip, $archive)
    return [pscustomobject]@{ Directory = $output; Archive = $archive; ArtifactKind = 'cli_connection_test'; LauncherSha256 = $hashes['Kaleidoscope.exe']; BridgeSha256 = $bridgeHash }
}

function New-KaleidoscopePackage {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$WorkspaceRoot,
        [Parameter(Mandatory)][string]$LauncherPath,
        [Parameter(Mandatory)][string]$BridgeDirectory,
        [Parameter(Mandatory)][string]$LicensePath,
        [Parameter(Mandatory)][string]$ExpectedCompatibilityPath,
        [Parameter(Mandatory)][string]$OutputDirectory,
        [Parameter(Mandatory)][string]$UiDirectory,
        [Parameter(Mandatory)][hashtable]$InterfaceEvidence,
        [ValidateSet('Release', 'Debug')][string]$BuildProfile = 'Release'
    )
    $output = Assert-PackageWorkspacePath $OutputDirectory $WorkspaceRoot
    $archive = Assert-PackageWorkspacePath ($output + '.zip') $WorkspaceRoot
    if ((Test-Path -LiteralPath $output) -or (Test-Path -LiteralPath $archive)) { throw 'Package output or ZIP already exists; choose a new output directory.' }
    $sources = [ordered]@{
        'Kaleidoscope.exe' = $LauncherPath; $script:bridgeName = (Join-Path $BridgeDirectory $script:bridgeName)
        'artifact.json' = (Join-Path $BridgeDirectory 'artifact.json'); 'compatibility.json' = (Join-Path $BridgeDirectory 'compatibility.json')
        'THIRD-PARTY-NOTICES.md' = (Join-Path $BridgeDirectory 'THIRD-PARTY-NOTICES.md'); 'LICENSE' = $LicensePath
        'Transfiguration-README.md' = (Join-Path $BridgeDirectory 'README.md')
    }
    foreach ($name in $script:licenseNames) { $sources["licenses/$name"] = Join-Path $BridgeDirectory "licenses/$name" }
    $UiDirectory = Assert-PackageWorkspacePath $UiDirectory $WorkspaceRoot
    $sources['Kaleidoscope-THIRD-PARTY-NOTICES.md'] = Join-Path $UiDirectory 'THIRD-PARTY-NOTICES.md'
    foreach ($name in @('Nunito.txt', 'NotoSansTC.txt')) { $sources["licenses/$name"] = Join-Path $UiDirectory "licenses/$name" }
    foreach ($name in $sources.Keys) {
        $null = Assert-PackageWorkspacePath $sources[$name] $WorkspaceRoot
        $source = Get-Item -LiteralPath $sources[$name] -Force
        if ($source.PSIsContainer -or $source.Length -eq 0 -or $source.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { throw "Missing or invalid package source: $name" }
    }
    $bridgeHash = (Get-FileHash -LiteralPath $sources[$script:bridgeName] -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-BridgePackageMetadata (Read-PackageJson ([IO.File]::ReadAllText($sources['artifact.json'])) 'artifact.json') (Read-PackageJson ([IO.File]::ReadAllText($sources['compatibility.json'])) 'compatibility.json') $bridgeHash
    $expectedCompatibilityHash = (Get-FileHash -LiteralPath $ExpectedCompatibilityPath -Algorithm SHA256).Hash
    if ((Get-FileHash -LiteralPath $sources['compatibility.json'] -Algorithm SHA256).Hash -ine $expectedCompatibilityHash) { throw 'Bridge artifact compatibility differs from the tested repository manifest.' }
    Assert-PackageKeys $InterfaceEvidence @('kind', 'launcher_sha256', 'exit_code') 'interface evidence'
    if ($InterfaceEvidence.kind -isnot [string]) { throw 'Unsupported interface evidence kind.' }
    Assert-PackageHash $InterfaceEvidence.launcher_sha256 'interface evidence launcher_sha256'
    if ($InterfaceEvidence.launcher_sha256 -ine (Get-FileHash -LiteralPath $LauncherPath -Algorithm SHA256).Hash) { throw 'Launcher changed after interface verification.' }
    $synthetic = $InterfaceEvidence.kind -ceq 'synthetic_package_integrity'
    if (!$synthetic -and $InterfaceEvidence.kind -cne 'embedded_interface_command') { throw 'Unsupported interface evidence kind.' }
    if ($synthetic) {
        if ($null -ne $InterfaceEvidence.exit_code) { throw 'Synthetic integrity evidence must not claim a native command result.' }
    } elseif (($InterfaceEvidence.exit_code -isnot [int] -and $InterfaceEvidence.exit_code -isnot [long]) -or $InterfaceEvidence.exit_code -ne 0) { throw 'Embedded production interface verification did not pass.' }
    $parent = [IO.Path]::GetDirectoryName($output)
    if (!(Test-Path -LiteralPath $parent -PathType Container)) { throw 'Package parent directory must already exist.' }
    $stage = Assert-PackageWorkspacePath (Join-Path $parent ('.kaleidoscope-package-staging-' + [guid]::NewGuid().ToString('N'))) $WorkspaceRoot
    $stageZip = Assert-PackageWorkspacePath ($stage + '.zip') $WorkspaceRoot
    [IO.Directory]::CreateDirectory($stage) | Out-Null
    [IO.Directory]::CreateDirectory((Join-Path $stage 'licenses')) | Out-Null
    foreach ($name in $sources.Keys) { Copy-Item -LiteralPath $sources[$name] -Destination (Join-Path $stage $name) }
    $readme = @'
Kaleidoscope — Illyaverse local test package (__BUILD_PROFILE__)

Run Kaleidoscope.exe, choose the installed stable/lazer osu!.exe, then launch.
The default endpoint is https://illya.test. Settings accept a validated HTTPS
origin for the next launch; an active session keeps its selected origin. The
illya.test setup requires the documented local DNS/CA. TLS remains required;
official osu! fallback is disabled. Stable requires the default HTTPS port 443;
lazer also supports custom HTTPS ports. This is a local test artifact.
Lazer updates outside the tested build stop before Main. Update the complete
launcher and bridge package together; do not copy this DLL into official rulesets.
Lazer uses your existing beatmap library, including configured custom/portable
storage. Gameplay, UI, audio and input settings remain shared in original
game.ini. Only account/session state and remember-login flags use
auth.illyaverse.json in that same library, with a separate profile for each
canonical endpoint. Official account fields are filtered before private API use
and preserved when saving shared settings. Old game.illyaverse.ini is not
copied or migrated. Changing the data folder
requires closing the private client and configuring the normal lazer client.
Close the game to remove the temporary DLL from the existing rulesets folder.
The independent guardian survives closing this window. After an interrupted
guardian or power loss, reopen the launcher and use cleanup. Foreign or changed
files are kept.

Native GUI/gameplay/login remains pending user-operated verification. Package
integrity and embedded-interface evidence do not prove a real game session.
See Transfiguration-README.md, compatibility.json, and THIRD-PARTY-NOTICES.md.
GUI font licenses and provenance are in Kaleidoscope-THIRD-PARTY-NOTICES.md
and licenses/Nunito.txt and licenses/NotoSansTC.txt.
'@
    $buildLabel = if ($synthetic) { 'synthetic integrity fixture; no executable build' } elseif ($BuildProfile -eq 'Debug') { 'debug GUI connection test build' } else { 'release GUI build' }
    $readme = $readme.Replace('__BUILD_PROFILE__', $buildLabel)
    [IO.File]::WriteAllText((Join-Path $stage 'README.txt'), $readme, [Text.UTF8Encoding]::new($false))
    $hashes = [ordered]@{}
    foreach ($name in $script:guiPayloadNames) { $hashes[$name] = (Get-FileHash -LiteralPath (Join-Path $stage $name) -Algorithm SHA256).Hash.ToLowerInvariant() }
    if ($hashes['Kaleidoscope.exe'] -ine $InterfaceEvidence.launcher_sha256 -or $hashes['compatibility.json'] -ine $expectedCompatibilityHash) { throw 'Verified launcher or compatibility changed during package assembly.' }
    # Completion metadata is the final file written, after every required payload file.
    $manifest = [ordered]@{
        version = 1; status = 'complete'; default_endpoint = 'https://illya.test'; endpoint_mode = 'https_origin_per_session'; configurable_endpoint = $true; fixture = $false; build_profile = $(if ($synthetic) { 'synthetic' } else { $BuildProfile.ToLowerInvariant() }); bridge_sha256 = $bridgeHash
        embedded_interface_verified = !$synthetic; native_gui_verified = $false; verification_kind = $InterfaceEvidence.kind
        completed_at_utc = [DateTime]::UtcNow.ToString('o'); content_sha256 = $hashes
    }
    [IO.File]::WriteAllText((Join-Path $stage 'package.json'), ($manifest | ConvertTo-Json -Depth 10), [Text.UTF8Encoding]::new($false))
    $null = Test-KaleidoscopePackage -Directory $stage -AllowSyntheticFixtures:$synthetic
    [IO.Compression.ZipFile]::CreateFromDirectory($stage, $stageZip, [IO.Compression.CompressionLevel]::Optimal, $false)
    $null = Test-KaleidoscopePackageArchive -ArchivePath $stageZip -Directory $stage -AllowSyntheticFixtures:$synthetic
    # Directory.Move/File.Move refuse existing targets; no old delivery is overwritten.
    $null = Assert-PackageWorkspacePath $stage $WorkspaceRoot
    $null = Assert-PackageWorkspacePath $output $WorkspaceRoot
    [IO.Directory]::Move($stage, $output)
    $null = Assert-PackageWorkspacePath $stageZip $WorkspaceRoot
    $null = Assert-PackageWorkspacePath $archive $WorkspaceRoot
    [IO.File]::Move($stageZip, $archive)
    return [pscustomobject]@{ Directory = $output; Archive = $archive; VerificationKind = $InterfaceEvidence.kind }
}

Export-ModuleMember -Function Assert-PackageWorkspacePath, Resolve-KaleidoscopeCargoTarget, Copy-KaleidoscopeLauncherArtifact, Test-KaleidoscopePackage, Test-KaleidoscopePackageArchive, New-KaleidoscopePackage, Test-KaleidoscopeCliTestPackage, Test-KaleidoscopeCliTestPackageArchive, New-KaleidoscopeCliTestPackage
