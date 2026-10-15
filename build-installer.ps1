# Builds ride and rterm and packs them into rkit's single setup program: dist\rkit-setup.exe
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

# The setup program embeds the release build of ride, so that is built first.
cargo build --release -p ride -p rterm -p rkit
if ($LASTEXITCODE -ne 0) { throw "building ride failed" }
cargo build --release -p rkit-setup
if ($LASTEXITCODE -ne 0) { throw "building the setup program failed" }

New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item target\release\rkit-setup.exe dist\rkit-setup.exe -Force
$size = [math]::Round((Get-Item dist\rkit-setup.exe).Length / 1MB, 1)
Write-Host "dist\rkit-setup.exe ($size MB)"
