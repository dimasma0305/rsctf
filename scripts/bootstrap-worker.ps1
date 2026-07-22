[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^https://[A-Za-z0-9.-]+(?::[0-9]{1,5})?$')]
    [string]$ServerUrl,

    [ValidatePattern('^v[0-9]+\.[0-9]+\.[0-9]+$')]
    [string]$Version
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.Net.Http

$repository = 'dimasma0305/rsctf'
$releaseBase = "https://github.com/$repository/releases"
$temporaryDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ("rsctf-worker-bootstrap-" + [Guid]::NewGuid().ToString('N'))
$installerPath = Join-Path $temporaryDirectory 'install-worker.ps1'
$checksumPath = Join-Path $temporaryDirectory 'SHA256SUMS'
$agentPath = Join-Path $env:ProgramFiles 'RSCTF Worker\rsctf-worker-agent.exe'
$stateDirectory = Join-Path $env:ProgramData 'RSCTF Worker'
$taskName = 'RSCTF Worker Agent'
$identityNames = @('worker-key.pem', 'worker-cert.pem', 'worker-ca.pem', 'worker.json')

function Get-HttpsFile {
    param([string]$Uri, [string]$Destination, [long]$MaximumBytes)

    $handler = [System.Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $true
    $handler.MaxAutomaticRedirections = 5
    $client = [System.Net.Http.HttpClient]::new($handler)
    $client.Timeout = [TimeSpan]::FromMinutes(5)
    $response = $null
    try {
        $response = $client.GetAsync($Uri, [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        [void]$response.EnsureSuccessStatusCode()
        $finalUri = $response.RequestMessage.RequestUri
        if ($finalUri.Scheme -ne 'https') { throw "download redirected outside HTTPS: $finalUri" }
        if ($response.Content.Headers.ContentLength -and $response.Content.Headers.ContentLength -gt $MaximumBytes) {
            throw "download exceeds $MaximumBytes bytes"
        }
        $inputStream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $outputStream = $null
        try {
            $outputStream = [System.IO.File]::Open($Destination, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
            $buffer = New-Object byte[] 65536
            [long]$written = 0
            while (($read = $inputStream.Read($buffer, 0, $buffer.Length)) -gt 0) {
                $written += $read
                if ($written -gt $MaximumBytes) { throw "download exceeds $MaximumBytes bytes" }
                $outputStream.Write($buffer, 0, $read)
            }
        } finally {
            if ($outputStream) { $outputStream.Dispose() }
            $inputStream.Dispose()
        }
        return $finalUri
    } finally {
        if ($response) { $response.Dispose() }
        $client.Dispose()
        $handler.Dispose()
    }
}

function Protect-TemporaryDirectory {
    param([string]$Path)

    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "$Path must be a real directory, not a reparse point or another file type"
    }
    $systemSid = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
    $adminSid = [Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $acl.SetAccessRuleProtection($true, $false)
    $acl.SetOwner($adminSid)
    $inheritance = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [Security.AccessControl.InheritanceFlags]::ObjectInherit
    foreach ($sid in @($systemSid, $adminSid)) {
        $rule = [Security.AccessControl.FileSystemAccessRule]::new(
            $sid,
            [Security.AccessControl.FileSystemRights]::FullControl,
            $inheritance,
            [Security.AccessControl.PropagationFlags]::None,
            [Security.AccessControl.AccessControlType]::Allow
        )
        [void]$acl.AddAccessRule($rule)
    }
    Set-Acl -LiteralPath $Path -AclObject $acl -ErrorAction Stop
    $check = Get-Acl -LiteralPath $Path -ErrorAction Stop
    if (-not $check.AreAccessRulesProtected) { throw 'temporary directory still inherits access rules' }
    if ($check.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $adminSid.Value) {
        throw 'temporary directory owner is not the local Administrators group'
    }
    $allowed = @($systemSid.Value, $adminSid.Value)
    $present = @{}
    foreach ($rule in $check.Access) {
        $sid = $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value
        if ($rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow -or $allowed -notcontains $sid) {
            throw "temporary directory contains an unexpected access rule for $sid"
        }
        if (($rule.FileSystemRights -band [Security.AccessControl.FileSystemRights]::FullControl) -eq [Security.AccessControl.FileSystemRights]::FullControl) {
            $present[$sid] = $true
        }
    }
    foreach ($sid in $allowed) {
        if (-not $present.ContainsKey($sid)) { throw "temporary directory is missing full control for $sid" }
    }
}

function Assert-WorkerTaskRunning {
    $task = Get-ScheduledTask -TaskName $taskName -ErrorAction Stop
    if ($task.State -ne 'Running') {
        Start-ScheduledTask -TaskName $taskName
    }
    Start-Sleep -Seconds 3
    $task = Get-ScheduledTask -TaskName $taskName -ErrorAction Stop
    if ($task.State -ne 'Running') {
        $result = (Get-ScheduledTaskInfo -TaskName $taskName -ErrorAction Stop).LastTaskResult
        throw "worker task did not remain running (state=$($task.State), lastResult=$result)"
    }
}

try {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'run this command from an Administrator PowerShell window'
    }
    $presentIdentityNames = @($identityNames | Where-Object {
        Test-Path -LiteralPath (Join-Path $stateDirectory $_) -PathType Leaf
    })
    if ($presentIdentityNames.Count -ne 0 -and $presentIdentityNames.Count -ne $identityNames.Count) {
        throw 'the worker state directory contains an incomplete identity; revoke that worker and clean the state deliberately before enrolling again'
    }
    $existingEnrollment = $presentIdentityNames.Count -eq $identityNames.Count
    New-Item -ItemType Directory -Path $temporaryDirectory -ErrorAction Stop | Out-Null
    Protect-TemporaryDirectory -Path $temporaryDirectory

    if (-not $Version) {
        $probePath = Join-Path $temporaryDirectory 'latest'
        $latestUri = Get-HttpsFile -Uri "$releaseBase/latest" -Destination $probePath -MaximumBytes 1048576
        $prefix = "$releaseBase/tag/"
        if (-not $latestUri.AbsoluteUri.StartsWith($prefix, [StringComparison]::Ordinal)) {
            throw "latest release redirected to an unexpected URL: $latestUri"
        }
        $Version = $latestUri.AbsoluteUri.Substring($prefix.Length)
        if ($Version -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') { throw 'latest release does not use a vX.Y.Z tag' }
    }

    $downloadBase = "$releaseBase/download/$Version"
    [void](Get-HttpsFile -Uri "$downloadBase/install-worker.ps1" -Destination $installerPath -MaximumBytes 1048576)
    [void](Get-HttpsFile -Uri "$downloadBase/SHA256SUMS" -Destination $checksumPath -MaximumBytes 1048576)
    $checksumLines = @(Get-Content -LiteralPath $checksumPath | Where-Object {
        $_ -match '^([0-9A-Fa-f]{64})  install-worker\.ps1$'
    })
    if ($checksumLines.Count -ne 1) { throw 'SHA256SUMS must contain exactly one checksum for install-worker.ps1' }
    $expectedHash = [Regex]::Match($checksumLines[0], '^([0-9A-Fa-f]{64})').Groups[1].Value.ToLowerInvariant()
    $actualHash = (Get-FileHash -LiteralPath $installerPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $expectedHash) { throw 'SHA-256 verification failed for install-worker.ps1' }
    & powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File $installerPath -Version $Version
    if ($LASTEXITCODE -ne 0) { throw "worker installer exited with code $LASTEXITCODE" }

    if (-not $existingEnrollment) {
        Write-Warning 'This host/VM must be dedicated to RSCTF challenge workloads and must not hold unrelated secrets.'
        $hostConfirmation = Read-Host 'Type DEDICATED to continue'
        if ($hostConfirmation -cne 'DEDICATED') { throw 'dedicated worker-host confirmation was not provided' }
        $hostConfirmation = $null
        $secureToken = Read-Host 'One-time enrollment token' -AsSecureString
        $tokenPointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secureToken)
        try {
            $plainToken = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($tokenPointer)
            if ([string]::IsNullOrWhiteSpace($plainToken)) { throw 'the enrollment token must not be empty' }
            $plainToken | & $agentPath enroll --server-url $ServerUrl --token-stdin --state-dir $stateDirectory --windows-service-account 'S-1-5-18'
            if ($LASTEXITCODE -ne 0) { throw "worker enrollment exited with code $LASTEXITCODE" }
        } finally {
            if ($tokenPointer -ne [IntPtr]::Zero) { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($tokenPointer) }
            $plainToken = $null
            $secureToken.Dispose()
        }
    }

    Assert-WorkerTaskRunning
    if ($existingEnrollment) {
        Write-Host 'RSCTF worker updated and restarted; the existing mTLS identity was preserved.' -ForegroundColor Green
    } else {
        Write-Host 'RSCTF worker installed, enrolled, and started successfully.' -ForegroundColor Green
    }
} finally {
    if (Test-Path -LiteralPath $temporaryDirectory) {
        Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force
    }
}
