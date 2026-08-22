[CmdletBinding()]
param()

Set-StrictMode -Version 2.0
$ErrorActionPreference = "Stop"

$packagingRoot = Split-Path -Parent $PSScriptRoot
$wixRoot = Join-Path $packagingRoot "wix"
$packagePath = Join-Path $wixRoot "Package.wxs"
$projectPath = Join-Path $wixRoot "UnionCAgent.Installer.wixproj"
$buildPath = Join-Path $wixRoot "build-msi.cmd"
$agentRoot = Split-Path -Parent (Split-Path -Parent $packagingRoot)
$workspaceRoot = Split-Path -Parent $agentRoot
$workspacePath = Join-Path $workspaceRoot "Cargo.toml"
$releasePath = Join-Path $workspaceRoot ".github\workflows\release.yml"
$helperPath = Join-Path $agentRoot "src\bin\unionc-agent-maintenance.rs"
$trayPath = Join-Path $agentRoot "src\bin\unionc-agent-tray.rs"
$mainPath = Join-Path $agentRoot "src\main.rs"
$helperSourceRoot = Join-Path $agentRoot "src\windows\maintenance"
$traySourceRoot = Join-Path $agentRoot "src\windows\tray"

foreach ($required in @(
    $packagePath, $projectPath, $buildPath, $workspacePath, $releasePath,
    $helperPath, $trayPath, $mainPath
)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Required WiX packaging file is missing: $required"
    }
}
foreach ($requiredSourceRoot in @($helperSourceRoot, $traySourceRoot)) {
    if (-not (Test-Path -LiteralPath $requiredSourceRoot -PathType Container)) {
        throw "Required Windows Agent source tree is missing: $requiredSourceRoot"
    }
}

function Get-SourceBundle {
    param(
        [Parameter(Mandatory = $true)][string]$EntryPath,
        [Parameter(Mandatory = $true)][string]$SourceRoot
    )

    # The Windows binaries deliberately keep tiny entrypoints and place their
    # implementation in module trees. Packaging invariants must inspect the
    # complete compiled/text-included source, not just the entrypoint wrapper.
    $parts = @(
        Get-Content -LiteralPath $EntryPath -Raw -Encoding UTF8
        Get-ChildItem -LiteralPath $SourceRoot -Recurse -File |
            Sort-Object -Property FullName |
            ForEach-Object {
                Get-Content -LiteralPath $_.FullName -Raw -Encoding UTF8
            }
    )
    return ($parts -join "`n")
}

[xml]$package = Get-Content -LiteralPath $packagePath -Raw
[xml]$project = Get-Content -LiteralPath $projectPath -Raw
$packageText = Get-Content -LiteralPath $packagePath -Raw
$projectText = Get-Content -LiteralPath $projectPath -Raw
$buildText = Get-Content -LiteralPath $buildPath -Raw
$workspaceText = Get-Content -LiteralPath $workspacePath -Raw
$releaseText = Get-Content -LiteralPath $releasePath -Raw
$helperEntryText = Get-Content -LiteralPath $helperPath -Raw -Encoding UTF8
$trayEntryText = Get-Content -LiteralPath $trayPath -Raw -Encoding UTF8
$helperText = Get-SourceBundle -EntryPath $helperPath -SourceRoot $helperSourceRoot
$trayText = Get-SourceBundle -EntryPath $trayPath -SourceRoot $traySourceRoot
$mainText = Get-Content -LiteralPath $mainPath -Raw

foreach ($removedScript in @(
    (Join-Path $packagingRoot "install.ps1"),
    (Join-Path $packagingRoot "uninstall.ps1"),
    (Join-Path $PSScriptRoot "Test-PackagingScripts.ps1")
)) {
    if (Test-Path -LiteralPath $removedScript) {
        throw "Removed compatibility script still exists: $removedScript"
    }
}

foreach ($currentVersionBinding in @(
    'env!("CARGO_PKG_VERSION")',
    'application_version: String',
    'const SNAPSHOT_FORMAT: u32 = 2'
)) {
    if (-not $helperText.Contains($currentVersionBinding)) {
        throw "Windows state markers and transaction journals must be bound to the current package version."
    }
}
foreach ($removedHelperMechanism in @('TaskScheduler', 'ScheduledTask', 'legacy', 'MajorUpgrade')) {
    if ($helperText.Contains($removedHelperMechanism)) {
        throw "Removed compatibility mechanism remains in the maintenance helper: $removedHelperMechanism"
    }
}

