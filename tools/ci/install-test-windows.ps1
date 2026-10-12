# Install test (spec § CI and install tests, Windows): the installer exactly
# as a browser downloads it (Mark of the Web), installed silently by a
# standard user. Checks it never asks for admin and (when signed) that its
# signature is valid; runs --self-test from the installed copy; registers
# the background check as that user. Then installs the newer build over it
# and proves the connection, the cache and the background check are exactly
# as they were; then uninstalls and checks the task and the app are gone
# while the cache stays (the owner didn't tick "delete app data").
#
# Usage: install-test-windows.ps1 -Old <setup.exe> -New <setup.exe> -Out <out.json> [-Signed]
param(
  [Parameter(Mandatory = $true)][string]$Old,
  [Parameter(Mandatory = $true)][string]$New,
  [Parameter(Mandatory = $true)][string]$Out,
  [switch]$Signed
)
$ErrorActionPreference = "Continue"
$checks = [System.Collections.Generic.List[object]]::new()
$script:failed = $false
function Check([string]$Name, [string]$Status, [string]$Detail) {
  $checks.Add([ordered]@{ check = $Name; status = $Status; detail = $Detail })
  Write-Host "[$Status] ${Name}: $Detail"
  if ($Status -eq "fail") { $script:failed = $true }
}

$work = "C:\dsf-install-test"
Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $work | Out-Null
Copy-Item $Old "$work\old-setup.exe"
Copy-Item $New "$work\new-setup.exe"
icacls $work /grant "*S-1-5-32-545:(OI)(CI)M" | Out-Null

# 1. Downloaded from the internet (zone 3), no admin, signature.
foreach ($f in @("old-setup.exe", "new-setup.exe")) {
  Set-Content -Path "$work\$f" -Stream Zone.Identifier -Value "[ZoneTransfer]`r`nZoneId=3`r`nHostUrl=https://get.tightline.systems/deadstock/$f"
}
Check "mark-of-the-web" "pass" ((Get-Content -Path "$work\new-setup.exe" -Stream Zone.Identifier) -join " ")
Check "installer-size" "info" "$((Get-Item "$work\new-setup.exe").Length) bytes"
$bytes = [System.IO.File]::ReadAllBytes("$work\new-setup.exe")
$head = [System.Text.Encoding]::ASCII.GetString($bytes, 0, [Math]::Min($bytes.Length, 4MB))
if ($head -match 'requestedExecutionLevel\s+level="(\w+)"') {
  Check "execution-level" $(if ($Matches[1] -eq "asInvoker") { "pass" } else { "fail" }) "requestedExecutionLevel=$($Matches[1])"
} else {
  Check "execution-level" "fail" "no requestedExecutionLevel in the manifest"
}
$sig = Get-AuthenticodeSignature "$work\new-setup.exe"
if ($Signed) {
  Check "signature" $(if ($sig.Status -eq "Valid") { "pass" } else { "fail" }) "$($sig.Status): $($sig.SignerCertificate.Subject)"
} else {
  Check "signature" "skip" "$($sig.Status): unsigned until a signing identity exists (DECISIONS.md)"
}

# 2. A standard user who may run batch jobs (to act as them here).
$user = "dsfinstall"
$password = "Dsf-" + [Guid]::NewGuid().ToString("N")
Get-LocalUser $user -ErrorAction SilentlyContinue | Remove-LocalUser
$account = New-LocalUser $user -Password (ConvertTo-SecureString $password -AsPlainText -Force) -PasswordNeverExpires -AccountNeverExpires
Add-LocalGroupMember -SID "S-1-5-32-545" -Member $user
$isAdmin = [bool](Get-LocalGroupMember -SID "S-1-5-32-544" | Where-Object { $_.SID -eq $account.SID })
Check "standard-user" $(if ($isAdmin) { "fail" } else { "pass" }) "$user"
$cfg = "$work\rights.inf"
secedit /export /cfg $cfg /areas USER_RIGHTS | Out-Null
$lines = Get-Content $cfg
if ($lines -match "^SeBatchLogonRight") { $lines = $lines -replace "^(SeBatchLogonRight = .*)$", "`$1,*$($account.SID)" }
else { $lines = $lines -replace "^\[Privilege Rights\]$", "[Privilege Rights]`r`nSeBatchLogonRight = *$($account.SID)" }
Set-Content $cfg $lines
secedit /configure /db "$work\rights.sdb" /cfg $cfg /areas USER_RIGHTS | Out-Null

