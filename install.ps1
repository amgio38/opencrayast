#Requires -Version 5.1
<#
.SYNOPSIS
    Install opencrayast and opencrayast-mcp.

.DESCRIPTION
    The Windows counterpart of install.sh, and it enforces the same contract in the same
    order. That order is the point, not an implementation detail:

      1. both install paths must be LISTED in SHA256SUMS   (a truncated sums file must not
         let an unchecked binary through — REL1-09)
      2. every listed digest must VERIFY                    (before PREFIX is touched at all)
      3. both binaries must EXIST
      4. only then is anything copied

    Three paths, tried in this order, matching install.sh:

      1. a prebuilt GitHub release asset, verified against its published SHA-256;
      2. a local DistDir artefact tree, verified against its SHA256SUMS;
      3. a cargo build, from this checkout or a fresh clone.

    Path 1 finds nothing until the project has a tagged release, and then it succeeds with no
    edit to this script. That is why -FromRelease is no longer a refusal: the download path is
    tried first and simply reports that there is nothing there yet.

    Does not require administrator rights: it writes only under the chosen prefix. On Windows
    the default prefix is $env:LOCALAPPDATA\opencrayast, because there is no $HOME and
    %USERPROFILE%\.local is not on PATH.

    What works on Windows today: reading tools and edits via temp+rename (directory
    fsync confirms the path is still a directory; see docs/ARCHITECTURE.md Platform notes).

.PARAMETER Prefix
    Install directory. Binaries go into PREFIX\bin. Default: $env:LOCALAPPDATA\opencrayast

.PARAMETER DistDir
    Artefact tree. Default: .\dist

.PARAMETER Target
    Subdirectory under DistDir, and the prebuilt asset's platform triple.
    Default: x86_64-pc-windows-msvc

.PARAMETER Version
    Release tag to fetch. Default: the newest release.

.PARAMETER FromSource
    Skip the release asset and the artefact tree; always build with cargo.

.PARAMETER Insecure
    Install a release asset with no published checksum. Off by default: the installer stops
    rather than install an unverified binary.

.PARAMETER Repo
    owner/name to fetch the release from or clone. Default: amgio38/opencrayast

.EXAMPLE
    .\install.ps1 -DistDir .\dist -Target x86_64-pc-windows-msvc

.EXAMPLE
    irm https://raw.githubusercontent.com/amgio38/opencrayast/main/install.ps1 | iex

.NOTES
    Requires PowerShell 5.1 or later. No elevation is required.
#>