$guiSubsystemAttribute = '#![cfg_attr(windows, windows_subsystem = "windows")]'
if (-not $helperEntryText.StartsWith($guiSubsystemAttribute)) {
    throw "The MSI maintenance helper must use the Windows GUI subsystem to avoid flashing console windows."
}
if ([regex]::Matches($helperEntryText, [regex]::Escape($guiSubsystemAttribute)).Count -ne 1) {
    throw "The MSI maintenance helper must declare the Windows GUI subsystem exactly once."
}
if (-not $trayEntryText.StartsWith($guiSubsystemAttribute)) {
    throw "The user-session tray companion must use the Windows GUI subsystem."
}
if ([regex]::Matches($trayEntryText, [regex]::Escape($guiSubsystemAttribute)).Count -ne 1) {
    throw "The tray companion must declare the Windows GUI subsystem exactly once."
}
if ($mainText -match 'windows_subsystem') {
    throw "The interactive Agent executable must not inherit the maintenance helper's GUI subsystem."
}

function Assert-Contains {
    param(
        [Parameter(Mandatory = $true)][string]$Text,
        [Parameter(Mandatory = $true)][string]$Expected,
        [Parameter(Mandatory = $true)][string]$Message
    )
    if (-not $Text.Contains($Expected)) {
        throw $Message
    }
}

# The local page may receive the short-lived authorization key, but it must not
# turn that key into a reusable browser, preference-file, argv, or environment credential.
Assert-Contains $trayText `
    'name=activation_code type=password maxlength=256 required autocomplete=one-time-code spellcheck=false' `
    "The local pairing form must expose a bounded one-time authorization-key field."
Assert-Contains $trayText `
    'placeholder=\"https://unionc.example.com\"' `
    "The local pairing form must show a secure, complete management-console origin."
if ($trayText -match 'COMMAND_OPEN_MANAGEMENT|open_management_console|id=management|\u6253\u5f00 UnionC \u7ba1\u7406\u53f0') {
    throw "The Agent tray and its local configuration page must not provide a direct management-console link."
}
Assert-Contains $trayText 'id=check-connection' `
    "The local Agent page must expose an explicit Server connection check."
Assert-Contains $trayText '"/connection" => server_connection_response' `
    "The local-control router must provide the authenticated connection-check endpoint."
Assert-Contains $trayText 'fn probe_server_connection' `
    "The tray must implement a bounded Server health probe."
Assert-Contains $trayText '.redirect(reqwest::redirect::Policy::none())' `
    "The Server health probe must reject redirects."
Assert-Contains $trayText 'MAX_SERVER_HEALTH_BODY_BYTES' `
    "The Server health response must be bounded before JSON parsing."
if ($trayText -notmatch "(?s)codeInput\.value='';\s*void startOperation\('/pair'.*?activation_code:activationCode") {
    throw "The local page must clear the authorization-key input before starting the asynchronous request."
}
$preferencesMatch = [regex]::Match(
    $trayText,
    '(?s)struct\s+TrayPreferences\s*\{(?<body>.*?)\}'
)
if (-not $preferencesMatch.Success) {
    throw "The tray preference schema could not be located."
}
if ($preferencesMatch.Groups["body"].Value -match '(?i)activation|authorization|secret|token|code') {
    throw "The one-time authorization key must never be persisted in tray preferences."
}
if ($trayText -match '"--(?:activation-code|authorization-key)(?:-[a-z0-9]+)?"') {
    throw "The one-time authorization key must never be placed in process argv."
}
Assert-Contains $trayText '.arg("--tray-activation-stdin")' `
    "The elevated Agent child must receive the authorization key through anonymous stdin."
