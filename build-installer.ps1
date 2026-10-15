# Builds ride and packs it into a single setup program: dist\ride-setup.exe
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

# The setup program embeds the release build of ride, so that is built first.
cargo build --release -p ride -p rterm
if ($LASTEXITCODE -ne 0) { throw "building ride failed" }
cargo build --release -p ride-setup
if ($LASTEXITCODE -ne 0) { throw "building the setup program failed" }

New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item target\release\ride-setup.exe dist\ride-setup.exe -Force
$size = [math]::Round((Get-Item dist\ride-setup.exe).Length / 1MB, 1)
Write-Host "dist\ride-setup.exe ($size MB)"
