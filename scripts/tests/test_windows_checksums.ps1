# Offline regression tests for the real install.ps1 checksum block.
# Run: pwsh -NoProfile -File scripts/tests/test_windows_checksums.ps1
param(
    [string]$InstallerPath = (Join-Path $PSScriptRoot "../../install.ps1")
)

$ErrorActionPreference = "Stop"
$Source = Get-Content -LiteralPath $InstallerPath -Raw
$Start = $Source.IndexOf("# Verify before extracting")
$End = $Source.IndexOf("# Extract", $Start)
if ($Start -lt 0 -or $End -le $Start) { throw "Checksum verification block not found" }
$Verification = [scriptblock]::Create($Source.Substring($Start, $End - $Start))

# GitHub serves SHA256SUMS as application/octet-stream. PowerShell 7 exposes
# the response Content as byte[], while OutFile preserves the original bytes.
function Invoke-WebRequest {
    param([string]$Uri, [string]$OutFile, [switch]$UseBasicParsing)
    if ($OutFile) {
        [IO.File]::WriteAllBytes($OutFile, $script:ManifestBytes)
    } else {
        [pscustomobject]@{ Content = $script:ManifestBytes }
    }
}

$Root = Join-Path ([IO.Path]::GetTempPath()) ("notmux-checksum-test-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $Root | Out-Null
try {
    $Payload = Join-Path $Root "payload.zip"
    [IO.File]::WriteAllText($Payload, "test archive bytes")
    $Hash = (Get-FileHash -LiteralPath $Payload -Algorithm SHA256).Hash.ToLowerInvariant()
    $Artifact = "notmux-windows-x64"
    $Repo = "notagentdev/notmux"
    $Version = "0.1.0"
    $Valid = "$Hash  $Artifact.zip`n"
    $Cases = @(
        @{ Name = "binary HTTP content"; Text = $Valid; Error = $null },
        @{ Name = "CRLF manifest"; Text = "$Hash  $Artifact.zip`r`n"; Error = $null },
        @{ Name = "UTF-8 BOM"; Text = ([char]0xFEFF).ToString() + $Valid; Error = $null },
        @{ Name = "missing entry"; Text = "$Hash  another.zip`n"; Error = "Missing or ambiguous" },
        @{ Name = "duplicate entry"; Text = $Valid + $Valid; Error = "Missing or ambiguous" },
        @{ Name = "malformed hash"; Text = "invalid  $Artifact.zip`n"; Error = "Missing or ambiguous" },
        @{ Name = "wrong hash"; Text = ("0" * 64) + "  $Artifact.zip`n"; Error = "Checksum verification failed" }
    )
    foreach ($Case in $Cases) {
        $TempDir = Join-Path $Root ([guid]::NewGuid().ToString())
        New-Item -ItemType Directory -Path $TempDir | Out-Null
        $ZipPath = Join-Path $TempDir "notmux.zip"
        Copy-Item -LiteralPath $Payload -Destination $ZipPath
        $script:ManifestBytes = [Text.Encoding]::UTF8.GetBytes($Case.Text)
        $Failure = $null
        try { & $Verification } catch { $Failure = $_.Exception.Message }
        if ($null -eq $Case.Error) {
            if ($null -ne $Failure) { throw "$($Case.Name): unexpected failure: $Failure" }
        } elseif ($null -eq $Failure -or $Failure -notlike "*$($Case.Error)*") {
            throw "$($Case.Name): expected '$($Case.Error)', got '$Failure'"
        }
        Write-Host "PASS: $($Case.Name)"
    }
} finally {
    Remove-Item -LiteralPath $Root -Recurse -Force
}
