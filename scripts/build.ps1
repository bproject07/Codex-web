[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-Command {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Name,
        [Parameter(Mandatory = $true)]
        [string] $InstallHint
    )

    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "$Name was not found. $InstallHint"
    }
}

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)]
        [scriptblock] $Command,
        [Parameter(Mandatory = $true)]
        [string] $FailureMessage
    )

    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$FailureMessage (exit code $LASTEXITCODE)"
    }
}

$scriptsDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $scriptsDirectory ".."))
$webDirectory = Join-Path $projectRoot "web"
$serverDirectory = Join-Path $projectRoot "server"
$distributionDirectory = [System.IO.Path]::GetFullPath((Join-Path $projectRoot "dist"))
$expectedDistributionDirectory = [System.IO.Path]::GetFullPath(
    (Join-Path $projectRoot "dist")
)

if ($distributionDirectory -ne $expectedDistributionDirectory) {
    throw "Refusing to use an unexpected distribution directory."
}

Assert-Command -Name "node" -InstallHint "Install Node.js ^20.19.0 or >=22.12.0."
Assert-Command -Name "npm" -InstallHint "Install npm with Node.js."
Assert-Command -Name "cargo" -InstallHint "Install stable Rust from https://rustup.rs/."
Assert-Command -Name "rustc" -InstallHint "Install stable Rust from https://rustup.rs/."

Invoke-Checked `
    -Command {
        node -e @"
const [major, minor] = process.versions.node.split(".").map(Number);
const supported =
  (major === 20 && minor >= 19) ||
  (major === 22 && minor >= 12) ||
  major > 22;
if (!supported) process.exit(1);
"@
    } `
    -FailureMessage "Unsupported Node.js version; use ^20.19.0 or >=22.12.0"

Write-Host "Node.js: $(node --version)"
Write-Host "npm:     $(npm --version)"
Write-Host "Rust:    $(rustc --version)"
Write-Host

Push-Location $webDirectory
try {
    if (Test-Path -LiteralPath (Join-Path $webDirectory "package-lock.json")) {
        Invoke-Checked -Command { npm ci } -FailureMessage "npm ci failed"
    }
    else {
        Invoke-Checked -Command { npm install } -FailureMessage "npm install failed"
    }
    Invoke-Checked -Command { npm run build } -FailureMessage "Frontend build failed"
}
finally {
    Pop-Location
}

$rustHostLine = rustc -vV | Where-Object { $_ -like "host:*" } | Select-Object -First 1
$rustHost = if ($rustHostLine) { ($rustHostLine -split ":", 2)[1].Trim() } else { "" }
$gnuToolchain = $null

if ($rustHost.EndsWith("-msvc") -and -not (Get-Command "link.exe" -ErrorAction SilentlyContinue)) {
    if ((Get-Command "rustup" -ErrorAction SilentlyContinue) -and
        (Get-Command "gcc.exe" -ErrorAction SilentlyContinue)) {
        $gnuToolchainLine = rustup toolchain list |
            Where-Object { $_ -match "x86_64-pc-windows-gnu" } |
            Select-Object -First 1
        if ($gnuToolchainLine) {
            $gnuToolchain = ($gnuToolchainLine -split "\s+")[0]
            Write-Warning "MSVC link.exe is not active; building with $gnuToolchain and MinGW."
        }
    }

    if (-not $gnuToolchain) {
        throw @"
The active Rust toolchain targets MSVC, but link.exe is unavailable.
Install "Desktop development with C++" in Visual Studio Build Tools and run this
script from Developer PowerShell, or install a GNU Rust toolchain plus MinGW.
"@
    }
}

Push-Location $serverDirectory
try {
    if ($gnuToolchain) {
        Invoke-Checked `
            -Command { rustup run $gnuToolchain cargo build --release --locked } `
            -FailureMessage "Rust release build failed"
    }
    else {
        Invoke-Checked `
            -Command { cargo build --release --locked } `
            -FailureMessage "Rust release build failed"
    }
}
finally {
    Pop-Location
}

$executableName = if ($env:OS -eq "Windows_NT") {
    "codex-web.exe"
}
else {
    "codex-web"
}
$builtExecutable = Join-Path $serverDirectory "target\release\$executableName"
$frontendBuild = Join-Path $webDirectory "dist"

if (-not (Test-Path -LiteralPath $builtExecutable -PathType Leaf)) {
    throw "Release executable was not created at $builtExecutable"
}
if (-not (Test-Path -LiteralPath (Join-Path $frontendBuild "index.html") -PathType Leaf)) {
    throw "Frontend assets were not created at $frontendBuild"
}

if (Test-Path -LiteralPath $distributionDirectory) {
    $distributionItem = Get-Item -LiteralPath $distributionDirectory -Force
    if (($distributionItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Refusing to replace a reparse-point distribution directory."
    }
    try {
        Remove-Item -LiteralPath $distributionDirectory -Recurse -Force
    }
    catch {
        throw @"
The distribution directory could not be recreated:
$distributionDirectory

Stop any process using files from that directory, then run the build again.
Original error: $($_.Exception.Message)
"@
    }
}
New-Item -ItemType Directory -Path $distributionDirectory | Out-Null
$packagedWebDirectory = New-Item `
    -ItemType Directory `
    -Path (Join-Path $distributionDirectory "web")

Copy-Item -LiteralPath $builtExecutable -Destination (
    Join-Path $distributionDirectory $executableName
)
Copy-Item -Path (Join-Path $frontendBuild "*") `
    -Destination $packagedWebDirectory.FullName `
    -Recurse `
    -Force
Copy-Item -LiteralPath (Join-Path $projectRoot "README.md") `
    -Destination $distributionDirectory
Copy-Item -LiteralPath (Join-Path $projectRoot "BUILDING.md") `
    -Destination $distributionDirectory
Copy-Item -LiteralPath (Join-Path $projectRoot "OPERATIONS.md") `
    -Destination $distributionDirectory
Copy-Item -LiteralPath (Join-Path $projectRoot "AGENTS.md") `
    -Destination $distributionDirectory
Copy-Item -LiteralPath (Join-Path $projectRoot "TODO.md") `
    -Destination $distributionDirectory
Copy-Item -LiteralPath (Join-Path $projectRoot "LICENSE") `
    -Destination $distributionDirectory

Write-Host
Write-Host "Build complete." -ForegroundColor Green
Write-Host "Executable: $(Join-Path $distributionDirectory $executableName)"
Write-Host "Run with:"
Write-Host ".\scripts\run.ps1 -Project `"C:\Projects\my-app`""
