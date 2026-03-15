param(
    [string]$BaseUrl = "http://127.0.0.1:3000",
    [string]$BackendHost = "127.0.0.1",
    [int]$BackendPort = 9001,
    [string]$TriggerPath = "/api/health",
    [int]$StartupWaitSeconds = 20,
    [int]$IdleWaitSeconds = 10,
    [int]$ConfiguredIdleTimeoutSeconds = 120,
    [switch]$ExpectStopped
)

$ErrorActionPreference = "Stop"

function Test-TcpPortOpen {
    param(
        [Parameter(Mandatory = $true)]
        [string]$HostName,

        [Parameter(Mandatory = $true)]
        [int]$Port,

        [int]$TimeoutMs = 500
    )

    $client = $null

    try {
        $client = [System.Net.Sockets.TcpClient]::new()
        $connectTask = $client.ConnectAsync($HostName, $Port)

        if (-not $connectTask.Wait($TimeoutMs)) {
            return $false
        }

        return $client.Connected
    } catch {
        return $false
    } finally {
        if ($null -ne $client) {
            $client.Dispose()
        }
    }
}

function Get-PortSnapshot {
    param(
        [Parameter(Mandatory = $true)]
        [string]$HostName,

        [Parameter(Mandatory = $true)]
        [int]$Port
    )

    $isListening = Test-TcpPortOpen -HostName $HostName -Port $Port
    $owningProcessId = $null
    $processName = $null

    try {
        $connection = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction Stop |
            Select-Object -First 1

        if ($null -ne $connection) {
            $owningProcessId = $connection.OwningProcess
            $process = Get-Process -Id $owningProcessId -ErrorAction SilentlyContinue
            if ($null -ne $process) {
                $processName = $process.ProcessName
            }
        }
    } catch {
    }

    [pscustomobject]@{
        IsListening = $isListening
        ProcessId = $owningProcessId
        ProcessName = $processName
    }
}

function Show-PortSnapshot {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Label,

        [Parameter(Mandatory = $true)]
        [psobject]$Snapshot
    )

    Write-Host ""
    Write-Host "[$Label]" -ForegroundColor Cyan
    Write-Host "Listening: $($Snapshot.IsListening)"

    if ($null -ne $Snapshot.ProcessId) {
        Write-Host "PID: $($Snapshot.ProcessId)"
    }

    if ($null -ne $Snapshot.ProcessName) {
        Write-Host "Process: $($Snapshot.ProcessName)"
    }
}

function Invoke-CurlRequest {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$CurlArgs
    )

    $responseLines = @(& curl.exe --silent --show-error --output - --write-out "`n__STATUS__:%{http_code}" @CurlArgs)
    $exitCode = $LASTEXITCODE
    $response = $responseLines -join "`n"

    if ($exitCode -ne 0) {
        return [pscustomobject]@{
            Success = $false
            ExitCode = $exitCode
            StatusCode = $null
            Body = $response
            Error = "curl.exe exited with code $exitCode"
        }
    }

    $statusMarker = "__STATUS__:"
    $markerIndex = $response.LastIndexOf($statusMarker)
    if ($markerIndex -lt 0) {
        return [pscustomobject]@{
            Success = $false
            ExitCode = 0
            StatusCode = $null
            Body = $response
            Error = "Could not parse HTTP status from curl response"
        }
    }

    $body = $response.Substring(0, $markerIndex).TrimEnd("`r", "`n")
    $statusCode = [int]$response.Substring($markerIndex + $statusMarker.Length).Trim()

    [pscustomobject]@{
        Success = $true
        ExitCode = 0
        StatusCode = $statusCode
        Body = $body
        Error = $null
    }
}

function Wait-ForPortState {
    param(
        [Parameter(Mandatory = $true)]
        [string]$HostName,

        [Parameter(Mandatory = $true)]
        [int]$Port,

        [Parameter(Mandatory = $true)]
        [bool]$TargetState,

        [Parameter(Mandatory = $true)]
        [int]$TimeoutSeconds
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)

    while ((Get-Date) -lt $deadline) {
        $isOpen = Test-TcpPortOpen -HostName $HostName -Port $Port
        if ($isOpen -eq $TargetState) {
            return $true
        }

        Start-Sleep -Milliseconds 250
    }

    return $false
}

