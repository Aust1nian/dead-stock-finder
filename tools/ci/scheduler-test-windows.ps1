# Scheduler test (spec § Tests, "Scheduler"): installs the app silently for
# the current user (no admin), registers the background check as that user,
# reads the task back (schtasks /query /xml), fires it (schtasks /run), and
# confirms a check ran with no window and that the lock stopped an
# overlapping run. Also registers once as a separate standard user to prove
# creating the task needs no admin rights.
#
# Usage: scheduler-test-windows.ps1 -Installer <setup.exe> -Out <out.json>
param(
  [Parameter(Mandatory = $true)][string]$Installer,
  [Parameter(Mandatory = $true)][string]$Out
)
$ErrorActionPreference = "Continue"
$checks = [System.Collections.Generic.List[object]]::new()
$script:failed = $false
function Check([string]$Name, [string]$Status, [string]$Detail) {
  $checks.Add([ordered]@{ check = $Name; status = $Status; detail = $Detail })
  Write-Host "[$Status] ${Name}: $Detail"
  if ($Status -eq "fail") { $script:failed = $true }
}

$task = "\Dead Stock Finder check ($env:USERNAME)"
$data = Join-Path $env:LOCALAPPDATA "com.tightline.deadstockfinder"
$log = Join-Path $data "logs\check.log"
Remove-Item $data -Recurse -Force -ErrorAction SilentlyContinue

# 1. Install for this user, silently.
$p = Start-Process -FilePath $Installer -ArgumentList "/S" -Wait -PassThru
$exe = Join-Path $env:LOCALAPPDATA "Dead Stock Finder\dead-stock-finder.exe"
if (-not (Test-Path $exe)) {
  $found = Get-ChildItem $env:LOCALAPPDATA -Recurse -Filter dead-stock-finder.exe -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($found) { $exe = $found.FullName }
}
Check "installed" $(if (Test-Path $exe) { "pass" } else { "fail" }) "exit $($p.ExitCode); $exe"
Check "windows" "info" "$([Environment]::OSVersion.VersionString); user $env:USERNAME; session $((Get-Process -Id $PID).SessionId)"

function Run-App([string]$Arguments, [int]$Wait = 60) {
  $proc = Start-Process -FilePath $exe -ArgumentList $Arguments -PassThru -WindowStyle Hidden
  if (-not $proc.WaitForExit($Wait * 1000)) { $proc.Kill(); return -1 }
  return $proc.ExitCode
}

function Wait-Log([string]$Pattern, [int]$Seconds) {
  for ($i = 0; $i -lt $Seconds; $i++) {
    if ((Test-Path $log) -and (Select-String -Path $log -Pattern $Pattern -SimpleMatch -Quiet)) { return $true }
    Start-Sleep -Seconds 1
  }
  return $false
}

# 2. Register.
Run-App "--test-mark-setup" | Out-Null
$regFile = Join-Path $env:RUNNER_TEMP "dsf-register.json"
$code = Run-App "--schedule register --json-out `"$regFile`""
$reg = Get-Content $regFile -Raw -ErrorAction SilentlyContinue
Check "register" $(if ($code -eq 0 -and $reg -match '"task_scheduler"') { "pass" } else { "fail" }) "exit ${code}: $reg"

# 3. Read it back.
$xml = (schtasks /query /tn $task /xml) -join "`n"
# RunLevel is left out when it is the default, least privilege.
$ok = ($xml -match "<LogonTrigger>") -and ($xml -match "<CalendarTrigger>") -and ($xml -match "InteractiveToken") -and ($xml -notmatch "HighestAvailable") -and ($xml -match "--check --trigger scheduled")
Check "schtasks-query" $(if ($ok) { "pass" } else { "fail" }) (($xml -split "`n" | Select-String -Pattern "Trigger>|UserId|LogonType|RunLevel|StartBoundary|Arguments|ExecutionTimeLimit|StartWhenAvailable") -join " ")

# 4. Fire it.
schtasks /run /tn $task | Out-Null
if (Wait-Log "headless check (scheduled) finished" 90) {
  $line = (Select-String -Path $log -Pattern "headless check (scheduled) finished" -SimpleMatch | Select-Object -Last 1).Line
  Check "fired" $(if ($line -match "windows owned by this process: 0") { "pass" } else { "fail" }) $line
} else {
  $info = (schtasks /query /tn $task /v /fo list) -join " "
  Check "fired" "fail" "no scheduled check in the log. $info"
}

# 5. The lock stops an overlapping run.
$holder = Start-Process -FilePath $exe -ArgumentList "--hold-lock 45" -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 3
schtasks /run /tn $task | Out-Null
if (Wait-Log "another check is running" 45) {
  Check "lock" "pass" (Select-String -Path $log -Pattern "another check is running" -SimpleMatch | Select-Object -Last 1).Line
} else {
  Check "lock" "fail" "overlapping run not stopped: $((Get-Content $log -Tail 5 -ErrorAction SilentlyContinue) -join ' | ')"
}
if (-not $holder.HasExited) { $holder.Kill() }