Assert-Contains $trayText '.stdin(Stdio::piped())' `
    "The elevated pairing broker must create a private stdin pipe for the Agent child."
if ($trayText -notmatch '(?s)\.serve\(process,\s*&server_for_ipc,\s*activation_code\)\s*\.and_then\(\|\(\)\|\s*\{\s*save_preferences') {
    throw "Tray preferences must be saved only after the elevated pairing broker succeeds."
}

# A user-selected exit is deliberately different from an installer/system close.
# Only the former asks for confirmation, goes through UAC, and waits for SCM to
# confirm Stopped. It must never change the service's automatic startup type.
$exitMenuLabel = -join @(
    [char]0x505C, [char]0x6B62, [char]0x672C, [char]0x6B21, [char]0x670D,
    [char]0x52A1, [char]0x5E76, [char]0x9000, [char]0x51FA, [char]0x6258,
    [char]0x76D8, [char]0xFF08, [char]0x91CD, [char]0x542F, [char]0x540E,
    [char]0x81EA, [char]0x52A8, [char]0x8FD0, [char]0x884C, [char]0xFF09
)
Assert-Contains $trayText ('"' + $exitMenuLabel + '"') `
    "The exit command must explain both the current stop and next-boot restart."
$exitCommandBranch = [regex]::Match(
    $trayText,
    '(?s)COMMAND_EXIT\s*=>\s*\{(?<body>.*?)\r?\n\s*\}\s*\r?\n\s*_\s*=>\s*Ok'
)
if (-not $exitCommandBranch.Success) {
    throw "The user exit command branch could not be located."
}
Assert-Contains $exitCommandBranch.Groups["body"].Value 'MB_OKCANCEL' `
    "Stopping the service and exiting the tray must require explicit confirmation."
Assert-Contains $exitCommandBranch.Groups["body"].Value 'request_stop_service_and_exit(window)' `
    "Confirmed user exit must use the dedicated stop-and-exit workflow."
Assert-Contains $exitCommandBranch.Groups["body"].Value 'Automatic' `
    "The confirmation must state that the automatic startup type is preserved."
Assert-Contains $trayText '"--elevated-stop-for-exit"' `
    "User exit must use a fixed, non-generic elevated stop mode."
if ($trayText -match '(?i)ChangeServiceConfig|SERVICE_DISABLED|sc(?:\.exe)?\s+config') {
    throw "Tray exit must never disable or change the Agent service startup type."
}
Assert-Contains $trayText 'const EXIT_SERVICE_STOPPED_MESSAGE: u32 = WM_APP + 43;' `
    "The tray must reserve a private success message for the stop-and-exit workflow."
$exitSuccessBranch = [regex]::Match(
    $trayText,
    '(?s)EXIT_SERVICE_STOPPED_MESSAGE\s*=>\s*\{(?<body>.*?)\r?\n\s*\}\s*\r?\n\s*REFRESH_TRAY_STATUS_MESSAGE\s*=>'
)
if (-not $exitSuccessBranch.Success) {
    throw "The private stop-and-exit completion branch could not be located."
}
foreach ($required in @('EXIT_PENDING.swap(false', 'query_service_state()', 'ServiceState::Stopped', 'DestroyWindow(window)')) {
    Assert-Contains $exitSuccessBranch.Groups["body"].Value $required `
        "The private exit completion message must verify both pending intent and stopped SCM state."
}
$wmCloseBranch = [regex]::Match(
    $trayText,
    '(?s)WM_CLOSE\s*=>\s*\{(?<body>.*?)\r?\n\s*\}\s*\r?\n\s*WM_DESTROY\s*=>'
)
if (-not $wmCloseBranch.Success -or
    [regex]::Matches($trayText, '\bWM_CLOSE\s*=>').Count -ne 1) {
    throw "The generic WM_CLOSE branch could not be located."
}
Assert-Contains $wmCloseBranch.Groups["body"].Value 'DestroyWindow(window)' `
    "Installer/system WM_CLOSE must still close the tray gracefully."
if ($wmCloseBranch.Groups["body"].Value -match '(?i)stop_service|request_stop|launch_elevated|EXIT_SERVICE_STOPPED') {
    throw "Installer/system WM_CLOSE must not invoke the user stop-and-exit workflow."
}

$namespace = New-Object System.Xml.XmlNamespaceManager($package.NameTable)
$namespace.AddNamespace("w", "http://wixtoolset.org/schemas/v4/wxs")
$namespace.AddNamespace("util", "http://wixtoolset.org/schemas/v4/wxs/util")

function Select-One {
    param([Parameter(Mandatory = $true)][string]$XPath)
    $nodes = @($package.SelectNodes($XPath, $namespace))
    if ($nodes.Count -ne 1) {
        throw "Expected exactly one WiX node for ${XPath}; found $($nodes.Count)."
    }
    return $nodes[0]
}

function Assert-Equal {
    param(
        [AllowNull()]$Actual,
        [AllowNull()]$Expected,
        [Parameter(Mandatory = $true)][string]$Message
    )
    if ([Convert]::ToString($Actual) -cne [Convert]::ToString($Expected)) {
        throw "${Message} Expected '$Expected', found '$Actual'."
    }
}

$product = Select-One "/w:Wix/w:Package"
Assert-Equal $product.Scope "perMachine" "The MSI must be per-machine."
Assert-Equal $product.InstallerVersion "500" "The MSI must target MSI 5.0."
Assert-Equal $product.UpgradeCode "{A1AB822F-91BA-4116-A7E3-CE842B93E93C}" `
    "The x64 upgrade family GUID must remain stable."