$baseUri = [uri]$BaseUrl
$triggerUrl = $BaseUrl.TrimEnd('/') + $TriggerPath
$shouldExpectStopped = $ExpectStopped.IsPresent -or ($IdleWaitSeconds -ge $ConfiguredIdleTimeoutSeconds)

Write-Host "Activator: $BaseUrl"
Write-Host "Backend: $BackendHost`:$BackendPort"
Write-Host "Trigger: $triggerUrl"
Write-Host "Idle wait: $IdleWaitSeconds second(s)"
Write-Host "Configured idle timeout: $ConfiguredIdleTimeoutSeconds second(s)"

if (-not $shouldExpectStopped) {
    Write-Host "Idle wait is shorter than the configured timeout, so shutdown is not expected yet." -ForegroundColor Yellow
}

$health = Invoke-CurlRequest -CurlArgs @("$BaseUrl/health")
if (-not $health.Success) {
    Write-Host ""
    Write-Host "Activator is not running or not reachable at $BaseUrl." -ForegroundColor Yellow
    Write-Host $health.Error
    exit 1
}

if ($health.StatusCode -ne 200) {
    Write-Host ""
    Write-Host "Activator health check failed with HTTP $($health.StatusCode)." -ForegroundColor Red
    if ($health.Body.Length -gt 0) {
        Write-Host "Body:"
        Write-Host $health.Body
    }
    exit 1
}

Write-Host ""
Write-Host "[Activator health]" -ForegroundColor Cyan
Write-Host "Status: $($health.StatusCode)"
Write-Host "Body:"
Write-Host $health.Body

$before = Get-PortSnapshot -HostName $BackendHost -Port $BackendPort
Show-PortSnapshot -Label "Before trigger" -Snapshot $before

if ($before.IsListening) {
    Write-Host "Backend was already listening before the trigger request." -ForegroundColor Yellow
    Write-Host "This is a warm-start check, not a cold-start check."
}

Write-Host ""
Write-Host "[Trigger request]" -ForegroundColor Cyan
Write-Host "curl.exe $triggerUrl"
$trigger = Invoke-CurlRequest -CurlArgs @("$triggerUrl")
if (-not $trigger.Success) {
    Write-Host "FAILED: could not reach trigger URL." -ForegroundColor Red
    Write-Host $trigger.Error
    exit 1
}

Write-Host "Status: $($trigger.StatusCode)"
if ($trigger.Body.Length -gt 0) {
    Write-Host "Body:"
    Write-Host $trigger.Body
}

if ($trigger.StatusCode -ne 200) {
    Write-Host "FAILED: trigger request returned HTTP $($trigger.StatusCode)." -ForegroundColor Red
    exit 1
}

$started = Wait-ForPortState -HostName $BackendHost -Port $BackendPort -TargetState $true -TimeoutSeconds $StartupWaitSeconds
if (-not $started) {
    Write-Host "FAILED: backend port $BackendPort did not open within $StartupWaitSeconds second(s)." -ForegroundColor Red
    exit 1
}

$afterStart = Get-PortSnapshot -HostName $BackendHost -Port $BackendPort
Show-PortSnapshot -Label "After startup" -Snapshot $afterStart

Write-Host ""
Write-Host "Waiting $IdleWaitSeconds second(s) before checking for shutdown..."
Start-Sleep -Seconds $IdleWaitSeconds

$afterWait = Get-PortSnapshot -HostName $BackendHost -Port $BackendPort
Show-PortSnapshot -Label "After idle wait" -Snapshot $afterWait

Write-Host ""
if ($shouldExpectStopped) {
    if ($afterWait.IsListening) {
        Write-Host "FAIL: backend is still listening after the idle wait." -ForegroundColor Red
        exit 1
    }

    Write-Host "PASS: backend shut down after the idle wait." -ForegroundColor Green
    exit 0
}

if ($afterWait.IsListening) {
    Write-Host "Backend is still running after the idle wait." -ForegroundColor Yellow
    Write-Host "Idle shutdown is implemented, but this run did not wait long enough to expect shutdown."
    exit 0
}

Write-Host "Backend is no longer listening after the idle wait." -ForegroundColor Green
Write-Host "If that was unexpected, the process may have exited or been stopped manually."