# 6. A standard user can create the task too (no admin): run the installed
#    app as a fresh standard user through a one-off batch task.
$user = "dsfowner"
$password = "Dsf-" + [Guid]::NewGuid().ToString("N")
Get-LocalUser $user -ErrorAction SilentlyContinue | Remove-LocalUser
$secure = ConvertTo-SecureString $password -AsPlainText -Force
$account = New-LocalUser $user -Password $secure -PasswordNeverExpires -AccountNeverExpires
Add-LocalGroupMember -SID "S-1-5-32-545" -Member $user
$isAdmin = [bool](Get-LocalGroupMember -SID "S-1-5-32-544" | Where-Object { $_.SID -eq $account.SID })
$cfg = Join-Path $env:RUNNER_TEMP "rights.inf"
secedit /export /cfg $cfg /areas USER_RIGHTS | Out-Null
$lines = Get-Content $cfg
if ($lines -match "^SeBatchLogonRight") {
  $lines = $lines -replace "^(SeBatchLogonRight = .*)$", "`$1,*$($account.SID)"
} else {
  $lines = $lines -replace "^\[Privilege Rights\]$", "[Privilege Rights]`r`nSeBatchLogonRight = *$($account.SID)"
}
Set-Content $cfg $lines
secedit /configure /db (Join-Path $env:RUNNER_TEMP "rights.sdb") /cfg $cfg /areas USER_RIGHTS | Out-Null
$work = "C:\dsf-sched"
Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $work | Out-Null
Copy-Item $Installer (Join-Path $work "setup.exe")
icacls $work /grant "*S-1-5-32-545:(OI)(CI)M" | Out-Null
$script = @"
`$ErrorActionPreference = 'Continue'
Start-Process -FilePath '$work\setup.exe' -ArgumentList '/S' -Wait
`$exe = Join-Path `$env:LOCALAPPDATA 'Dead Stock Finder\dead-stock-finder.exe'
& `$exe --test-mark-setup | Out-Null
`$p = Start-Process -FilePath `$exe -ArgumentList '--schedule register --json-out $work\user-register.json' -Wait -PassThru
whoami /groups | Out-File '$work\user-groups.txt'
"@
Set-Content (Join-Path $work "as-user.ps1") $script
schtasks /create /tn "dsf-as-user" /tr "powershell -NoProfile -ExecutionPolicy Bypass -File $work\as-user.ps1" /sc once /st 23:59 /ru $user /rp $password /f | Out-Null
schtasks /run /tn "dsf-as-user" | Out-Null
for ($i = 0; $i -lt 180 -and -not (Test-Path "$work\user-groups.txt"); $i++) { Start-Sleep -Seconds 1 }
$userReg = Get-Content "$work\user-register.json" -Raw -ErrorAction SilentlyContinue
$elevated = (Get-Content "$work\user-groups.txt" -Raw -ErrorAction SilentlyContinue) -match "S-1-5-32-544.*Enabled group"
Check "standard-user" $(if ($isAdmin -or $elevated) { "fail" } else { "pass" }) "$user, not in Administrators"
Check "standard-user-register" $(if ($userReg -match '"task_scheduler"') { "pass" } elseif ($userReg -match '"task_scheduler_daily_and_run_key"|"run_key_only"') { "info" } else { "fail" }) "$userReg"
$userXml = (schtasks /query /tn "\Dead Stock Finder check ($user)" /xml 2>$null) -join "`n"
Check "standard-user-task" $(if ($userXml -match "<LogonTrigger>" -and $userXml -match "<CalendarTrigger>") { "pass" } else { "info" }) (($userXml -split "`n" | Select-String -Pattern "Trigger>|UserId|LogonType") -join " ")
schtasks /delete /tn "\Dead Stock Finder check ($user)" /f 2>$null | Out-Null
schtasks /delete /tn "dsf-as-user" /f | Out-Null

# 7. Unregister, then uninstall; nothing of the schedule is left.
Run-App "--schedule unregister" | Out-Null
schtasks /query /tn $task 2>$null | Out-Null
Check "unregister" $(if ($LASTEXITCODE -ne 0) { "pass" } else { "fail" }) "task query exit $LASTEXITCODE"
$uninstaller = Join-Path (Split-Path $exe) "uninstall.exe"
if (Test-Path $uninstaller) {
  Start-Process -FilePath $uninstaller -ArgumentList "/S" -Wait | Out-Null
  Start-Sleep -Seconds 3
}
$run = Get-ItemProperty "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "DeadStockFinderCheck" -ErrorAction SilentlyContinue
Check "uninstalled" $(if (-not (Test-Path $exe) -and -not $run) { "pass" } else { "fail" }) "exe present: $(Test-Path $exe); Run entry: $([bool]$run)"
Check "log" "info" ((Get-Content $log -Tail 20 -ErrorAction SilentlyContinue) -join " | ")

[ordered]@{ runner = $env:RUNNER_NAME; failed = $script:failed; checks = $checks } | ConvertTo-Json -Depth 4 | Set-Content $Out
if ($script:failed) { exit 1 }
exit 0
