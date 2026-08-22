[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string] $ProductVersion,
    [string] $ArtifactDirectory,
    [string] $LogDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..\..")).Path
if ([string]::IsNullOrWhiteSpace($ArtifactDirectory)) {
    $ArtifactDirectory = Join-Path $repositoryRoot "dist"
}
else {
    $ArtifactDirectory = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath(
        $ArtifactDirectory
    )
}
if ([string]::IsNullOrWhiteSpace($LogDirectory)) {
    $temporaryRoot = if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
        [IO.Path]::GetTempPath()
    }
    else {
        $env:RUNNER_TEMP
    }
    $LogDirectory = Join-Path $temporaryRoot "unionc-agent-msi-logs"
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "The MSI lifecycle smoke test requires an elevated Windows session."
}

$installedRoot = Join-Path $env:ProgramFiles "UnionC Agent"
$installedTray = Join-Path $installedRoot "unionc-agent-tray.exe"
$stateRoot = Join-Path $env:ProgramData "UnionC Agent"
$trayRunKey = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Run"
$trayRunName = "UnionCAgentTray"
$commonPrograms = [Environment]::GetFolderPath(
    [Environment+SpecialFolder]::CommonPrograms
)
$trayShortcut = Join-Path $commonPrograms "UnionC Agent.lnk"
$stateMarker = ".unionc-agent-managed-$ProductVersion"
$installJournal = Join-Path $env:ProgramData `
    "UnionC Agent.install-journal-$ProductVersion"
$uninstallJournal = Join-Path $env:ProgramData `
    "UnionC Agent.uninstall-journal-$ProductVersion"
$purgeQuarantine = Join-Path $env:ProgramData `
    "UnionC Agent.purge-quarantine-$ProductVersion"
$logs = $LogDirectory
New-Item -ItemType Directory -Force $logs | Out-Null

if (-not ("UnionC.MsiNativeMethods" -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;

namespace UnionC {
    public static class MsiNativeMethods {
        [DllImport("msi.dll", EntryPoint = "MsiGetShortcutTargetW",
            CharSet = CharSet.Unicode, ExactSpelling = true)]
        public static extern uint MsiGetShortcutTarget(
            string shortcutTarget,
            StringBuilder productCode,
            StringBuilder featureId,
            StringBuilder componentCode);

        [DllImport("msi.dll", EntryPoint = "MsiGetComponentPathW",
            CharSet = CharSet.Unicode, ExactSpelling = true)]
        public static extern int MsiGetComponentPath(
            string productCode,
            string componentCode,
            StringBuilder path,
            ref uint pathLength);
    }
}
"@
}

function Invoke-Msi {
    param(
        [Parameter(Mandatory = $true)][ValidateSet('/i', '/x')][string]$Operation,
        [Parameter(Mandatory = $true)][string]$Package,
        [Parameter(Mandatory = $true)][string]$Name,
        [string]$Properties = "",
        [switch]$ExpectFailure
    )
    $log = Join-Path $logs "${Name}.log"
    $arguments = "${Operation} `"${Package}`" ${Properties} /qn /norestart /l*v `"${log}`""
    $process = Start-Process -FilePath msiexec.exe -ArgumentList $arguments -Wait -PassThru
    $succeeded = $process.ExitCode -in @(0, 3010)
    if ($ExpectFailure -and $succeeded) {
        throw "MSI operation '$Name' unexpectedly succeeded. Log: $log"
    }
    if (-not $ExpectFailure -and -not $succeeded) {
        Get-Content -LiteralPath $log -Tail 120
        throw "MSI operation '$Name' failed with exit code $($process.ExitCode). Log: $log"
    }
}