if (@($package.SelectNodes("/w:Wix/w:Package/w:MajorUpgrade", $namespace)).Count -ne 0) {
    throw "The current-only MSI must not author automatic major-upgrade migration."
}
$otherVersion = Select-One "/w:Wix/w:Package/w:Upgrade/w:UpgradeVersion"
Assert-Equal $otherVersion.Minimum "0.0.0" `
    "Related-product detection must cover every other version in the upgrade family."
Assert-Equal $otherVersion.IncludeMinimum "yes" `
    "The related-product lower bound must be inclusive."
Assert-Equal $otherVersion.OnlyDetect "yes" `
    "Other versions must be detected but never removed or migrated."
Assert-Equal $otherVersion.Property "UNIONC_OTHER_VERSION_FOUND" `
    "The current-only related-product property drifted."
$otherVersionLaunch = Select-One `
    "/w:Wix/w:Package/w:Launch[@Condition='Installed OR NOT UNIONC_OTHER_VERSION_FOUND']"
Assert-Contains $otherVersionLaunch.Message "Uninstall it before installing this version" `
    "The current-only gate must tell operators to remove another version explicitly."

$service = Select-One "//w:ServiceInstall[@Name='UnionCAgent']"
Assert-Equal $service.DisplayName "UnionC Agent" "Unexpected service display name."
Assert-Equal $service.Type "ownProcess" "The Agent must be an own-process service."
Assert-Equal $service.Start "auto" "The Agent service must start automatically."
Assert-Equal $service.Account "NT AUTHORITY\LocalService" `
    "The service must run as LocalService."
Assert-Equal $service.Arguments `
    '--windows-service run --config "[CommonAppDataFolder]UnionC Agent\config.json"' `
    "The SCM entrypoint and fixed config path drifted."

$serviceControl = Select-One "//w:ServiceControl[@Name='UnionCAgent']"
Assert-Equal $serviceControl.Start "install" "MSI must start the service on install."
Assert-Equal $serviceControl.Stop "both" "MSI must stop the service transactionally."
Assert-Equal $serviceControl.Remove "uninstall" "MSI must unregister the service on uninstall."
Assert-Equal $serviceControl.Wait "yes" "MSI must wait for SCM operations."

$failurePolicy = Select-One "//util:ServiceConfig"
foreach ($attribute in @(
    "FirstFailureActionType",
    "SecondFailureActionType",
    "ThirdFailureActionType"
)) {
    Assert-Equal $failurePolicy.GetAttribute($attribute) "restart" `
        "All service failures must request restart."
}
Assert-Equal $failurePolicy.RestartServiceDelayInSeconds "60" `
    "Unexpected service restart delay."

$trayComponent = Select-One "//w:Component[@Id='AgentTrayComponent']"
Assert-Equal $trayComponent.Bitness "always64" "The x64 tray component bitness drifted."
if (@($trayComponent.SelectNodes(".//w:ServiceInstall", $namespace)).Count -ne 0) {
    throw "The user-session tray companion must never be installed as a service."
}
$trayFile = Select-One "//w:File[@Id='TrayExecutable']"
Assert-Equal $trayFile.Source '$(var.TrayExe)' "The MSI tray input variable drifted."
Assert-Equal $trayFile.Name "unionc-agent-tray.exe" "Unexpected installed tray filename."
Assert-Equal $trayFile.KeyPath "yes" "The signed tray executable must be its component key path."
$null = Select-One "//w:Feature[@Id='AgentFeature']/w:ComponentRef[@Id='AgentTrayComponent']"

$trayRun = Select-One `
    "//w:Component[@Id='AgentTrayComponent']/w:RegistryValue[@Name='UnionCAgentTray']"
