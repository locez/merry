# Merry installer for Windows (PowerShell 5.1+ / PowerShell 7+).
# Usage:
#   irm https://github.com/locez/merry/releases/latest/download/install.ps1 | iex
# Environment overrides:
#   $env:MERRY_VERSION      Release tag to install (e.g. v0.1.0); defaults to latest
#   $env:MERRY_INSTALL_DIR  Destination directory; defaults to "$env:LOCALAPPDATA\merry\bin"

$ErrorActionPreference = "Stop"

$Repo = "locez/merry"

function Get-Target {
    $arch = $env:PROCESSOR_ARCHITECTURE
    if ($arch -eq "AMD64" -or [Environment]::Is64BitOperatingSystem) {
        if ($arch -eq "ARM64") {
            throw "Windows ARM64 prebuilt binaries are not published yet; install with 'cargo install --locked merry-cli'."
        }
        return "x86_64-pc-windows-msvc"
    }
    throw "Unsupported Windows architecture: $arch"
}

function Resolve-Version {
    if ($env:MERRY_VERSION) {
        if ($env:MERRY_VERSION.StartsWith("v")) {
            return $env:MERRY_VERSION
        }
        return "v$($env:MERRY_VERSION)"
    }

    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers @{ "User-Agent" = "merry-installer" }
    if (-not $release.tag_name) {
        throw "Failed to resolve latest Merry release tag."
    }
    return [string]$release.tag_name
}

$target = Get-Target
$version = Resolve-Version
$installDir = if ($env:MERRY_INSTALL_DIR) { $env:MERRY_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "merry\bin" }
$archive = "merry-$version-$target.zip"
$baseUrl = "https://github.com/$Repo/releases/download/$version"

$tmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("merry-install-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $tmpDir | Out-Null

try {
    $archivePath = Join-Path $tmpDir $archive
    $sumsPath = Join-Path $tmpDir "SHA256SUMS.txt"

    Write-Host "Downloading Merry $version ($target)..."
    Invoke-WebRequest -Uri "$baseUrl/$archive" -OutFile $archivePath -UseBasicParsing
    Invoke-WebRequest -Uri "$baseUrl/SHA256SUMS.txt" -OutFile $sumsPath -UseBasicParsing

    $expected = $null
    foreach ($line in Get-Content $sumsPath) {
        $parts = $line.Trim() -split '\s+'
        if ($parts.Length -ge 2 -and $parts[1] -eq $archive) {
            $expected = $parts[0].ToLowerInvariant()
            break
        }
    }
    if (-not $expected) {
        throw "Checksum entry for $archive not found in SHA256SUMS.txt"
    }

    $actual = (Get-FileHash -Path $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "SHA256 mismatch for $archive (expected $expected, got $actual)"
    }

    $extractDir = Join-Path $tmpDir "extract"
    Expand-Archive -Path $archivePath -DestinationPath $extractDir -Force

    $srcBin = Join-Path $extractDir "merry-$version-$target\merry.exe"
    if (-not (Test-Path $srcBin)) {
        throw "Archive did not contain expected binary: $srcBin"
    }

    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    $destBin = Join-Path $installDir "merry.exe"
    Copy-Item -Path $srcBin -Destination $destBin -Force

    Write-Host "Installed merry $version to $destBin"

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $entries = if ($userPath) { $userPath -split ';' | Where-Object { $_ -ne '' } } else { @() }
    if ($entries -notcontains $installDir) {
        $newPath = if ($userPath) { "$userPath;$installDir" } else { $installDir }
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        $env:Path = "$env:Path;$installDir"
        Write-Host "Added $installDir to your user PATH. Restart your terminal if 'merry' is not found."
    }
}
finally {
    Remove-Item -Path $tmpDir -Recurse -Force -ErrorAction SilentlyContinue
}