function Assert-ServiceRunning {
    $service = Get-Service -Name "UnionCAgent" -ErrorAction Stop
    $service.WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Running,
        [TimeSpan]::FromSeconds(30))
    $definition = Get-CimInstance Win32_Service -Filter "Name='UnionCAgent'"
    if ($definition.StartMode -ne "Auto" -or
        $definition.StartName -ne "NT AUTHORITY\LocalService" -or
        $definition.PathName -notmatch '--windows-service run --config') {
        throw "Installed SCM service definition is not the expected UnionC Agent service."
    }
    $serviceKey = "HKLM:\SYSTEM\CurrentControlSet\Services\UnionCAgent"
    if ((Get-ItemPropertyValue -LiteralPath $serviceKey -Name ServiceSidType) -ne 1) {
        throw "UnionCAgent does not have an unrestricted service SID."
    }
    if ((Get-ItemPropertyValue -LiteralPath $serviceKey `
        -Name FailureActionsOnNonCrashFailures) -ne 1) {
        throw "UnionCAgent does not enable failure actions for non-crash failures."
    }
}

function Assert-StateAcl {
    $serviceSid = (New-Object System.Security.Principal.NTAccount(
        "NT SERVICE", "UnionCAgent"
    )).Translate([System.Security.Principal.SecurityIdentifier]).Value
    $protectedPaths = @($stateRoot)
    foreach ($child in @(
        $stateMarker, "config.json", "release-lifecycle-marker"
    )) {
        $candidate = Join-Path $stateRoot $child
        if (Test-Path -LiteralPath $candidate) { $protectedPaths += $candidate }
    }
    foreach ($protectedPath in $protectedPaths) {
        $acl = Get-Acl -LiteralPath $protectedPath
        $allowSids = @($acl.Access |
            Where-Object AccessControlType -eq `
                ([System.Security.AccessControl.AccessControlType]::Allow) |
            ForEach-Object {
                $_.IdentityReference.Translate(
                    [System.Security.Principal.SecurityIdentifier]
                ).Value
            })
        if ($allowSids -notcontains $serviceSid -or
            $allowSids -contains "S-1-5-19" -or
            $allowSids -notcontains "S-1-3-4") {
            throw "$protectedPath is not isolated to the dedicated service SID."
        }
        if ($protectedPath -in @(
            $stateRoot,
            (Join-Path $stateRoot $stateMarker),
            (Join-Path $stateRoot "config.json")
        )) {
            $ownerSid = $acl.GetOwner(
                [System.Security.Principal.SecurityIdentifier]
            ).Value
            if ($ownerSid -ne "S-1-5-18" -or -not $acl.AreAccessRulesProtected) {
                throw "$protectedPath lacks the exact managed SYSTEM/protected anchor."
            }
        }
    }
}

function Assert-TrayIntegration {
    if (Get-Process -Name "unionc-agent-tray" -ErrorAction SilentlyContinue) {
        throw "A quiet MSI install incorrectly launched an interactive tray process."
    }
    if (-not (Test-Path -LiteralPath $installedTray -PathType Leaf)) {
        throw "The installed tray companion is missing."
    }
    $expectedRun = '"{0}" --startup' -f $installedTray
    $actualRun = Get-ItemPropertyValue -LiteralPath $trayRunKey `
        -Name $trayRunName -ErrorAction Stop
    if ($actualRun -cne $expectedRun) {
        throw "The tray Run registration is not the fixed installed command."
    }
    if (-not (Test-Path -LiteralPath $trayShortcut -PathType Leaf)) {
        throw "The UnionC Agent Start menu shortcut is missing."
    }

    # Advertised MSI shortcuts store a Darwin descriptor instead of a normal shell-link
    # target, so WScript.Shell.TargetPath is not a reliable assertion. Ask Windows
    # Installer which installed component the actual shortcut advertises instead.
    $productCode = New-Object System.Text.StringBuilder 39
    $featureId = New-Object System.Text.StringBuilder 39
    $componentCode = New-Object System.Text.StringBuilder 39
    $shortcutResult = [UnionC.MsiNativeMethods]::MsiGetShortcutTarget(
        $trayShortcut, $productCode, $featureId, $componentCode
    )
    if ($shortcutResult -ne 0 -or $featureId.ToString() -cne "AgentFeature" -or
        $componentCode.ToString() -ine "{882DF421-2758-42E4-95D4-730C2571803E}") {
        throw "The Start menu shortcut does not advertise the tray component."
    }

    $componentPath = New-Object System.Text.StringBuilder 32768
    [uint32]$componentPathLength = $componentPath.Capacity
    $componentState = [UnionC.MsiNativeMethods]::MsiGetComponentPath(
        $productCode.ToString(), $componentCode.ToString(),
        $componentPath, [ref]$componentPathLength
    )
    if ($componentState -ne 3 -or -not [string]::Equals(
        [IO.Path]::GetFullPath($componentPath.ToString()),
        [IO.Path]::GetFullPath($installedTray),
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "The advertised tray component is not installed at the fixed path."
    }

    # The service/config remains isolated, but every interactive user must be able
    # to execute the signed tray image from the protected Program Files tree.
    $usersRules = @((Get-Acl -LiteralPath $installedTray).Access | Where-Object {
        $_.AccessControlType -eq `
            [System.Security.AccessControl.AccessControlType]::Allow -and
        $_.IdentityReference.Translate(
            [System.Security.Principal.SecurityIdentifier]
        ).Value -eq "S-1-5-32-545"
    })
    if ($usersRules.Count -ne 1 -or
        [int]$usersRules[0].FileSystemRights -ne 0x1200a9) {
        throw "The tray image does not grant BUILTIN\\Users exact read/execute access."
    }
}

