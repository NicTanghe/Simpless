param(
    [string]$BaseUrl = "http://127.0.0.1:3000"
)

$ErrorActionPreference = "Stop"
$ExpectedForwardedHost = ([uri]$BaseUrl).Authority

$script:Failures = 0

Write-Host "Smoke test target: $BaseUrl"
Write-Host "The backend should autostart on the first /api request."

function Invoke-CurlTest {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name,

        [Parameter(Mandatory = $true)]
        [string[]]$CurlArgs,

        [Parameter(Mandatory = $true)]
        [int]$ExpectedStatus,

        [string[]]$ExpectedContains = @()
    )

    Write-Host ""
    Write-Host "[$Name]" -ForegroundColor Cyan
    Write-Host ("curl.exe " + ($CurlArgs -join " "))

    $responseLines = @(& curl.exe --silent --show-error --output - --write-out "`n__STATUS__:%{http_code}" @CurlArgs)
    $exitCode = $LASTEXITCODE
    $response = $responseLines -join "`n"

    if ($exitCode -ne 0) {
        Write-Host "FAILED: curl exited with code $exitCode" -ForegroundColor Red
        $script:Failures++
        return
    }

    $statusMarker = "__STATUS__:"
    $markerIndex = $response.LastIndexOf($statusMarker)
    if ($markerIndex -lt 0) {
        Write-Host "FAILED: could not parse HTTP status" -ForegroundColor Red
        $script:Failures++
        return
    }

    $body = $response.Substring(0, $markerIndex).TrimEnd("`r", "`n")
    $statusText = $response.Substring($markerIndex + $statusMarker.Length).Trim()
    $statusCode = [int]$statusText

    Write-Host "Status: $statusCode"
    if ($body.Length -gt 0) {
        Write-Host "Body:"
        Write-Host $body
    }

    if ($statusCode -ne $ExpectedStatus) {
        Write-Host "FAILED: expected status $ExpectedStatus" -ForegroundColor Red
        $script:Failures++
        return
    }

    foreach ($expected in $ExpectedContains) {
        if (-not $body.Contains($expected)) {
            Write-Host "FAILED: body did not contain: $expected" -ForegroundColor Red
            $script:Failures++
            return
        }
    }

    Write-Host "PASS" -ForegroundColor Green
}

Invoke-CurlTest `
    -Name "Activator health" `
    -CurlArgs @("$BaseUrl/health") `
    -ExpectedStatus 200 `
    -ExpectedContains @('"status":"ok"')

Invoke-CurlTest `
    -Name "Activator ready" `
    -CurlArgs @("$BaseUrl/ready") `
    -ExpectedStatus 200 `
    -ExpectedContains @('"status":"ready"')

Invoke-CurlTest `
    -Name "Backend root through /api" `
    -CurlArgs @("$BaseUrl/api/") `
    -ExpectedStatus 200 `
    -ExpectedContains @("hello from the test backend")

Invoke-CurlTest `
    -Name "Backend health through /api" `
    -CurlArgs @("$BaseUrl/api/health") `
    -ExpectedStatus 200 `
    -ExpectedContains @("ok")

Invoke-CurlTest `
    -Name "Path rewrite" `
    -CurlArgs @("$BaseUrl/api/demo") `
    -ExpectedStatus 200 `
    -ExpectedContains @("GET /demo", "x-forwarded-host=$ExpectedForwardedHost")

Invoke-CurlTest `
    -Name "Query string forwarding" `
    -CurlArgs @("$BaseUrl/api/orders/123?expand=true") `
    -ExpectedStatus 200 `
    -ExpectedContains @("GET /orders/123?expand=true")

Invoke-CurlTest `
    -Name "POST body forwarding" `
    -CurlArgs @("-X", "POST", "$BaseUrl/api/demo", "-d", "ping") `
    -ExpectedStatus 200 `
    -ExpectedContains @("POST /demo", "body=ping")

Invoke-CurlTest `
    -Name "Forwarded host override" `
    -CurlArgs @("-X", "POST", "$BaseUrl/api/demo", "-H", "Host: gateway.test", "-d", "ping") `
    -ExpectedStatus 200 `
    -ExpectedContains @("x-forwarded-host=gateway.test", "body=ping")

Invoke-CurlTest `
    -Name "Unknown route fails closed" `
    -CurlArgs @("$BaseUrl/admin/test") `
    -ExpectedStatus 404 `
    -ExpectedContains @("unknown service route")

Write-Host ""
if ($script:Failures -gt 0) {
    Write-Host "$script:Failures test(s) failed." -ForegroundColor Red
    exit 1
}

Write-Host "All smoke tests passed." -ForegroundColor Green
