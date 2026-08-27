# Q1 ART <-> iAi A/B harness launcher (Phase 1 of the Develop completion plan).
#
# Drives tests/raw_q1_art_ab.rs: renders a locked recipe set from iAi (Develop3)
# and from ART (black-box oracle via ART-cli.exe), aligns them, measures the
# difference and writes labeled + blind contact sheets, a manifest and metrics.
#
# ART is used only as a black-box oracle; no ART code/constant/LUT/profile/asset
# is copied into iAi.
#
# Usage (from the repo root):
#   ./scripts/q1_art_ab.ps1
#   ./scripts/q1_art_ab.ps1 -Files '_DLL6009,HUY_7933' -Recipes 'neutral,exp_p1'
#
param(
    [string]$Corpus  = 'C:\Users\Admin\Pictures\anh-raw',
    [string]$ArtCli  = 'C:\Users\Admin\Pictures\1111\ART_1.26.7_Win64_portable',
    [string]$Out     = 'C:\Users\Admin\Documents\IAI\target\q1_ab',
    [string]$Files   = '',
    [string]$Recipes = '',
    [int]   $Width   = 1100,
    [int]   $MaxFiles = 0
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot

# The iAi camera DCPs live beside the release binary, but the integration-test
# executable runs from target/release/deps, so mirror the DCP folder there. This
# makes the harness resolve the same per-camera profiles the portable app uses.
$srcDcp = Join-Path $repo 'target/release/camera_profiles'
$dstDcp = Join-Path $repo 'target/release/deps/camera_profiles'
if (Test-Path $srcDcp) {
    New-Item -ItemType Directory -Force -Path $dstDcp | Out-Null
    Copy-Item (Join-Path $srcDcp '*.dcp') $dstDcp -Force
    Write-Host "Mirrored $((Get-ChildItem $dstDcp -Filter *.dcp).Count) DCP(s) beside the test binary."
} else {
    Write-Warning "No local DCPs at $srcDcp; iAi will use the no-profile fallback for every camera."
}

$env:IAI_RAW_CORPUS = $Corpus
$env:IAI_ART_CLI    = $ArtCli
$env:IAI_Q1_OUT     = $Out
$env:IAI_Q1_WIDTH   = "$Width"
if ($Files)    { $env:IAI_Q1_FILES = $Files }       else { Remove-Item Env:IAI_Q1_FILES -ErrorAction SilentlyContinue }
if ($Recipes)  { $env:IAI_Q1_RECIPES = $Recipes }   else { Remove-Item Env:IAI_Q1_RECIPES -ErrorAction SilentlyContinue }
if ($MaxFiles -gt 0) { $env:IAI_Q1_MAX_FILES = "$MaxFiles" } else { Remove-Item Env:IAI_Q1_MAX_FILES -ErrorAction SilentlyContinue }

Push-Location $repo
try {
    cargo test --release --test raw_q1_art_ab -- --ignored --nocapture
    Write-Host "`nDone. Open $Out\index.html (labeled) or $Out\blind.html (blind)."
} finally {
    Pop-Location
}
