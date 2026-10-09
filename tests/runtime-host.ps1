param()
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../..')).Path
$proofRoot = Join-Path $repo ('data-runtime-host-proof-' + [guid]::NewGuid().ToString('N'))
$fixtureRoot = Join-Path $PSScriptRoot 'runtime-fixtures'
$previous = @{}
foreach ($key in @('DOTNET_STARTUP_HOOKS','ILLYA_FIXTURE_MAIN_MARKER','ILLYA_FIXTURE_HOOK_MARKER','ILLYA_FIXTURE_HOOK_REQUIRED','ILLYA_FIXTURE_THROW','ILLYA_FIXTURE_EXPECTED_ENTRY','ILLYA_FIXTURE_ENTRY_RECORD')) {
    $previous[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
function Assert-NativeSuccess([string]$step) {
    if ($LASTEXITCODE -ne 0) { throw "$step failed" }
}
try {
    New-Item -ItemType Directory -Path $proofRoot | Out-Null
    Push-Location $repo
    try {
        & cargo build -p kaleidoscope --locked --example runtime_probe
        Assert-NativeSuccess 'Runtime proof build'
        & dotnet publish (Join-Path $fixtureRoot 'application/Application.csproj') -c Release -r win-x64 --self-contained true -o (Join-Path $proofRoot 'application')
        Assert-NativeSuccess 'Headless self-contained fixture build'
        & dotnet build (Join-Path $fixtureRoot 'guard/Guard.csproj') -c Release -o (Join-Path $proofRoot 'guard')
        Assert-NativeSuccess 'Headless hook fixture build'
    } finally { Pop-Location }
    $cargoTarget = if ($env:CARGO_TARGET_DIR) { [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR, $repo) } else { Join-Path $repo 'target' }
    $probe = Join-Path $cargoTarget 'debug/examples/runtime_probe.exe'
    $application = Join-Path $proofRoot 'application/Application.dll'
    $hostLibrary = Join-Path $proofRoot 'application/hostfxr.dll'
    $guard = Join-Path $proofRoot 'guard/Guard.dll'
    $env:ILLYA_FIXTURE_MAIN_MARKER = Join-Path $proofRoot 'baseline-main'
    $env:ILLYA_FIXTURE_HOOK_MARKER = Join-Path $proofRoot 'baseline-hook'
    $env:DOTNET_STARTUP_HOOKS = $guard
    Remove-Item Env:ILLYA_FIXTURE_THROW,Env:ILLYA_FIXTURE_HOOK_REQUIRED,Env:ILLYA_FIXTURE_EXPECTED_ENTRY,Env:ILLYA_FIXTURE_ENTRY_RECORD -ErrorAction SilentlyContinue
    & (Join-Path $proofRoot 'application/Application.exe') | Out-Null
    if ($LASTEXITCODE -ne 23 -or (Test-Path -LiteralPath $env:ILLYA_FIXTURE_HOOK_MARKER) -or !(Test-Path -LiteralPath $env:ILLYA_FIXTURE_MAIN_MARKER)) { throw 'Disabled startup-hook baseline did not match' }
    Remove-Item Env:DOTNET_STARTUP_HOOKS
    $env:ILLYA_FIXTURE_HOOK_REQUIRED = '1'
    $env:ILLYA_FIXTURE_MAIN_MARKER = Join-Path $proofRoot 'required-main'
    $env:ILLYA_FIXTURE_HOOK_MARKER = Join-Path $proofRoot 'required-hook'
    $null = & $probe $application $hostLibrary $guard 2>&1
    if ($LASTEXITCODE -ne 23 -or !(Test-Path -LiteralPath $env:ILLYA_FIXTURE_HOOK_MARKER) -or !(Test-Path -LiteralPath $env:ILLYA_FIXTURE_MAIN_MARKER)) { throw 'Mandatory startup hook did not run before Main' }
    $env:ILLYA_FIXTURE_THROW = '1'
    $env:ILLYA_FIXTURE_MAIN_MARKER = Join-Path $proofRoot 'refused-main'
    $null = & $probe $application $hostLibrary $guard 2>&1
    if ($LASTEXITCODE -eq 23 -or (Test-Path -LiteralPath $env:ILLYA_FIXTURE_MAIN_MARKER)) { throw 'Refused hook reached Main' }
    Remove-Item Env:ILLYA_FIXTURE_THROW
    $env:ILLYA_FIXTURE_MAIN_MARKER = Join-Path $proofRoot 'missing-main'
    $missingDiagnostic = & $probe $application $hostLibrary (Join-Path $proofRoot 'missing.dll') 2>&1
    if ($LASTEXITCODE -ne 2 -or (Test-Path -LiteralPath $env:ILLYA_FIXTURE_MAIN_MARKER) -or !($missingDiagnostic -match 'host_path_invalid') -or ($missingDiagnostic -match [regex]::Escape($proofRoot))) { throw 'Missing hook diagnostic boundary failed' }
    $extendedApplication = '\\?\' + $application
    $env:ILLYA_FIXTURE_EXPECTED_ENTRY = $application
    $env:ILLYA_FIXTURE_ENTRY_RECORD = Join-Path $proofRoot 'extended-entry.json'
    $env:ILLYA_FIXTURE_MAIN_MARKER = Join-Path $proofRoot 'extended-main'
    $env:ILLYA_FIXTURE_HOOK_MARKER = Join-Path $proofRoot 'extended-hook'
    $null = & $probe $extendedApplication $hostLibrary $guard 2>&1
    if ($LASTEXITCODE -ne 23 -or !(Test-Path -LiteralPath $env:ILLYA_FIXTURE_MAIN_MARKER)) { throw 'Extended local application path failed' }
    $entry = Get-Content -LiteralPath $env:ILLYA_FIXTURE_ENTRY_RECORD -Raw | ConvertFrom-Json
    if (!$entry.matches -or !$entry.beforeMain -or $entry.canonicalLoaded -ine $entry.canonicalExpected) { throw 'Extended local entry path was not checked before Main' }
    $env:ILLYA_FIXTURE_EXPECTED_ENTRY = $extendedApplication
    $env:ILLYA_FIXTURE_ENTRY_RECORD = Join-Path $proofRoot 'ordinary-entry.json'
    $env:ILLYA_FIXTURE_MAIN_MARKER = Join-Path $proofRoot 'ordinary-main'
    $env:ILLYA_FIXTURE_HOOK_MARKER = Join-Path $proofRoot 'ordinary-hook'
    $null = & $probe $application $hostLibrary $guard 2>&1
    if ($LASTEXITCODE -ne 23 -or !(Test-Path -LiteralPath $env:ILLYA_FIXTURE_MAIN_MARKER)) { throw 'Ordinary application versus extended expected path failed' }
    $entry = Get-Content -LiteralPath $env:ILLYA_FIXTURE_ENTRY_RECORD -Raw | ConvertFrom-Json
    if (!$entry.matches -or !$entry.beforeMain) { throw 'Ordinary local entry path was not checked before Main' }
    $foreignDirectory = Join-Path $proofRoot 'different-install'
    New-Item -ItemType Directory -Path $foreignDirectory | Out-Null
    $foreignApplication = Join-Path $foreignDirectory 'Application.dll'
    Copy-Item -LiteralPath $application -Destination $foreignApplication
    foreach ($expected in @($foreignApplication, ('\\?\' + $foreignApplication))) {
        $suffix = if ($expected.StartsWith('\\?\')) { 'extended' } else { 'ordinary' }
        $env:ILLYA_FIXTURE_EXPECTED_ENTRY = $expected
        $env:ILLYA_FIXTURE_ENTRY_RECORD = Join-Path $proofRoot ('foreign-' + $suffix + '-entry.json')
        $env:ILLYA_FIXTURE_MAIN_MARKER = Join-Path $proofRoot ('foreign-' + $suffix + '-main')
        $env:ILLYA_FIXTURE_HOOK_MARKER = Join-Path $proofRoot ('foreign-' + $suffix + '-hook')
        $null = & $probe $extendedApplication $hostLibrary $guard 2>&1
        if ($LASTEXITCODE -eq 23 -or (Test-Path -LiteralPath $env:ILLYA_FIXTURE_MAIN_MARKER)) { throw 'Different installation reached Main' }
        $entry = Get-Content -LiteralPath $env:ILLYA_FIXTURE_ENTRY_RECORD -Raw | ConvertFrom-Json
        if ($entry.matches -or !$entry.beforeMain) { throw 'Different installation identity was accepted' }
    }
    Write-Output 'PASS 8 headless self-contained runtime-host boundaries; safe native codes, extended paths and different-install refusal; no game launched'
} finally {
    foreach ($key in $previous.Keys) { [Environment]::SetEnvironmentVariable($key, $previous[$key], 'Process') }
    if (Test-Path -LiteralPath $proofRoot) {
        $resolvedProof = (Resolve-Path -LiteralPath $proofRoot).Path
        if ([IO.Path]::GetDirectoryName($resolvedProof) -ne $repo -or !([IO.Path]::GetFileName($resolvedProof).StartsWith('data-runtime-host-proof-')) -or (Get-Item -LiteralPath $resolvedProof).Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { throw 'Unsafe proof cleanup target' }
        Remove-Item -LiteralPath $resolvedProof -Recurse -Force
    }
}
