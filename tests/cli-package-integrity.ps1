[CmdletBinding()]
param([Parameter(Mandatory)][string]$PackageDirectory)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot '../scripts/launcher-package.psm1') -Force
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../..')).Path
$package = (Resolve-Path -LiteralPath $PackageDirectory).Path
$null = Test-KaleidoscopeCliTestPackage -Directory $package
$identity = [guid]::NewGuid().ToString('N')
$root = Assert-PackageWorkspacePath (Join-Path $repo ('.artifacts/tests/cli-package/run-' + $identity)) $repo
$checks = 0
$success = $false
function Assert-Check([bool]$Condition, [string]$Name) {
    if (!$Condition) { throw "Failed CLI package check: $Name" }
    $script:checks++
    Write-Output "PASS $Name"
}
function Expect-Refusal([string]$Name, [scriptblock]$Action, [string]$MessagePattern = '*') {
    try { $null = & $Action } catch {
        if ($_.Exception.Message -notlike $MessagePattern) { throw "Unexpected refusal in ${Name}: $($_.Exception.Message)" }
        $script:checks++; Write-Output "PASS $Name"; return
    }
    throw "CLI package failed to refuse: $Name"
}
function Copy-Fixture([string]$Name) {
    $copy = Assert-PackageWorkspacePath (Join-Path $root $Name) $repo
    Copy-Item -LiteralPath $package -Destination $copy -Recurse
    return $copy
}
function Write-Json([string]$Path, $Value) { [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 20), [Text.UTF8Encoding]::new($false)) }
function Get-ByteInventory([string]$Directory) {
    $records = [ordered]@{}
    foreach ($item in Get-ChildItem -LiteralPath $Directory -File -Recurse -Force) {
        if ($item.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { throw 'Fixture unexpectedly contains a reparse point.' }
        $records[[IO.Path]::GetRelativePath($Directory, $item.FullName)] = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash
    }
    return ($records | ConvertTo-Json -Compress)
}
function Expect-DiagnosticRefusal([string]$Directory, [string]$Name) {
    $before = Get-ByteInventory $Directory
    $errorFile = Join-Path $root ('diagnostic-' + $Name + '.stderr')
    $output = & (Join-Path $Directory 'Kaleidoscope.exe') --inspect-package-bindings 2> $errorFile
    Assert-Check ($LASTEXITCODE -eq 2 -and !$output) "$Name diagnostic refuses before runtime"
    Assert-Check ((Get-ByteInventory $Directory) -ceq $before) "$Name diagnostic leaves all package bytes unchanged"
}
try {
    [IO.Directory]::CreateDirectory($root) | Out-Null
    Write-Json (Join-Path $root '.owner.json') @{ identity = $identity; root = $root }
    $null = Test-KaleidoscopeCliTestPackageArchive -ArchivePath ($package + '.zip') -Directory $package
    Assert-Check $true 'production CLI directory and ZIP match every payload byte'
    $bindings = (& (Join-Path $package 'Kaleidoscope.exe') --inspect-package-bindings | Out-String) | ConvertFrom-Json -AsHashtable
    Assert-Check ($LASTEXITCODE -eq 0 -and $bindings.bridge_verified -eq $true -and $bindings.official_fallback -eq $false -and $bindings.shared_existing_library -eq $true) 'actual production-bound CLI diagnostic passes'
    $manifest = Get-Content -LiteralPath (Join-Path $package 'package.json') -Raw | ConvertFrom-Json -AsHashtable
    Assert-Check ($manifest.artifact_kind -ceq 'cli_connection_test' -and $manifest.native_gui_verified -eq $false -and $manifest.native_login_verified -eq $false) 'CLI manifest keeps GUI and login unverified'
    Expect-Refusal 'desktop GUI verifier rejects CLI-only evidence' { Test-KaleidoscopePackage -Directory $package }

    $constructor = @{
        WorkspaceRoot = $repo; LauncherPath = (Join-Path $package 'Kaleidoscope.exe')
        BridgeDirectory = $package; LicensePath = (Join-Path $package 'LICENSE')
        ExpectedCompatibilityPath = (Join-Path $repo 'tools/transfiguration/compatibility.json')
    }
    # The outside path is never created or opened. The junction points only to
    # an owned fixture and is refused before staging or diagnostic execution.
    $outside = Join-Path ([IO.Path]::GetTempPath()) ('kaleidoscope-never-create-' + $identity + '.exe')
    $outsideArgs = $constructor.Clone()
    $outsideArgs.LauncherPath = $outside
    $outsideOutput = Join-Path $root 'refuse-outside'
    Expect-Refusal 'constructor refuses outside-workspace launcher before staging' { New-KaleidoscopeCliTestPackage @outsideArgs -OutputDirectory $outsideOutput } '*inside the repository workspace*'
    Assert-Check (!(Test-Path -LiteralPath $outsideOutput) -and !(Test-Path -LiteralPath $outside)) 'outside refusal produces no staged executable or outside file'
    $linkTarget = Copy-Fixture 'source-target'
    $junction = Join-Path $root 'source-junction'
    $null = New-Item -ItemType Junction -Path $junction -Target $linkTarget
    try {
        foreach ($field in @('LauncherPath', 'BridgeDirectory', 'LicensePath', 'ExpectedCompatibilityPath')) {
            $linkedArgs = $constructor.Clone()
            $linkedArgs[$field] = switch ($field) {
                'LauncherPath' { Join-Path $junction 'Kaleidoscope.exe' }
                'BridgeDirectory' { $junction }
                'LicensePath' { Join-Path $junction 'LICENSE' }
                'ExpectedCompatibilityPath' { Join-Path $junction 'compatibility.json' }
            }
            $before = Get-ByteInventory $root
            $linkedOutput = Join-Path $root ('refuse-linked-' + $field)
            Expect-Refusal "constructor refuses $field with linked source ancestor" { New-KaleidoscopeCliTestPackage @linkedArgs -OutputDirectory $linkedOutput } '*reparse point*'
            Assert-Check (!(Test-Path -LiteralPath $linkedOutput) -and (Get-ByteInventory $root) -ceq $before) "$field refusal happens before staging or process execution"
        }
        Expect-Refusal 'fixed Cargo target guard rejects linked ancestor' { Assert-PackageWorkspacePath (Join-Path $junction 'debug/kaleidoscope.exe') $repo } '*reparse point*'
    } finally {
        # Remove just the junction object; never recurse through its target.
        $checkedLink = Assert-PackageWorkspacePath (Join-Path $root 'junction-removal-parent-check') $repo
        if ([IO.Path]::GetDirectoryName($checkedLink) -cne $root -or !(Get-Item -LiteralPath $junction -Force).Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { throw 'Source junction identity differs; retained.' }
        [IO.Directory]::Delete($junction, $false)
    }

    $missing = Copy-Fixture 'missing'
    Remove-Item -LiteralPath (Join-Path $missing 'osu.Game.Rulesets.Transfiguration.dll')
    Expect-DiagnosticRefusal $missing 'missing-bridge'
    Expect-Refusal 'CLI package verifier rejects missing bridge' { Test-KaleidoscopeCliTestPackage -Directory $missing }
    $changed = Copy-Fixture 'changed'
    $dll = Join-Path $changed 'osu.Game.Rulesets.Transfiguration.dll'
    $bytes = [IO.File]::ReadAllBytes($dll)
    $bytes[0] = $bytes[0] -bxor 1
    [IO.File]::WriteAllBytes($dll, $bytes)
    Expect-DiagnosticRefusal $changed 'changed-bridge'
    Expect-Refusal 'CLI package verifier rejects changed bridge' { Test-KaleidoscopeCliTestPackage -Directory $changed }

    # Re-signing the adjacent metadata cannot override the hash compiled into
    # the real CLI. The altered DLL is never loaded or installed into a game.
    $wrong = Copy-Fixture 'wrong'
    [IO.File]::WriteAllText((Join-Path $wrong 'osu.Game.Rulesets.Transfiguration.dll'), 'wrong DLL fixture; never loaded')
    $wrongHash = (Get-FileHash -LiteralPath (Join-Path $wrong 'osu.Game.Rulesets.Transfiguration.dll') -Algorithm SHA256).Hash.ToLowerInvariant()
    $artifact = Get-Content -LiteralPath (Join-Path $wrong 'artifact.json') -Raw | ConvertFrom-Json -AsHashtable
    $artifact.sha256 = $wrongHash
    Write-Json (Join-Path $wrong 'artifact.json') $artifact
    Expect-DiagnosticRefusal $wrong 'metadata-replaced-wrong-bridge'

    foreach ($field in @('official_fallback', 'private_config_filename', 'native_login_verified', 'artifact_kind')) {
        $tampered = Copy-Fixture ('policy-' + $field)
        $value = Get-Content -LiteralPath (Join-Path $tampered 'package.json') -Raw | ConvertFrom-Json -AsHashtable
        switch ($field) {
            'official_fallback' { $value.package_bindings.official_fallback = $true }
            'private_config_filename' { $value.package_bindings.private_config_filename = 'game.ini' }
            'native_login_verified' { $value.native_login_verified = $true }
            'artifact_kind' { $value.artifact_kind = 'gui' }
        }
        Write-Json (Join-Path $tampered 'package.json') $value
        Expect-Refusal "CLI verifier rejects $field overclaim or policy change" { Test-KaleidoscopeCliTestPackage -Directory $tampered }
    }
    $mismatch = Copy-Fixture 'zip-mismatch'
    [IO.File]::AppendAllText((Join-Path $mismatch 'README.txt'), "`nFixture changed README.")
    $value = Get-Content -LiteralPath (Join-Path $mismatch 'package.json') -Raw | ConvertFrom-Json -AsHashtable
    $value.content_sha256['README.txt'] = (Get-FileHash -LiteralPath (Join-Path $mismatch 'README.txt') -Algorithm SHA256).Hash.ToLowerInvariant()
    Write-Json (Join-Path $mismatch 'package.json') $value
    $null = Test-KaleidoscopeCliTestPackage -Directory $mismatch
    Expect-Refusal 'CLI archive must match the assembled directory' { Test-KaleidoscopeCliTestPackageArchive -ArchivePath ($package + '.zip') -Directory $mismatch }
    foreach ($name in @('duplicate', 'traversal')) {
        $zipPath = Join-Path $root ($name + '.zip')
        Copy-Item -LiteralPath ($package + '.zip') -Destination $zipPath
        $zip = [IO.Compression.ZipFile]::Open($zipPath, [IO.Compression.ZipArchiveMode]::Update)
        try {
            $entry = $zip.CreateEntry($(if ($name -eq 'duplicate') { 'README.txt' } else { '../README.txt' }))
            $writer = [IO.StreamWriter]::new($entry.Open())
            try { $writer.Write('unsafe fixture entry') } finally { $writer.Dispose() }
        } finally { $zip.Dispose() }
        Expect-Refusal "CLI archive rejects $name entry" { Test-KaleidoscopeCliTestPackageArchive -ArchivePath $zipPath }
    }
    $success = $true
    Write-Output "PASS $checks CLI-only package integrity and compiled binding checks; no game launched."
} finally {
    if ($success -and (Test-Path -LiteralPath $root)) {
        $resolved = Assert-PackageWorkspacePath (Resolve-Path -LiteralPath $root).Path $repo
        $owner = Get-Content -LiteralPath (Join-Path $resolved '.owner.json') -Raw | ConvertFrom-Json -AsHashtable
        if ($owner.identity -cne $identity -or $owner.root -cne $root -or [IO.Path]::GetFileName($resolved) -cne ('run-' + $identity)) { throw 'CLI fixture ownership differs; retained.' }
        foreach ($item in Get-ChildItem -LiteralPath $resolved -Recurse -Force) { if ($item.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { throw 'CLI fixture contains a reparse point; retained.' } }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