function Start-TrayForRemovalSmoke {
    $process = Start-Process -FilePath $installedTray -ArgumentList "--startup" -PassThru
    Start-Sleep -Seconds 2
    $process.Refresh()
    if ($process.HasExited) {
        throw "The tray companion exited before the MSI shutdown smoke could run."
    }
    return $process
}

function Assert-ArpVersion([string]$ExpectedVersion) {
    $uninstallRoot = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"
    $entries = @(Get-ChildItem -LiteralPath $uninstallRoot | Get-ItemProperty |
        Where-Object DisplayName -eq "UnionC Agent")
    if ($entries.Count -ne 1 -or $entries[0].DisplayVersion -ne $ExpectedVersion) {
        throw "Apps & Features does not contain exactly UnionC Agent $ExpectedVersion."
    }
}

function Assert-AgentCompletelyAbsent {
    if ((Test-Path -LiteralPath $installedRoot) -or
        (Test-Path -LiteralPath $stateRoot) -or
        (Test-Path -LiteralPath $installJournal) -or
        (Test-Path -LiteralPath $uninstallJournal) -or
        (Test-Path -LiteralPath $purgeQuarantine) -or
        (Test-Path -LiteralPath $trayShortcut) -or
        (Get-ItemProperty -LiteralPath $trayRunKey -Name $trayRunName `
            -ErrorAction SilentlyContinue) -or
        (Get-Service -Name "UnionCAgent" -ErrorAction SilentlyContinue)) {
        throw "Agent installation or a protected transaction artifact survived purge."
    }
    $uninstallRoot = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"
    $entries = @(Get-ChildItem -LiteralPath $uninstallRoot | Get-ItemProperty |
        Where-Object DisplayName -eq "UnionC Agent")
    if ($entries.Count -ne 0) {
        throw "Apps & Features still contains UnionC Agent."
    }
}

function Assert-PreservedStateAcl([string]$RetiredServiceSid) {
    $protectedPaths = @($stateRoot)
    foreach ($child in @(
        $stateMarker, "config.json", "release-lifecycle-marker"
    )) {
        $candidate = Join-Path $stateRoot $child
        if (Test-Path -LiteralPath $candidate) { $protectedPaths += $candidate }
    }
    foreach ($protectedPath in $protectedPaths) {
        $acl = Get-Acl -LiteralPath $protectedPath
        $ownerSid = $acl.GetOwner(
            [System.Security.Principal.SecurityIdentifier]
        ).Value
        if ($ownerSid -ne "S-1-5-18" -or -not $acl.AreAccessRulesProtected) {
            throw "$protectedPath does not have SYSTEM ownership and a protected DACL."
        }

        $rules = @($acl.Access)
        $expectedRights = @{
            "S-1-5-18" = 0x1f01ff
            "S-1-5-32-544" = 0x1f01ff
            "S-1-3-4" = 0x00020000
        }
        $expectedInheritance = `
            [System.Security.AccessControl.InheritanceFlags]::ContainerInherit -bor `
            [System.Security.AccessControl.InheritanceFlags]::ObjectInherit
        if ($rules.Count -ne $expectedRights.Count) {
            throw "$protectedPath has an unexpected preserved-state ACE count."
        }
        foreach ($rule in $rules) {
            $sid = $rule.IdentityReference.Translate(
                [System.Security.Principal.SecurityIdentifier]
            ).Value
            if (-not $expectedRights.ContainsKey($sid) -or
                $sid -eq $RetiredServiceSid -or $sid -eq "S-1-5-19" -or
                $rule.AccessControlType -ne `
                    [System.Security.AccessControl.AccessControlType]::Allow -or
                [int]$rule.FileSystemRights -ne $expectedRights[$sid] -or
                $rule.InheritanceFlags -ne $expectedInheritance -or
                $rule.PropagationFlags -ne `
                    [System.Security.AccessControl.PropagationFlags]::None -or
                $rule.IsInherited) {
                throw "$protectedPath has an unexpected preserved-state ACE for $sid."
            }
            $expectedRights.Remove($sid) | Out-Null
        }
        if ($expectedRights.Count -ne 0) {
            throw "$protectedPath is missing a required preserved-state ACE."
        }
    }
}

if ((Test-Path -LiteralPath $installedRoot) -or (Test-Path -LiteralPath $stateRoot) -or
    (Test-Path -LiteralPath $trayShortcut) -or
    (Get-ItemProperty -LiteralPath $trayRunKey -Name $trayRunName `
        -ErrorAction SilentlyContinue) -or
    (Get-Service -Name "UnionCAgent" -ErrorAction SilentlyContinue)) {
    throw "The disposable runner is not clean enough for the Windows MSI lifecycle test."
}

$currentPackages = @(Get-ChildItem -LiteralPath $ArtifactDirectory -Filter "*.msi")
if ($currentPackages.Count -ne 1) {
    throw "Expected exactly one release MSI; found $($currentPackages.Count)."
}
$currentMsi = $currentPackages[0].FullName

# An unowned pre-existing state root must never be handed to LocalService.
New-Item -ItemType Directory -Path $stateRoot | Out-Null
$preseed = Join-Path $stateRoot "attacker-controlled.json"
Set-Content -LiteralPath $preseed -Value "must remain untrusted"
Invoke-Msi /i $currentMsi "reject-preseeded-state" -ExpectFailure
if (-not (Test-Path -LiteralPath $preseed -PathType Leaf)) {
    throw "Rejected install modified the untrusted pre-existing state tree."
}
Remove-Item -LiteralPath $stateRoot -Recurse -Force

# A foreign same-name SCM service must likewise remain untouched.
New-Service -Name "UnionCAgent" -BinaryPathName "$env:SystemRoot\System32\cmd.exe /c exit 0" `
    -StartupType Manual | Out-Null
$foreignPath = (Get-CimInstance Win32_Service -Filter "Name='UnionCAgent'").PathName
Invoke-Msi /i $currentMsi "reject-foreign-service" -ExpectFailure
$remainingForeignPath = (Get-CimInstance Win32_Service -Filter "Name='UnionCAgent'").PathName
if ($remainingForeignPath -ne $foreignPath) {
    throw "Rejected install modified the foreign SCM service."
}
& sc.exe delete UnionCAgent | Out-Host
if ($LASTEXITCODE -ne 0) { throw "Could not remove the smoke-test foreign service." }
for ($attempt = 0; $attempt -lt 30; $attempt++) {
    if (-not (Get-Service -Name "UnionCAgent" -ErrorAction SilentlyContinue)) { break }
    Start-Sleep -Milliseconds 500
}
if (Get-Service -Name "UnionCAgent" -ErrorAction SilentlyContinue) {
    throw "Smoke-test foreign service was not deleted."
}

Invoke-Msi /i $currentMsi "fresh-install"
Assert-ServiceRunning
Assert-StateAcl
Assert-TrayIntegration
Assert-ArpVersion $ProductVersion
$marker = Join-Path $stateRoot "release-lifecycle-marker"
Set-Content -LiteralPath $marker -Value "must survive ordinary uninstall"

$installedServiceSid = (New-Object System.Security.Principal.NTAccount(
    "NT SERVICE", "UnionCAgent"
)).Translate([System.Security.Principal.SecurityIdentifier]).Value
$trayBeforeUninstall = Start-TrayForRemovalSmoke
Invoke-Msi /x $currentMsi "preserve-uninstall"
if (-not $trayBeforeUninstall.WaitForExit(30000)) {
    throw "MSI uninstall did not gracefully close the running tray before file removal."
}
if (Get-Service -Name "UnionCAgent" -ErrorAction SilentlyContinue) {
    throw "Agent service survived ordinary uninstall."
}
if (Test-Path -LiteralPath $installedRoot) {
    throw "Program directory survived ordinary uninstall."
}
if ((Test-Path -LiteralPath $trayShortcut) -or
    (Get-ItemProperty -LiteralPath $trayRunKey -Name $trayRunName `
        -ErrorAction SilentlyContinue)) {
    throw "Tray startup integration survived ordinary uninstall."
}
if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
    throw "Ordinary uninstall removed Agent state."
}
if ((Test-Path -LiteralPath $installJournal) -or
    (Test-Path -LiteralPath $uninstallJournal) -or
    (Test-Path -LiteralPath $purgeQuarantine)) {
    throw "Ordinary uninstall left a transaction journal or purge quarantine."
}
Assert-PreservedStateAcl $installedServiceSid

Invoke-Msi /i $currentMsi "reinstall"
Assert-ServiceRunning
Assert-StateAcl
Assert-TrayIntegration
Assert-ArpVersion $ProductVersion
Invoke-Msi /x $currentMsi "purge-uninstall" "PURGE=1"
Assert-AgentCompletelyAbsent
