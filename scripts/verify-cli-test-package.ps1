[CmdletBinding(DefaultParameterSetName = 'Directory')]
param(
    [Parameter(Mandatory, ParameterSetName = 'Directory')][string]$Directory,
    [Parameter(Mandatory, ParameterSetName = 'Archive')][string]$ArchivePath
)
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'launcher-package.psm1') -Force
if ($PSCmdlet.ParameterSetName -eq 'Archive') {
    $manifest = Test-KaleidoscopeCliTestPackageArchive -ArchivePath $ArchivePath
} else {
    $manifest = Test-KaleidoscopeCliTestPackage -Directory $Directory
}
Write-Output "PASS $($manifest.artifact_kind) package integrity; native login remains pending manual testing."