$n = 0
function As-User([string]$Body, [int]$Timeout = 600) {
  $script:n++
  $done = "$work\done-$script:n.txt"
  $ps1 = "$work\step-$script:n.ps1"
  Set-Content $ps1 ("`$ErrorActionPreference = 'Continue'`r`n" + $Body + "`r`nSet-Content '$done' 'done'")
  schtasks /create /tn "dsf-install-step" /tr "powershell -NoProfile -ExecutionPolicy Bypass -File $ps1" /sc once /st 23:59 /ru $user /rp $password /f | Out-Null
  schtasks /run /tn "dsf-install-step" | Out-Null
  for ($i = 0; $i -lt $Timeout -and -not (Test-Path $done); $i++) { Start-Sleep -Seconds 1 }
  schtasks /delete /tn "dsf-install-step" /f | Out-Null
  return (Test-Path $done)
}

# 3. Install the older build as that user; self-test; owner state.
$ok = As-User @"
`$p = Start-Process -FilePath '$work\old-setup.exe' -ArgumentList '/S' -Wait -PassThru
Set-Content '$work\install-exit.txt' `$p.ExitCode
`$exe = Join-Path `$env:LOCALAPPDATA 'Dead Stock Finder\dead-stock-finder.exe'
Set-Content '$work\exe-path.txt' `$exe
`$st = Start-Process -FilePath `$exe -ArgumentList '--self-test','--json-out','$work\selftest-installed.json' -Wait -PassThru
Set-Content '$work\selftest-exit.txt' `$st.ExitCode
& `$exe --test-mark-setup | Out-Null
cmdkey /generic:com.tightline.deadstockfinder/square-production /user:Square /pass:upgrade-test-token | Out-Null
Start-Process -FilePath `$exe -ArgumentList '--schedule','register','--json-out','$work\register.json' -Wait | Out-Null
Get-FileHash (Join-Path `$env:LOCALAPPDATA 'com.tightline.deadstockfinder\cache.sqlite') | Select-Object -ExpandProperty Hash | Set-Content '$work\cache-before.txt'
whoami /groups | Out-File '$work\groups.txt'
"@
$exePath = (Get-Content "$work\exe-path.txt" -ErrorAction SilentlyContinue)
Check "installed-as-standard-user" $(if ($ok -and (Get-Content "$work\install-exit.txt" -ErrorAction SilentlyContinue) -eq "0") { "pass" } else { "fail" }) "$exePath"
$elevated = (Get-Content "$work\groups.txt" -Raw -ErrorAction SilentlyContinue) -match "S-1-5-32-544.*Enabled group"
Check "never-elevated" $(if ($elevated) { "fail" } else { "pass" }) "the install ran with a standard user's token"
$selftest = (Get-Content "$work\selftest-installed.json" -Raw -ErrorAction SilentlyContinue) -replace "\s+", " "
if ($selftest -and $selftest.Length -gt 400) { $selftest = $selftest.Substring(0, 400) }
Check "self-test" $(if ((Get-Content "$work\selftest-exit.txt" -ErrorAction SilentlyContinue) -eq "0") { "pass" } else { "fail" }) "$selftest"
$reg = Get-Content "$work\register.json" -Raw -ErrorAction SilentlyContinue
Check "register" $(if ($reg -match '"task_scheduler"') { "pass" } else { "fail" }) "$reg"
$task = "\Dead Stock Finder check ($user)"
$xml = (schtasks /query /tn $task /xml 2>$null) -join "`n"
Check "task-under-user" $(if ($xml -match "<LogonTrigger>" -and $xml -match "InteractiveToken") { "pass" } else { "fail" }) (($xml -split "`n" | Select-String "UserId|LogonType") -join " ")

# 4. Upgrade: the newer installer over the older one.
$ok = As-User @"
`$p = Start-Process -FilePath '$work\new-setup.exe' -ArgumentList '/S' -Wait -PassThru
Set-Content '$work\upgrade-exit.txt' `$p.ExitCode
cmdkey /list:com.tightline.deadstockfinder/square-production | Out-File '$work\cred-after.txt'
Get-FileHash (Join-Path `$env:LOCALAPPDATA 'com.tightline.deadstockfinder\cache.sqlite') | Select-Object -ExpandProperty Hash | Set-Content '$work\cache-after.txt'
(Get-Item (Join-Path `$env:LOCALAPPDATA 'Dead Stock Finder\dead-stock-finder.exe')).VersionInfo.ProductVersion | Set-Content '$work\version-after.txt'
"@
Check "upgraded" $(if ($ok -and (Get-Content "$work\upgrade-exit.txt" -ErrorAction SilentlyContinue) -eq "0") { "pass" } else { "fail" }) "now $(Get-Content "$work\version-after.txt" -ErrorAction SilentlyContinue)"
$cred = Get-Content "$work\cred-after.txt" -Raw -ErrorAction SilentlyContinue
Check "upgrade-keeps-connection" $(if ($cred -match "com.tightline.deadstockfinder/square-production") { "pass" } else { "fail" }) ($cred -replace "\s+", " ")
$before = Get-Content "$work\cache-before.txt" -ErrorAction SilentlyContinue
$after = Get-Content "$work\cache-after.txt" -ErrorAction SilentlyContinue
Check "upgrade-keeps-cache" $(if ($before -and $before -eq $after) { "pass" } else { "fail" }) "before $before after $after"
$xml = (schtasks /query /tn $task /xml 2>$null) -join "`n"
Check "upgrade-keeps-background-check" $(if ($xml -match "<LogonTrigger>" -and $xml -match "<CalendarTrigger>") { "pass" } else { "fail" }) "task $task"

