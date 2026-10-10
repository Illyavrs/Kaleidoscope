[CmdletBinding()]
param([string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'launcher-package.psm1') -Force
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../..')).Path
if (!$OutputDirectory) {
    $packageParent = Assert-PackageWorkspacePath (Join-Path $repo '.artifacts/packages') $repo
    [IO.Directory]::CreateDirectory($packageParent) | Out-Null
    $OutputDirectory = Join-Path $packageParent ('kaleidoscope-cli-test-' + [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss') + '-' + [guid]::NewGuid().ToString('N'))
}
$output = Assert-PackageWorkspacePath $OutputDirectory $repo
if ((Test-Path -LiteralPath $output) -or (Test-Path -LiteralPath ($output + '.zip')) -or (Test-Path -LiteralPath ($output + '.build.log'))) { throw 'CLI package target already exists.' }
if (!(Test-Path -LiteralPath ([IO.Path]::GetDirectoryName($output)) -PathType Container)) { throw 'CLI package parent must already exist.' }
$cargoTarget = Assert-PackageWorkspacePath (Join-Path $repo 'target') $repo
$launcher = Assert-PackageWorkspacePath (Join-Path $cargoTarget 'debug/kaleidoscope.exe') $repo
$bridgeDirectory = Assert-PackageWorkspacePath (Join-Path $repo 'tools/transfiguration/artifacts') $repo
$bridge = Assert-PackageWorkspacePath (Join-Path $bridgeDirectory 'osu.Game.Rulesets.Transfiguration.dll') $repo
$artifactPath = Assert-PackageWorkspacePath (Join-Path $bridgeDirectory 'artifact.json') $repo
$compatibilityPath = Assert-PackageWorkspacePath (Join-Path $bridgeDirectory 'compatibility.json') $repo
$expectedCompatibilityPath = Assert-PackageWorkspacePath (Join-Path $repo 'tools/transfiguration/compatibility.json') $repo
$licensePath = Assert-PackageWorkspacePath (Join-Path $repo 'LICENSE') $repo
$bridgeHash = (Get-FileHash -LiteralPath $bridge -Algorithm SHA256).Hash.ToLowerInvariant()
$artifact = Get-Content -LiteralPath $artifactPath -Raw | ConvertFrom-Json -AsHashtable
if ($artifact.protocol -ne 1 -or $artifact.fixture -isnot [bool] -or $artifact.fixture -ne $false -or $artifact.default_endpoint -cne 'https://shdcloud.top' -or $artifact.endpoint_mode -cne 'https_origin_per_session' -or $artifact.configurable_endpoint -isnot [bool] -or !$artifact.configurable_endpoint -or $artifact.artifact -cne 'osu.Game.Rulesets.Transfiguration.dll' -or $artifact.sha256 -cne $bridgeHash) { throw 'Production bridge preflight failed.' }
if ((Get-FileHash -LiteralPath $compatibilityPath -Algorithm SHA256).Hash -ine (Get-FileHash -LiteralPath $expectedCompatibilityPath -Algorithm SHA256).Hash) { throw 'Production compatibility preflight failed.' }
$previousHash = [Environment]::GetEnvironmentVariable('ILLYA_BRIDGE_SHA256', 'Process')
$previousIncremental = [Environment]::GetEnvironmentVariable('CARGO_INCREMENTAL', 'Process')
$previousTarget = [Environment]::GetEnvironmentVariable('CARGO_TARGET_DIR', 'Process')
Push-Location $repo
try {
    $env:ILLYA_BRIDGE_SHA256 = $bridgeHash
    $env:CARGO_INCREMENTAL = '0'
    $env:CARGO_TARGET_DIR = $cargoTarget
    # Ordinary existing debug target. Never retries release builds or alters
    # security policy, profiles, ACLs, scanner settings or unreadable artifacts.
    & cargo build -p kaleidoscope --locked 2>&1 | Tee-Object -FilePath ($output + '.build.log')
    if ($LASTEXITCODE -ne 0) { throw "Normal CLI build failed; log retained at $output.build.log. No test package was produced." }
    $package = New-KaleidoscopeCliTestPackage -WorkspaceRoot $repo -LauncherPath $launcher -BridgeDirectory $bridgeDirectory -LicensePath $licensePath -ExpectedCompatibilityPath $expectedCompatibilityPath -OutputDirectory $output
    Write-Output ($package | ConvertTo-Json -Depth 4)
} finally {
    Pop-Location
    [Environment]::SetEnvironmentVariable('ILLYA_BRIDGE_SHA256', $previousHash, 'Process')
    [Environment]::SetEnvironmentVariable('CARGO_INCREMENTAL', $previousIncremental, 'Process')
    [Environment]::SetEnvironmentVariable('CARGO_TARGET_DIR', $previousTarget, 'Process')
}
