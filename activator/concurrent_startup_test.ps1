#Requires -Version 7.0

param(
    [string]$BaseUrl = "http://127.0.0.1:3000",
    [string]$BackendHost = "127.0.0.1",
    [int]$BackendPort = 9001,
    [int]$RequestCount = 10
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

function Invoke-CurlRequest {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$CurlArgs
    )

    $responseLines = @(& curl.exe --silent --show-error --output - --write-out "`n__STATUS__:%{http_code}" @CurlArgs)
    $exitCode = $LASTEXITCODE
    $response = $responseLines -join "`n"

    if ($exitCode -ne 0) {
        throw "curl.exe exited with code $exitCode"
    }

    $statusMarker = "__STATUS__:"
    $markerIndex = $response.LastIndexOf($statusMarker)
    if ($markerIndex -lt 0) {
        throw "Could not parse HTTP status from curl response"
    }

    $body = $response.Substring(0, $markerIndex).TrimEnd("`r", "`n")
    $statusCode = [int]$response.Substring($markerIndex + $statusMarker.Length).Trim()

    [pscustomobject]@{
        StatusCode = $statusCode
        Body = $body
    }
}

$testUrl = $BaseUrl.TrimEnd('/') + "/api/demo"

Write-Host "Activator: $BaseUrl"
Write-Host "Backend: $BackendHost`:$BackendPort"
Write-Host "Concurrent requests: $RequestCount"
Write-Host "Test URL: $testUrl"

$health = Invoke-CurlRequest -CurlArgs @("$BaseUrl/health")
if ($health.StatusCode -ne 200) {
    throw "Activator health check failed with HTTP $($health.StatusCode)"
}

Write-Host ""
Write-Host "[Activator health]" -ForegroundColor Cyan
Write-Host "Status: $($health.StatusCode)"
Write-Host "Body:"
Write-Host $health.Body

if (Test-TcpPortOpen -HostName $BackendHost -Port $BackendPort) {
    Write-Host ""
    Write-Host "Backend port $BackendPort is already open." -ForegroundColor Yellow
    Write-Host "Restart the activator or stop the backend first if you want a real cold-start concurrency check."
    exit 1
}

Write-Host ""
Write-Host "[Concurrent cold start]" -ForegroundColor Cyan
Write-Host "Launching $RequestCount curl.exe requests in parallel..."

$results = 1..$RequestCount | ForEach-Object -Parallel {
    $responseLines = @(& curl.exe --silent --show-error --output - --write-out "`n__STATUS__:%{http_code}" $using:testUrl)
    $exitCode = $LASTEXITCODE
    $response = $responseLines -join "`n"
    $statusMarker = "__STATUS__:"
    $markerIndex = $response.LastIndexOf($statusMarker)

    $statusCode = -1
    $body = $response

    if ($markerIndex -ge 0) {
        $body = $response.Substring(0, $markerIndex).TrimEnd("`r", "`n")
        $statusCode = [int]$response.Substring($markerIndex + $statusMarker.Length).Trim()
    }

    [pscustomobject]@{
        Index = $_
        ExitCode = $exitCode
        StatusCode = $statusCode
        Body = $body
    }
} -ThrottleLimit $RequestCount

$failed = $false
foreach ($result in ($results | Sort-Object Index)) {
    Write-Host ""
    Write-Host "[Request $($result.Index)]" -ForegroundColor Cyan
    Write-Host "Status: $($result.StatusCode)"
    Write-Host "Body:"
    Write-Host $result.Body

    if ($result.ExitCode -ne 0) {
        Write-Host "FAILED: curl exited with code $($result.ExitCode)" -ForegroundColor Red
        $failed = $true
        continue
    }

    if ($result.StatusCode -ne 200) {
        Write-Host "FAILED: expected HTTP 200" -ForegroundColor Red
        $failed = $true
        continue
    }

    if (-not $result.Body.Contains("GET /demo")) {
        Write-Host "FAILED: unexpected body" -ForegroundColor Red
        $failed = $true
    }
}

if (-not (Test-TcpPortOpen -HostName $BackendHost -Port $BackendPort)) {
    Write-Host ""
    Write-Host "FAILED: backend port did not open after the concurrent requests." -ForegroundColor Red
    exit 1
}

if ($failed) {
    Write-Host ""
    Write-Host "Concurrent startup test failed." -ForegroundColor Red
    exit 1
}

Write-Host ""
Write-Host "All concurrent requests succeeded from a cold start." -ForegroundColor Green
Write-Host "This validates the external Phase 2 behavior. The Rust test suite also checks single-startup coordination."