Assert-Equal $trayRun.Root "HKLM" "Tray login startup must be registered per machine."
Assert-Equal $trayRun.Key "Software\Microsoft\Windows\CurrentVersion\Run" `
    "Tray startup must use the native machine Run key."
Assert-Equal $trayRun.Type "string" "Tray startup must use a string command."
Assert-Equal $trayRun.Value '"[INSTALLFOLDER]unionc-agent-tray.exe" --startup' `
    "Tray startup must use only the fixed installed image and startup mode."

$trayShortcut = Select-One "//w:Shortcut[@Id='AgentTrayStartMenuShortcut']"
Assert-Equal $trayShortcut.Directory "ProgramMenuFolder" `
    "The tray launcher must be available to every user from the per-machine Start menu."
Assert-Equal $trayShortcut.Advertise "yes" `
    "The common Start-menu shortcut must use MSI advertisement instead of a per-user HKCU key path."
Assert-Equal $trayShortcut.Arguments "--open" `
    "The user-invoked Start menu shortcut must explicitly request the configuration page."

$closeTray = Select-One "//util:CloseApplication[@Id='CloseAgentTray']"
Assert-Equal $closeTray.Target "unionc-agent-tray.exe" `
    "The repair/uninstall close target drifted."
Assert-Equal $closeTray.Condition "Installed" `
    "A clean first install must not close unrelated same-name processes."
Assert-Equal $closeTray.CloseMessage "yes" `
    "The tray must receive a graceful close request in the user context."
Assert-Equal $closeTray.ElevatedCloseMessage "yes" `
    "The elevated installer pass must also attempt graceful tray shutdown."
Assert-Equal $closeTray.Timeout "10" "Unexpected tray shutdown timeout."
Assert-Equal $closeTray.RebootPrompt "yes" `
    "A stuck tray must use MSI reboot handling instead of forced termination."
Assert-Equal $closeTray.GetAttribute("TerminateProcess") "" `
    "The MSI must never force-terminate the tray companion."

$purgeProperty = Select-One "//w:Property[@Id='PURGE']"
Assert-Equal $purgeProperty.Secure "yes" "PURGE must survive the client/server MSI boundary."

$testOnlyProperties = @($package.SelectNodes(
    "//w:Property[starts-with(@Id, 'UNIONC_TEST_')]",
    $namespace
))
if ($testOnlyProperties.Count -ne 0) {
    throw "The current-only MSI must not carry upgrade fault-injection properties."
}

if (@($package.SelectNodes("//w:DirectoryRef[@Id='STATEDIRECTORY']/w:Component/w:CreateFolder", $namespace)).Count -ne 0) {
    throw "MSI must not own/recreate the mutable state directory; the native helper owns its transaction."
}

$actions = @($package.SelectNodes("//w:CustomAction", $namespace))
$expectedActions = [ordered]@{
    "RollbackAgentInstall" = @("rollback-install", "rollback", "check")
    "PrepareAgentInstall" = @("prepare-install", "deferred", "check")
    "ApplyAgentInstall" = @("apply-install", "deferred", "check")
    "CommitAgentInstall" = @("commit-install", "commit", "ignore")
    "RollbackUninstallPreflight" = @("rollback-uninstall-preflight", "rollback", "check")
    "PreflightAgentUninstall" = @("preflight-uninstall", "deferred", "check")
    "RollbackPreservedState" = @("rollback-uninstall", "rollback", "check")
    "PreserveAgentState" = @("preserve-state", "deferred", "check")
    "CommitPreservedState" = @("commit-uninstall", "commit", "ignore")
    "RollbackPurgedState" = @("rollback-purge", "rollback", "check")
    "PreparePurgedState" = @("prepare-purge", "deferred", "check")
    "CommitPurgedState" = @("commit-purge", "commit", "ignore")
}
$nativeActions = @($actions | Where-Object {
    $_.GetAttribute("BinaryRef") -eq "UnionCAgentMaintenance.exe"
})
if ($nativeActions.Count -ne $expectedActions.Count) {
    throw "Expected exactly $($expectedActions.Count) native lifecycle custom actions; found $($nativeActions.Count)."
}
if ($actions.Count -ne ($expectedActions.Count + 1)) {
    throw "Only native lifecycle actions and the fixed tray launch may be authored; found $($actions.Count)."
}

foreach ($entry in $expectedActions.GetEnumerator()) {
    $action = Select-One "//w:CustomAction[@Id='$($entry.Key)']"
    Assert-Equal $action.ExeCommand $entry.Value[0] `
        "The command bound to custom action $($entry.Key) drifted."
    Assert-Equal $action.Execute $entry.Value[1] `
        "The execution phase for custom action $($entry.Key) drifted."
    Assert-Equal $action.Return $entry.Value[2] `
        "The return policy for custom action $($entry.Key) drifted."
    Assert-Equal $action.BinaryRef "UnionCAgentMaintenance.exe" `
        "Lifecycle actions must use only the embedded native helper."
    Assert-Equal $action.Impersonate "no" `
        "Privileged lifecycle actions must run in the system install context."
    if ($action.ExeCommand -match '(?i)powershell|cmd(?:\.exe)?|schtasks(?:\.exe)?|sc(?:\.exe)?') {
        throw "Lifecycle action invokes a command shell or inbox CLI: $($action.Id)"
    }
}

