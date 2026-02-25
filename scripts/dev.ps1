$ErrorActionPreference = "Stop"

$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$webDir = Join-Path $root "web"

$rustProc = $null
$nextProc = $null
$cleanupDone = $false

function Load-DotEnv {
    param([string]$Path)

    if (-not (Test-Path $Path)) {
        return
    }

    foreach ($rawLine in Get-Content -Path $Path) {
        $line = $rawLine.Trim()
        if ([string]::IsNullOrWhiteSpace($line) -or $line.StartsWith("#")) {
            continue
        }

        $separator = $line.IndexOf("=")
        if ($separator -le 0) {
            continue
        }

        $key = $line.Substring(0, $separator).Trim()
        $value = $line.Substring($separator + 1).Trim()

        if (($value.StartsWith('"') -and $value.EndsWith('"')) -or ($value.StartsWith("'") -and $value.EndsWith("'"))) {
            $value = $value.Substring(1, $value.Length - 2)
        }

        [Environment]::SetEnvironmentVariable($key, $value, "Process")
    }

    Write-Host "[dev] Loaded .env"
}

function Stop-Servers {
    if ($script:cleanupDone) {
        return
    }

    $script:cleanupDone = $true
    Write-Host "Stopping servers..."

    foreach ($proc in @($script:rustProc, $script:nextProc)) {
        if ($null -eq $proc) {
            continue
        }

        try {
            if (-not $proc.HasExited) {
                Stop-Process -Id $proc.Id -Force -ErrorAction Stop
            }
        } catch {
            # Ignore process cleanup failures
        }
    }
}

try {
    Load-DotEnv -Path (Join-Path $root ".env")

    Write-Host "[dev] Starting Rust backend on :8080"
    $rustProc = Start-Process -FilePath "cargo" -ArgumentList @("run", "--", "serve", "--host", "127.0.0.1", "--port", "8080") -WorkingDirectory $root -PassThru -NoNewWindow

    Write-Host "[dev] Starting Next.js frontend on :3000"
    if (-not (Test-Path (Join-Path $webDir "node_modules"))) {
        Write-Host "[dev] Installing npm dependencies..."
        & npm install --prefix $webDir
    }

    # npm on Windows is typically a .cmd shim, so execute through cmd.exe.
    $nextProc = Start-Process -FilePath "cmd.exe" -ArgumentList @("/d", "/c", "npm run dev") -WorkingDirectory $webDir -PassThru -NoNewWindow

    Write-Host ""
    Write-Host "  Backend:  http://localhost:8080"
    Write-Host "  Frontend: http://localhost:3000"
    Write-Host ""

    Wait-Process -Id @($rustProc.Id, $nextProc.Id)
} finally {
    Stop-Servers
}
