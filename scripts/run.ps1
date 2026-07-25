[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $Project,

    [string] $ListenHost = "127.0.0.1",

    [ValidateRange(1, 65535)]
    [int] $Port = 8787,

    [ValidateSet("powershell", "cmd")]
    [string] $Shell = "powershell",

    [ValidateNotNullOrEmpty()]
    [string] $Command = "codex",

    [string] $Token,

    [switch] $NoOpenBrowser
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$scriptsDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $scriptsDirectory ".."))
$projectPath = Resolve-Path -LiteralPath $Project -ErrorAction Stop

if (-not (Test-Path -LiteralPath $projectPath.Path -PathType Container)) {
    throw "Project is not a directory: $Project"
}

$executableName = if ($env:OS -eq "Windows_NT") {
    "codex-web.exe"
}
else {
    "codex-web"
}
$executableCandidates = @(
    (Join-Path $projectRoot "dist\$executableName"),
    (Join-Path $projectRoot "server\target\release\$executableName"),
    (Join-Path $projectRoot "server\target\debug\$executableName")
)
$executable = $executableCandidates |
    Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
    Select-Object -First 1

if (-not $executable) {
    throw "codex-web has not been built. Run .\scripts\build.ps1 first."
}

$arguments = @(
    "--project", $projectPath.Path,
    "--host", $ListenHost,
    "--port", $Port.ToString(),
    "--shell", $Shell,
    "--command", $Command
)

if ($Token) {
    $arguments += @("--token", $Token)
}
if ($NoOpenBrowser) {
    $arguments += "--no-open-browser"
}

& $executable @arguments
exit $LASTEXITCODE