if (@($actions | Where-Object {
    -not [string]::IsNullOrEmpty($_.GetAttribute("Error"))
}).Count -ne 0) {
    throw "The MSI must not carry a Type 19 upgrade fault-injection action."
}

$launchTray = Select-One "//w:CustomAction[@Id='LaunchAgentTray']"
Assert-Equal $launchTray.BinaryRef "Wix4UtilCA_X64" `
    "The post-install launch must use the pinned WiX x64 utility helper."
Assert-Equal $launchTray.DllEntry "WixUnelevatedShellExec" `
    "The post-install launch must explicitly obtain the normal Explorer token."
Assert-Equal $launchTray.Execute "immediate" `
    "The tray launch must run outside the privileged deferred lifecycle."
Assert-Equal $launchTray.Impersonate "yes" `
    "The tray launch must run in the invoking user's interactive context."
Assert-Equal $launchTray.Return "ignore" `
    "An unavailable interactive shell must not fail an already committed installation."
foreach ($forbiddenAttribute in @("FileRef", "ExeCommand")) {
    Assert-Equal $launchTray.GetAttribute($forbiddenAttribute) "" `
        "The unelevated tray launch must not expose $forbiddenAttribute."
}
$unelevatedTarget = Select-One "//w:Property[@Id='WixUnelevatedShellExecTarget']"
Assert-Equal $unelevatedTarget.Value '[#TrayExecutable]' `
    "The unelevated launch target must be exactly the installed tray file."

$helperCommandMatches = [regex]::Matches(
    $helperText,
    '"(?<command>[a-z][a-z-]+)"\s*=>\s*[a-z_]+\(&paths\)'
)
$helperCommands = @($helperCommandMatches | ForEach-Object { $_.Groups["command"].Value } | Sort-Object -Unique)
$authoredCommands = @(
    $expectedActions.GetEnumerator() |
        ForEach-Object { $_.Value[0] } |
        Sort-Object -Unique
)
$commandDifference = @(Compare-Object -ReferenceObject $authoredCommands -DifferenceObject $helperCommands)
if ($helperCommands.Count -ne $expectedActions.Count -or $commandDifference.Count -ne 0) {
    throw "MSI/helper command sets differ. Authored: $($authoredCommands -join ', '); helper: $($helperCommands -join ', ')."
}

Assert-Contains $packageText 'Condition="NOT RollbackDisabled"' `
    "Transactional current-version lifecycle changes must reject policy-disabled rollback."
