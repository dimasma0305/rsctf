[CmdletBinding()]
param(
    [ValidatePattern('^v[0-9]+\.[0-9]+\.[0-9]+$')]
    [string]$Version
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.Net.Http

$repository = 'dimasma0305/rsctf'
$releaseBase = "https://github.com/$repository/releases"
$taskName = 'RSCTF Worker Agent'
$installDirectory = Join-Path $env:ProgramFiles 'RSCTF Worker'
$stateDirectory = Join-Path $env:ProgramData 'RSCTF Worker'
$binaryPath = Join-Path $installDirectory 'rsctf-worker-agent.exe'
$temporaryDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ("rsctf-worker-install-" + [Guid]::NewGuid().ToString('N'))
$archiveName = 'rsctf-worker-agent-windows-amd64.zip'
$archivePath = Join-Path $temporaryDirectory $archiveName
$checksumPath = Join-Path $temporaryDirectory 'SHA256SUMS'
$extractDirectory = Join-Path $temporaryDirectory 'extract'
$restartExistingTask = $false

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

function Protect-WorkerDirectory {
    param(
        [string]$Path,
        [string]$OwnerSid = 'S-1-5-32-544'
    )
    if (Test-Path -LiteralPath $Path) {
        $item = Get-Item -LiteralPath $Path -Force
        if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "$Path must be a real directory, not a reparse point or another file type"
        }
    } else {
        New-Item -ItemType Directory -Path $Path -Force | Out-Null
    }
    $systemSid = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
    $adminSid = [Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $acl.SetAccessRuleProtection($true, $false)
    $owner = [Security.Principal.SecurityIdentifier]::new($OwnerSid)
    $acl.SetOwner($owner)
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
    Assert-WorkerAcl -Path $Path -ExpectedOwnerSid $owner.Value
}

function Protect-WorkerFile {
    param([string]$Path)

    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "$Path must be a regular file, not a reparse point or directory"
    }
    $systemSid = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
    $adminSid = [Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')
    $acl = [Security.AccessControl.FileSecurity]::new()
    $acl.SetAccessRuleProtection($true, $false)
    $acl.SetOwner($adminSid)
    foreach ($sid in @($systemSid, $adminSid)) {
        $rule = [Security.AccessControl.FileSystemAccessRule]::new(
            $sid,
            [Security.AccessControl.FileSystemRights]::FullControl,
            [Security.AccessControl.AccessControlType]::Allow
        )
        [void]$acl.AddAccessRule($rule)
    }
    Set-Acl -LiteralPath $Path -AclObject $acl -ErrorAction Stop
    Assert-WorkerAcl -Path $Path -ExpectedOwnerSid $adminSid.Value
}

function Assert-WorkerAcl {
    param(
        [string]$Path,
        [string]$ExpectedOwnerSid
    )

    $systemSid = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
    $adminSid = [Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')
    $allowed = @($systemSid.Value, $adminSid.Value)
    $acl = Get-Acl -LiteralPath $Path -ErrorAction Stop
    if (-not $acl.AreAccessRulesProtected) { throw "$Path still inherits access rules" }
    if ($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $ExpectedOwnerSid) {
        throw "$Path has an unexpected owner"
    }
    $present = @{}
    foreach ($rule in $acl.Access) {
        $sid = $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value
        if ($rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow -or $allowed -notcontains $sid) {
            throw "$Path contains an unexpected access rule for $sid"
        }
        if (($rule.FileSystemRights -band [Security.AccessControl.FileSystemRights]::FullControl) -eq [Security.AccessControl.FileSystemRights]::FullControl) {
            $present[$sid] = $true
        }
    }
    foreach ($sid in $allowed) {
        if (-not $present.ContainsKey($sid)) { throw "$Path is missing full control for $sid" }
    }
}

try {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'run this installer from an Administrator PowerShell window'
    }
    if (-not [Environment]::Is64BitOperatingSystem -or -not [Environment]::Is64BitProcess) {
        throw 'run this installer from 64-bit PowerShell on 64-bit Windows'
    }
    $nativeArchitecture = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if ($nativeArchitecture -ne 'AMD64') { throw "Windows AMD64 is required (found $nativeArchitecture)" }
    if (-not (Get-Command docker.exe -ErrorAction SilentlyContinue)) { throw 'Docker is not installed or not on PATH' }
    $dockerInfo = (& docker.exe info --format '{{.OSType}}|{{.Driver}}' 2>$null).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Docker is not running' }
    $dockerFields = $dockerInfo.Split('|')
    if ($dockerFields.Count -ne 2) { throw 'Docker returned incomplete runtime information' }
    $dockerOs = $dockerFields[0]
    $dockerDriver = $dockerFields[1]
    if ($dockerOs -ne 'windows') {
        throw 'Docker must be switched to native Windows-container mode; Linux-container mode is intentionally unsupported by the native agent'
    }
    if ($dockerDriver -ne 'windowsfilter') {
        throw "Docker must use windowsfilter so writable-layer quotas are enforceable (found $dockerDriver)"
    }

    New-Item -ItemType Directory -Path $temporaryDirectory -ErrorAction Stop | Out-Null
    Protect-WorkerDirectory -Path $temporaryDirectory
    New-Item -ItemType Directory -Path $extractDirectory -ErrorAction Stop | Out-Null
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
    [void](Get-HttpsFile -Uri "$downloadBase/$archiveName" -Destination $archivePath -MaximumBytes 134217728)
    [void](Get-HttpsFile -Uri "$downloadBase/SHA256SUMS" -Destination $checksumPath -MaximumBytes 1048576)

    $escapedArchive = [Regex]::Escape($archiveName)
    $checksumLines = @(Get-Content -LiteralPath $checksumPath | Where-Object { $_ -match "^([0-9A-Fa-f]{64})  $escapedArchive$" })
    if ($checksumLines.Count -ne 1) { throw "SHA256SUMS must contain exactly one checksum for $archiveName" }
    $hashMatch = [Regex]::Match($checksumLines[0], '^([0-9A-Fa-f]{64})')
    $expectedHash = $hashMatch.Groups[1].Value.ToLowerInvariant()
    $actualHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $expectedHash) { throw "SHA-256 verification failed for $archiveName" }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $allowed = @{
            'rsctf-worker-agent/rsctf-worker-agent.exe' = 'rsctf-worker-agent.exe'
            'rsctf-worker-agent/LICENSE.txt' = 'LICENSE.txt'
            'rsctf-worker-agent/NOTICE' = 'NOTICE'
        }
        $seen = @{}
        [long]$uncompressedBytes = 0
        foreach ($entry in $zip.Entries) {
            $entryName = $entry.FullName.Replace('\', '/')
            if ($entryName -eq 'rsctf-worker-agent/' -and $entry.Length -eq 0) { continue }
            if (-not $allowed.ContainsKey($entryName) -or $seen.ContainsKey($entryName)) {
                throw "release archive contains an unexpected or duplicate entry: $entryName"
            }
            if ($entry.Length -gt 134217728) { throw "release archive entry is too large: $entryName" }
            $uncompressedBytes += $entry.Length
            if ($uncompressedBytes -gt 268435456) { throw 'release archive expands beyond the 256 MiB safety limit' }
            $destination = Join-Path $extractDirectory $allowed[$entryName]
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $destination, $false)
            $seen[$entryName] = $true
        }
        if ($seen.Count -ne $allowed.Count) { throw 'release archive is missing required files' }
    } finally {
        $zip.Dispose()
    }

    Protect-WorkerDirectory -Path $installDirectory
    Protect-WorkerDirectory -Path $stateDirectory -OwnerSid 'S-1-5-18'
    $existingTask = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if ($existingTask) {
        $restartExistingTask = $existingTask.State -eq 'Running'
        Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        for ($attempt = 0; $attempt -lt 30; $attempt += 1) {
            if ((Get-ScheduledTask -TaskName $taskName).State -ne 'Running') { break }
            Start-Sleep -Milliseconds 500
        }
        if ((Get-ScheduledTask -TaskName $taskName).State -eq 'Running') {
            throw 'existing worker task did not stop before the upgrade'
        }
    }
    $transaction = [Guid]::NewGuid().ToString('N')
    $stagedFiles = @{}
    foreach ($fileName in @('rsctf-worker-agent.exe', 'LICENSE.txt', 'NOTICE')) {
        $destination = Join-Path $installDirectory $fileName
        if (Test-Path -LiteralPath $destination) {
            $installedItem = Get-Item -LiteralPath $destination -Force
            if ($installedItem.PSIsContainer -or ($installedItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                throw "$destination must be a regular file, not a reparse point or directory"
            }
        }
        $stagedPath = Join-Path $installDirectory (".$fileName.new-$transaction")
        Copy-Item -LiteralPath (Join-Path $extractDirectory $fileName) -Destination $stagedPath
        $stagedFiles[$fileName] = $stagedPath
    }
    try {
        foreach ($fileName in @('rsctf-worker-agent.exe', 'LICENSE.txt', 'NOTICE')) {
            $destination = Join-Path $installDirectory $fileName
            $stagedPath = $stagedFiles[$fileName]
            if (Test-Path -LiteralPath $destination -PathType Leaf) {
                [System.IO.File]::Replace($stagedPath, $destination, $null, $true)
            } else {
                Move-Item -LiteralPath $stagedPath -Destination $destination
            }
            Protect-WorkerFile -Path $destination
        }
    } finally {
        foreach ($stagedPath in $stagedFiles.Values) {
            if (Test-Path -LiteralPath $stagedPath) { Remove-Item -LiteralPath $stagedPath -Force }
        }
    }

    $arguments = 'run --config "' + (Join-Path $stateDirectory 'worker.json') + '" --accept-host-network-boundary --writable-layer-bytes 34359738368'
    $action = New-ScheduledTaskAction -Execute $binaryPath -Argument $arguments
    $trigger = New-ScheduledTaskTrigger -AtStartup
    $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero)
    Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
    if ($restartExistingTask) {
        Start-ScheduledTask -TaskName $taskName
        Start-Sleep -Seconds 3
        if ((Get-ScheduledTask -TaskName $taskName).State -ne 'Running') {
            throw 'the upgraded worker task did not remain running'
        }
    }
    if ($restartExistingTask) {
        Write-Host "Updated RSCTF worker $Version and restarted its existing task." -ForegroundColor Green
    } elseif (Test-Path -LiteralPath (Join-Path $stateDirectory 'worker.json') -PathType Leaf) {
        Write-Host "Updated RSCTF worker $Version. Its existing task remains stopped." -ForegroundColor Green
    } else {
        Write-Host "Installed RSCTF worker $Version. Enrollment is required before the task is started." -ForegroundColor Green
    }
} finally {
    if (Test-Path -LiteralPath $temporaryDirectory) {
        Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force
    }
}