# 5. Uninstall (silent: the owner didn't ask to delete app data).
$ok = As-User @"
`$u = Join-Path `$env:LOCALAPPDATA 'Dead Stock Finder\uninstall.exe'
Start-Process -FilePath `$u -ArgumentList '/S' -Wait
Start-Sleep -Seconds 5
Test-Path (Join-Path `$env:LOCALAPPDATA 'Dead Stock Finder\dead-stock-finder.exe') | Set-Content '$work\exe-left.txt'
Test-Path (Join-Path `$env:LOCALAPPDATA 'com.tightline.deadstockfinder\cache.sqlite') | Set-Content '$work\cache-left.txt'
cmdkey /delete:com.tightline.deadstockfinder/square-production | Out-Null
"@
$xml = (schtasks /query /tn $task 2>$null)
Check "uninstall-removes-task" $(if ($LASTEXITCODE -ne 0) { "pass" } else { "fail" }) "task query exit $LASTEXITCODE"
Check "uninstall-removes-app" $(if ((Get-Content "$work\exe-left.txt" -ErrorAction SilentlyContinue) -eq "False") { "pass" } else { "fail" }) "exe left: $(Get-Content "$work\exe-left.txt" -ErrorAction SilentlyContinue)"
Check "uninstall-keeps-cache" $(if ((Get-Content "$work\cache-left.txt" -ErrorAction SilentlyContinue) -eq "True") { "pass" } else { "fail" }) "the owner chose to keep app data"

[ordered]@{ runner = $env:RUNNER_NAME; failed = $script:failed; checks = $checks } | ConvertTo-Json -Depth 4 | Set-Content $Out
if ($script:failed) { exit 1 }
exit 0