$installCondition = 'NOT REMOVE~="ALL"'
$preflightCondition = 'REMOVE~="ALL"'
$preserveCondition = 'REMOVE~="ALL" AND NOT (PURGE = "1")'
$purgeCondition = 'REMOVE~="ALL" AND PURGE = "1"'
$expectedSequence = [ordered]@{
    "RollbackAgentInstall" = @("Before", "PrepareAgentInstall", $installCondition)
    "PrepareAgentInstall" = @("Before", "StopServices", $installCondition)
    "ApplyAgentInstall" = @("After", "InstallServices", $installCondition)
    "CommitAgentInstall" = @("After", "ApplyAgentInstall", $installCondition)
    "RollbackUninstallPreflight" = @("Before", "PreflightAgentUninstall", $preflightCondition)
    "PreflightAgentUninstall" = @("Before", "StopServices", $preflightCondition)
    "RollbackPreservedState" = @("After", "StopServices", $preserveCondition)
    "PreserveAgentState" = @("After", "RollbackPreservedState", $preserveCondition)
    "CommitPreservedState" = @("After", "PreserveAgentState", $preserveCondition)
    "RollbackPurgedState" = @("After", "StopServices", $purgeCondition)
    "PreparePurgedState" = @("After", "RollbackPurgedState", $purgeCondition)
    "CommitPurgedState" = @("After", "PreparePurgedState", $purgeCondition)
}
$sequenceActions = @($package.SelectNodes("//w:InstallExecuteSequence/w:Custom", $namespace))
if ($sequenceActions.Count -ne ($expectedSequence.Count + 2)) {
    throw "Every lifecycle action, CloseApplications override and tray launch must be sequenced exactly once; found $($sequenceActions.Count) sequence rows."
}
foreach ($entry in $expectedSequence.GetEnumerator()) {
    $sequence = Select-One "//w:InstallExecuteSequence/w:Custom[@Action='$($entry.Key)']"
    $relation = $entry.Value[0]
    $opposite = if ($relation -eq "Before") { "After" } else { "Before" }
    Assert-Equal $sequence.GetAttribute($relation) $entry.Value[1] `
        "The $relation anchor for $($entry.Key) drifted."
    Assert-Equal $sequence.GetAttribute($opposite) "" `
        "The $($entry.Key) sequence row must use exactly one relative anchor."
    Assert-Equal $sequence.Condition $entry.Value[2] `
        "The execution condition for $($entry.Key) drifted."
}

$trayLaunchSequence = Select-One `
    "//w:InstallExecuteSequence/w:Custom[@Action='LaunchAgentTray']"
Assert-Equal $trayLaunchSequence.GetAttribute("After") "InstallFinalize" `
    "The tray may launch only after the MSI transaction commits successfully."
Assert-Equal $trayLaunchSequence.GetAttribute("Before") "" `
    "The tray launch must use exactly one relative sequence anchor."
Assert-Equal $trayLaunchSequence.Condition `
    'NOT Installed AND NOT REMOVE~="ALL" AND UILevel >= 4 AND NOT ReplacedInUseFiles' `
    "Only a fresh interactive install without deferred file replacement may launch the tray."
$closeApplicationsSequence = Select-One `
    "//w:InstallExecuteSequence/w:Custom[@Action='Wix4CloseApplications_X64']"
Assert-Equal $closeApplicationsSequence.GetAttribute("After") "InstallInitialize" `
    "WiX CloseApplications must run inside the transaction before uninstall removes files."
Assert-Equal $closeApplicationsSequence.GetAttribute("Before") "" `
    "The CloseApplications override must use exactly one relative sequence anchor."
Assert-Equal $closeApplicationsSequence.Condition 'VersionNT > 400' `
    "The CloseApplications override must retain the WiX Util platform condition."
Assert-Contains $packageText 'Property="UNIONC_OTHER_VERSION_FOUND"' `
    "A fail-closed other-version detection row is required."
foreach ($removedUpgradeMechanism in @(
    '<MajorUpgrade', 'RemoveExistingProducts', 'UPGRADINGPRODUCTCODE',
    'WIX_UPGRADE_DETECTED', 'UNIONC_TEST_FAIL_AFTER_REMOVE'
)) {
    if ($packageText.Contains($removedUpgradeMechanism)) {
        throw "Removed automatic-upgrade mechanism remains in Package.wxs: $removedUpgradeMechanism"
    }
}

if ($packageText -match '(?i)WixQuietExec|CAQuietExec') {
    throw "The MSI authoring must not use command-shell custom actions."
}
if ($projectText -match '(?i)SuppressIce') {
    throw "MSI ICE validation must not be suppressed."
}
Assert-Contains $projectText 'WixToolset.Sdk/4.0.6' `
    "The WiX SDK version must be pinned for reproducible builds."
$workspaceVersionMatches = [regex]::Matches(
    $workspaceText,
    '(?m)^version\s*=\s*"(?<version>\d+\.\d+\.\d+)"\s*$'
)
if ($workspaceVersionMatches.Count -ne 1) {
    throw "Expected exactly one strict workspace package version; found $($workspaceVersionMatches.Count)."
}
$defaultProductVersions = @($project.Project.PropertyGroup.ProductVersion)
if ($defaultProductVersions.Count -ne 1) {
    throw "Expected exactly one default WiX ProductVersion; found $($defaultProductVersions.Count)."
}
Assert-Equal $defaultProductVersions[0].InnerText `
    $workspaceVersionMatches[0].Groups["version"].Value `
    "The default WiX ProductVersion must match the unionc-agent workspace package version."
Assert-Contains $releaseText 'Where-Object name -eq "unionc-agent"' `
    "The release workflow must resolve the Windows MSI version from unionc-agent Cargo metadata."
Assert-Contains $releaseText '$version = $agentPackage[0].version' `
    "The development MSI version must default to the unionc-agent Cargo package version."
Assert-Contains $releaseText 'if ($agentPackage[0].version -ne $version)' `
    "The release workflow must reject a tag that differs from the unionc-agent Cargo package version."
$productVersionBindings = [regex]::Matches(
    $releaseText,
    '(?m)^\s+PRODUCT_VERSION:\s+\$\{\{\s+steps\.version\.outputs\.version\s+\}\}\s*$'
)
if ($productVersionBindings.Count -ne 2) {
    throw "Expected the resolved MSI version to feed both Windows MSI build steps; found $($productVersionBindings.Count)."
}
$lifecycleVersionBindings = [regex]::Matches(
    $releaseText,
    '(?m)^\s+-ProductVersion "\$\{\{\s+steps\.version\.outputs\.version\s+\}\}"\s*$'
)
if ($lifecycleVersionBindings.Count -ne 1) {
    throw "Expected the resolved MSI version to feed the Windows lifecycle test once; found $($lifecycleVersionBindings.Count)."
}
Assert-Contains $projectText "'^\d+\.\d+\.\d+$'" `
    "The build must enforce a strict three-field MSI version."
Assert-Contains $projectText '.Major) &gt; 255' `
    "The MSI major version field range must be checked."
Assert-Contains $projectText '.Minor) &gt; 255' `
    "The MSI minor version field range must be checked."
Assert-Contains $projectText '.Build) &gt; 65535' `
    "The MSI build version field range must be checked."
Assert-Contains $projectText 'ConsoleToMSBuild="true"' `
    "The WiX project must execute AgentExe to bind the MSI version to its binary."
Assert-Contains $projectText "'`$(DetectedAgentVersion)' != 'unionc-agent `$(ProductVersion)'" `
    "Direct MSBuild callers must fail when ProductVersion differs from AgentExe --version."
if ($buildText -match '(?i)powershell(?:\.exe)?|pwsh(?:\.exe)?') {
    throw "The MSI build entrypoint must not require PowerShell."
}
Assert-Contains $projectText '<TrayExe Condition=' `
    "The WiX project must accept the signed tray executable as an explicit input."
Assert-Contains $projectText "!Exists('`$(TrayExe)')" `
    "The WiX build must fail when the tray executable input is missing."
Assert-Contains $buildText 'TRAY_EXE=%~f4' `
    "The command-line MSI build entrypoint must require the tray executable."
Assert-Contains $buildText '"%AGENT_EXE%" --version' `
    "The command-line MSI build entrypoint must read the Agent binary version."
Assert-Contains $buildText 'unionc-agent %PRODUCT_VERSION%' `
    "The command-line MSI build entrypoint must reject a binary/version mismatch."
if ($buildText.Contains('1.2.3')) {
    throw "The current-only MSI build documentation must not advertise an arbitrary version."
}

Write-Host "WiX MSI authoring passed current-only lifecycle, tray, service, rollback, and purge checks."