[CmdletBinding()]
param(
    [string] $Prefix,
    [string] $DistDir = '.\dist',
    [string] $Target = 'x86_64-pc-windows-msvc',
    [string] $Version = $env:OPENCRAYAST_VERSION,
    [switch] $FromSource,
    [switch] $Insecure,
    [string] $Repo = $(if ($env:OPENCRAYAST_REPO) { $env:OPENCRAYAST_REPO } else { 'amgio38/opencrayast' })
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Where the licence files go under $Prefix. A binary in bin\ is a redistribution, and the
# licences of what is bundled in it have to travel with it rather than being left in the
# repository where a person who only has the binary cannot read them.
$DocSubdir = 'share\doc\opencrayast'

function Fail {
    param([string] $Message, [string] $Next, [int] $Code = 1)
    Write-Error "install.ps1: $Message"
    Write-Error "install.ps1: $Next"
    exit $Code
}

function Note { param([string] $Message) Write-Host "install.ps1: $Message" }

# ---------------------------------------------------------------------------
# Release path: a prebuilt asset from the newest (or a named) GitHub release.
# Returns $false when there is nothing to install, so the caller falls through.
# ---------------------------------------------------------------------------

function Get-ReleaseAsset {
    if ($FromSource) { return $false }

    $base = if ($Version) {
        "https://github.com/$Repo/releases/download/$Version"
    } else {
        "https://github.com/$Repo/releases/latest/download"
    }
    $asset = "opencrayast-$Target.zip"

    Note "fetching $base/$asset"
    try {
        # -UseBasicParsing keeps this working on Windows PowerShell 5.1, where the default
        # parser shells out to IE and fails on a machine with IE disabled.
        $progress = $ProgressPreference
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri "$base/$asset" -OutFile $script:DownloadedAsset -UseBasicParsing
        $ProgressPreference = $progress
    } catch {
        Note "no release asset there; falling through"
        return $false
    }

    if (-not (Test-Path -LiteralPath $script:DownloadedAsset -PathType Leaf)) {
        Note "no release asset there; falling through"
        return $false
    }
    return $true
}

function Test-ReleaseChecksum {
    $base = if ($Version) {
        "https://github.com/$Repo/releases/download/$Version"
    } else {
        "https://github.com/$Repo/releases/latest/download"
    }
    $asset = "opencrayast-$Target.zip"

    $want = $null
    try {
        $progress = $ProgressPreference
        $ProgressPreference = 'SilentlyContinue'
        $text = (Invoke-WebRequest -Uri "$base/$asset.sha256" -UseBasicParsing).Content
        $ProgressPreference = $progress
        if ($text -match '([0-9a-fA-F]{64})') { $want = $Matches[1].ToLowerInvariant() }
    } catch {
        $want = $null
    }

    if (-not $want) {
        if (-not $Insecure) {
            # Fail (not Write-Error): $ErrorActionPreference is Stop, so Write-Error
            # would terminate before DistDir/source fall-through could run. A downloaded
            # asset without a checksum is a hard refuse, not a soft miss.
            Fail "no published SHA-256 for $asset; refusing to install an unverified binary." `
                 'pass -Insecure to accept it anyway.'
        }
        Note "-Insecure: installing $asset with no checksum"
        return $true
    }

    $actual = (Get-FileHash -LiteralPath $script:DownloadedAsset -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $want) {
        Fail "checksum mismatch for $asset (expected $want, got $actual); nothing installed" `
             'nothing installed.'
    }
    Note "checksum verified for $asset"
    return $true
}

function Install-FromRelease {
    if (-not (Get-ReleaseAsset)) { return $false }
    if (-not (Test-ReleaseChecksum)) { return $false }

    $unpack = Join-Path $script:Work 'release'
    New-Item -ItemType Directory -Path $unpack -Force | Out-Null
    # The archive carries the two programs and, under one directory, the licence files. The
    # two programs are an exact allowlist: anything else at the top level is refused.
    try {
        Expand-Archive -LiteralPath $script:DownloadedAsset -DestinationPath $unpack -Force
    } catch {
        Note "release asset could not be unpacked; falling through ($($_.Exception.Message))"
        return $false
    }

    $programs = @(Get-ChildItem -LiteralPath $unpack -File |
                   Where-Object { $_.Name -in @('opencrayast.exe', 'opencrayast-mcp.exe') })
    $unexpected = @(Get-ChildItem -LiteralPath $unpack |
                    Where-Object { $_.PSIsContainer -and $_.Name -ne 'share' })
    if ($unexpected.Count -gt 0) {
        Fail "unexpected member in the release asset: $($unexpected[0].Name)" 'nothing installed.'
    }
    if ($programs.Count -ne 2) {
        Fail "release asset did not contain opencrayast.exe and opencrayast-mcp.exe" 'nothing installed.'
    }

    $destBin = Join-Path $Prefix 'bin'
    New-Item -ItemType Directory -Path $destBin -Force | Out-Null
    # Staged first, copied after every check, so a failure leaves the previous install alone.
    foreach ($name in @('opencrayast.exe', 'opencrayast-mcp.exe')) {
        Copy-Item -LiteralPath (Join-Path $unpack $name) -Destination (Join-Path $destBin $name) -Force
    }
    Write-Output "install.ps1: installed opencrayast and opencrayast-mcp into $destBin"

    Install-DocFiles $unpack
    # A script-scope flag, not a return value. `Write-Output` above is pipeline output, and a
    # function's return value in PowerShell is ALL of its pipeline output — so `$x = Fn` would
    # capture an array of the message and the boolean, and any comparison against $true would be
    # false for a function that succeeded. The flag sidesteps that entirely.
    $script:Handled = $true
}

# ---------------------------------------------------------------------------
# Artefact path: install from a local DistDir tree after verifying SHA256SUMS.
# ---------------------------------------------------------------------------

function Install-FromDist {
    # UTF-8, BOM-or-not: read the sums file as bytes and parse the text ourselves. A sums
    # file with a BOM would otherwise produce a first field of "<BOM>deadbeef" and fail the
    # digest check for a reason that has nothing to do with tampering — a false refusal,
    # which is the other half of the same bug class this script exists to avoid.
    $sumsPath = Join-Path $DistDir 'SHA256SUMS'
    if (-not (Test-Path -LiteralPath $sumsPath -PathType Leaf)) {
        Fail "missing $sumsPath" 'build the artefact tree first, or run without -DistDir to build from source.'
    }

    $sumsText = [System.IO.File]::ReadAllText($sumsPath)
    if ($sumsText.Length -gt 0 -and $sumsText[0] -eq [char]0xFEFF) {
        $sumsText = $sumsText.Substring(1)
    }

    # Every install path, relative to DistDir. These two are the WHITELIST: nothing else in
    # the tree is ever copied, whatever SHA256SUMS happens to list.
    $relCli = "$Target/opencrayast.exe"
    $relMcp = "$Target/opencrayast-mcp.exe"
    $required = @($relCli, $relMcp)

    # Parse: "<64 hex> <space|*> <path>". Anything else is a malformed sums file, not a
    # pass — a line we cannot read is a file we cannot verify.
    $listed = @{}
    foreach ($line in ($sumsText -split "`r?`n")) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        if ($line -match '^\s*([0-9a-fA-F]{64})\s+\*?(.+?)\s*$') {
            $listed[$Matches[2].Replace('\', '/')] = $Matches[1].ToLowerInvariant()
        }
        elseif (-not [string]::IsNullOrWhiteSpace($line)) {
            Fail "malformed line in SHA256SUMS: $line" 'the sums file must be produced by sha256sum; nothing installed.'
        }
    }

    # Step 1: coverage. Both required paths listed, before anything is verified or copied.
    foreach ($rel in $required) {
        if (-not $listed.ContainsKey($rel)) {
            Fail "$rel is not listed in SHA256SUMS; refusing to install" "nothing installed under $Prefix"
        }
    }

    # Step 2: verify every listed entry, not only the two we will copy. A corrupt file
    # elsewhere in the tree means the tree is not the tree that was signed off, so it is
    # refused as a whole.
    foreach ($rel in $listed.Keys) {
        $full = Join-Path $DistDir ($rel.Replace('/', [System.IO.Path]::DirectorySeparatorChar))
        if (-not (Test-Path -LiteralPath $full -PathType Leaf)) {
            Fail "SHA256SUMS lists $rel but it is not in the tree" 'the sums file and the tree disagree; nothing installed.'
        }
        $actual = (Get-FileHash -LiteralPath $full -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $listed[$rel]) {
            Fail "checksum mismatch for $rel" "nothing installed under $Prefix"
        }
    }

    # Step 3: both binaries exist (coverage above proves they are listed; this proves they
    # are files).
    $binDir = Join-Path $DistDir $Target
    foreach ($name in @('opencrayast.exe', 'opencrayast-mcp.exe')) {
        $p = Join-Path $binDir $name
        if (-not (Test-Path -LiteralPath $p -PathType Leaf)) {
            Fail "missing binary: $p" 'build the artefact tree first; nothing installed.'
        }
    }

    # Step 4: copy. Only now, after coverage AND digests AND existence.
    $destBin = Join-Path $Prefix 'bin'
    New-Item -ItemType Directory -Path $destBin -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $binDir 'opencrayast.exe') -Destination (Join-Path $destBin 'opencrayast.exe') -Force
    Copy-Item -LiteralPath (Join-Path $binDir 'opencrayast-mcp.exe') -Destination (Join-Path $destBin 'opencrayast-mcp.exe') -Force

    Write-Output "install.ps1: installed opencrayast and opencrayast-mcp into $destBin"
    Install-DocFiles $DistDir
    $script:Handled = $true
}

# ---------------------------------------------------------------------------
# Build path: what `irm | iex` uses while there is no tagged release.
# ---------------------------------------------------------------------------

function Install-FromSource {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Fail 'cargo is not installed.' 'Install a Rust toolchain (https://rustup.rs) and retry; nothing installed.'
    }

    $repo = $PWD.Path
    if (-not (Test-Path -LiteralPath (Join-Path $repo 'Cargo.toml')) -or
        -not (Test-Path -LiteralPath (Join-Path $repo 'crates\cli'))) {
        if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
            Fail 'not in a checkout and git is not installed' 'nothing installed.'
        }
        $repo = Join-Path $script:Work 'src'
        Note "cloning https://github.com/$Repo"
        & git clone --depth 1 "https://github.com/$Repo" $repo 2>&1 | Out-Host
        if ($LASTEXITCODE -ne 0) { Fail "cannot clone https://github.com/$Repo" 'nothing installed.' }
    }

    Note 'building with cargo (this can take a few minutes)'
    $buildRoot = Join-Path $script:Work 'cargo-root'
    New-Item -ItemType Directory -Path $buildRoot -Force | Out-Null
    # Build into a private root first: the pair is checked and then installed, and no cargo
    # bookkeeping is left in $Prefix.
    & cargo install --quiet --locked --path (Join-Path $repo 'crates\cli') --root $buildRoot
    if ($LASTEXITCODE -ne 0) { Fail 'cargo failed to build the CLI' 'nothing installed.' }
    & cargo install --quiet --locked --path (Join-Path $repo 'crates\mcp') --root $buildRoot
    if ($LASTEXITCODE -ne 0) { Fail 'cargo failed to build the MCP server' 'nothing installed.' }

    $builtBin = Join-Path $buildRoot 'bin'
    foreach ($name in @('opencrayast.exe', 'opencrayast-mcp.exe')) {
        if (-not (Test-Path -LiteralPath (Join-Path $builtBin $name) -PathType Leaf)) {
            Fail "cargo did not produce $name" 'nothing installed.'
        }
    }

    $destBin = Join-Path $Prefix 'bin'
    New-Item -ItemType Directory -Path $destBin -Force | Out-Null
    foreach ($name in @('opencrayast.exe', 'opencrayast-mcp.exe')) {
        Copy-Item -LiteralPath (Join-Path $builtBin $name) -Destination (Join-Path $destBin $name) -Force
    }
    Write-Output "install.ps1: installed opencrayast and opencrayast-mcp into $destBin"

    Install-DocFiles $repo
    $script:Handled = $true
}

