# triage-cli installer for Windows.
# Usage:  irm https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.ps1 | iex
# Flags:  -Version v0.2.0      Pin to a specific release tag.
#         -Channel prerelease  Allow prereleases when picking "latest".
#         -DryRun              Print actions without executing them.

[CmdletBinding()]
param(
    [string]$Version,
    [ValidateSet('stable', 'prerelease')]
    [string]$Channel = 'stable',
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo       = 'midwestman35/triage-cli'
$BinDir     = Join-Path $env:LOCALAPPDATA 'Programs\triage-cli\bin'
$DataDirEnv = $env:TRIAGE_HOME
$DataDir    = if ($DataDirEnv) { $DataDirEnv } else { Join-Path $env:LOCALAPPDATA 'triage-cli' }

function Step($msg) {
    if ($DryRun) { Write-Host "[dry-run] $msg" -ForegroundColor Yellow }
    else        { Write-Host $msg -ForegroundColor Cyan }
}

# 1. Pre-flight: arch check.
if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw "Unsupported architecture: $env:PROCESSOR_ARCHITECTURE. Only AMD64 (x64) is supported."
}

# 2. Resolve target release.
$apiUrl = if ($Version) {
    "https://api.github.com/repos/$Repo/releases/tags/$Version"
} elseif ($Channel -eq 'prerelease') {
    "https://api.github.com/repos/$Repo/releases"  # returns array; we'll pick [0]
} else {
    "https://api.github.com/repos/$Repo/releases/latest"
}

Step "Querying $apiUrl"
$release = Invoke-RestMethod -Uri $apiUrl -UseBasicParsing
if ($release -is [System.Array]) { $release = $release[0] }
$tag = $release.tag_name
Write-Host "Installing $tag"

# 3. Find the windows zip and sums asset.
$zipName  = 'triage-cli-x86_64-windows.zip'
$sumsName = 'SHA256SUMS'
$zipAsset  = $release.assets | Where-Object { $_.name -eq $zipName }  | Select-Object -First 1
$sumsAsset = $release.assets | Where-Object { $_.name -eq $sumsName } | Select-Object -First 1
if (-not $zipAsset)  { throw "Release $tag is missing asset: $zipName" }
if (-not $sumsAsset) { throw "Release $tag is missing asset: $sumsName" }

# 4. Download to a tempdir.
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) "triage-cli-install-$([System.Guid]::NewGuid())"
New-Item -ItemType Directory -Path $tmp | Out-Null
$zipPath  = Join-Path $tmp $zipName
$sumsPath = Join-Path $tmp $sumsName

Step "Downloading $zipName"
if (-not $DryRun) { Invoke-WebRequest -Uri $zipAsset.browser_download_url -OutFile $zipPath -UseBasicParsing }
Step "Downloading $sumsName"
if (-not $DryRun) { Invoke-WebRequest -Uri $sumsAsset.browser_download_url -OutFile $sumsPath -UseBasicParsing }

# 5. Verify SHA256.
if (-not $DryRun) {
    $actual   = (Get-FileHash $zipPath -Algorithm SHA256).Hash.ToLower()
    $expected = (Select-String -Path $sumsPath -Pattern "  $zipName$" | Select-Object -First 1).Line.Split(' ')[0].ToLower()
    if (-not $expected) { throw "SHA256SUMS did not contain a line for $zipName" }
    if ($actual -ne $expected) {
        throw "SHA256 mismatch for ${zipName}: expected $expected, got $actual"
    }
    Step "SHA256 verified: $expected"
}

# 6. Install: unpack into BinDir.
Step "Installing binary to $BinDir"
if (-not $DryRun) {
    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
    $unpack = Join-Path $tmp 'unpack'
    New-Item -ItemType Directory -Path $unpack | Out-Null
    Expand-Archive -Path $zipPath -DestinationPath $unpack -Force

    # 6a. Atomic binary swap (handles "exe is running" by renaming first).
    $exeDest = Join-Path $BinDir 'triage-cli.exe'
    $exeNew  = Join-Path $BinDir 'triage-cli.exe.new'
    $exeOld  = Join-Path $BinDir 'triage-cli.exe.old'
    Copy-Item (Join-Path $unpack 'triage-cli.exe') $exeNew -Force
    if (Test-Path $exeDest) {
        if (Test-Path $exeOld) { Remove-Item $exeOld -Force -ErrorAction SilentlyContinue }
        Rename-Item $exeDest $exeOld -ErrorAction SilentlyContinue
    }
    Rename-Item $exeNew $exeDest
    Remove-Item $exeOld -Force -ErrorAction SilentlyContinue  # best-effort
}

# 7. Seed data dir.
Step "Seeding data dir at $DataDir"
if (-not $DryRun) {
    New-Item -ItemType Directory -Path $DataDir -Force | Out-Null
    $invSrc = Join-Path $unpack 'apex-cnc-inventory.md'
    $invDst = Join-Path $DataDir 'apex-cnc-inventory.md'
    $invVer = Join-Path $DataDir '.inventory-version'
    if (-not (Test-Path $invDst)) {
        Copy-Item $invSrc $invDst
        (Get-FileHash $invDst -Algorithm SHA256).Hash.ToLower() | Set-Content $invVer
    } else {
        $shippedHash = (Get-FileHash $invSrc -Algorithm SHA256).Hash.ToLower()
        $previousHash = if (Test-Path $invVer) { (Get-Content $invVer).Trim().ToLower() } else { '' }
        $localHash    = (Get-FileHash $invDst -Algorithm SHA256).Hash.ToLower()
        if ($localHash -eq $previousHash) {
            # Analyst hasn't edited locally — safe to update.
            Copy-Item $invSrc $invDst -Force
            $shippedHash | Set-Content $invVer
        } else {
            # Analyst has hand-edited. Drop the new copy beside it.
            Copy-Item $invSrc "$invDst.new" -Force
            Write-Host "warning: existing apex-cnc-inventory.md has local edits; new copy saved as apex-cnc-inventory.md.new" -ForegroundColor Yellow
        }
    }

    # Seed .env.example if not already present.
    $envSrc = Join-Path $unpack '.env.example'
    $envDst = Join-Path $DataDir '.env.example'
    if ((Test-Path $envSrc) -and (-not (Test-Path $envDst))) {
        Copy-Item $envSrc $envDst
    }
}

# 8. PATH management.
$userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
if ($userPath -notlike "*$BinDir*") {
    Step "Adding $BinDir to user PATH"
    if (-not $DryRun) {
        $newPath = if ($userPath) { "$userPath;$BinDir" } else { $BinDir }
        [Environment]::SetEnvironmentVariable('PATH', $newPath, 'User')
    }
    Write-Host "note: Open a new terminal window for PATH changes to take effect." -ForegroundColor Yellow
}

# 9. Cleanup.
if (-not $DryRun) { Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue }

# 10. Final output.
Write-Host ""
Write-Host "triage-cli installed ($tag)." -ForegroundColor Green
Write-Host "Run: triage-cli setup    # to enter your Zendesk and provider credentials"
Write-Host "Run: triage-cli doctor   # to verify everything works"