# Put the licence files where a person who only has the installed binaries can read them: a
# binary that statically links five tree-sitter grammars plus the Rust dependency tree
# redistributes licensed work, and the licences require the notice to travel with it.
function Install-DocFiles {
    param([string] $Source)
    $docDir = Join-Path $Prefix $DocSubdir
    New-Item -ItemType Directory -Path $docDir -Force | Out-Null
    foreach ($f in @('LICENSE', 'THIRD-PARTY-LICENSES.md')) {
        $p = Join-Path $Source $f
        if (Test-Path -LiteralPath $p -PathType Leaf) {
            Copy-Item -LiteralPath $p -Destination (Join-Path $docDir $f) -Force
        }
    }
    Write-Output "install.ps1: licence files installed into $docDir"
}

# ---------------------------------------------------------------------------

if (-not $Prefix) {
    $base = $env:LOCALAPPDATA
    if ([string]::IsNullOrWhiteSpace($base)) {
        Fail 'Prefix is unset and LOCALAPPDATA is unset.' 'pass -Prefix DIR; nothing installed.'
    }
    $Prefix = Join-Path $base 'opencrayast'
}

$script:Work = Join-Path ([System.IO.Path]::GetTempPath()) ("opencrayast-install-" + [System.Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $script:Work -Force | Out-Null
$script:DownloadedAsset = Join-Path $script:Work 'asset.zip'

# A script-scope flag rather than the functions' return values: see the note in
# Install-FromRelease for why a return value cannot carry this.
$script:Handled = $false
try {
    # 1. a prebuilt release asset. Does nothing (and leaves the flag unset) until a tagged
    #    release exists, and then it succeeds with no edit to this script.
    Install-FromRelease
    # 2. a local artefact tree, when the caller has one. Called directly, NOT piped: `| Out-Null`
    #    would run it in a child scope, where its `exit` ends that scope rather than the script,
    #    and a refusal would be read as "this path did not apply".
    if (-not $script:Handled -and -not $FromSource -and (Test-Path -LiteralPath $DistDir -PathType Container)) {
        Install-FromDist
    }
    # 3. a cargo build. Say what is happening: the next thing is a multi-minute build and
    #    silence would read as a hang.
    if (-not $script:Handled) {
        if (-not (Test-Path -LiteralPath $DistDir -PathType Container)) {
            Note 'no release asset and no artefact tree; building from source.'
            Note 'this needs a Rust toolchain (https://rustup.rs) and the MSVC build tools.'
        }
        Install-FromSource
    }
}
finally {
    if (Test-Path -LiteralPath $script:Work) {
        Remove-Item -LiteralPath $script:Work -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if (-not $script:Handled) { Fail 'nothing was installed' 'see the messages above.' }

Write-Output ''
Write-Output "Add $(Join-Path $Prefix 'bin') to your PATH."
Write-Output ''
Write-Output 'Register the MCP server with your agent:'
Write-Output '    claude mcp add opencrayast -- opencrayast-mcp --workspace .'
Write-Output ''
Write-Output 'Check the installation:'
Write-Output '    opencrayast doctor'
Write-Output ''
Write-Output 'On Windows every edit is refused: ast_edit_apply, ast_undo and ast_recover'
Write-Output 'need the atomic-replace primitive, which is not ported yet. The reading'
Write-Output 'tools work.'
exit 0