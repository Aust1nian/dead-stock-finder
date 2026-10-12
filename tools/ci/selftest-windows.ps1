# Self-test on Windows with only loopback reachable: installs the app for this
# user, blocks every outbound address except 127.0.0.1 and ::1 for the app's
# executable with a firewall rule, runs --self-test, and removes the rule.
# Usage: selftest-windows.ps1 -Installer <setup.exe> -Out <out.json>
param(
  [Parameter(Mandatory = $true)][string]$Installer,
  [Parameter(Mandatory = $true)][string]$Out
)
$ErrorActionPreference = "Stop"
Start-Process -FilePath $Installer -ArgumentList "/S" -Wait
$exe = Join-Path $env:LOCALAPPDATA "Dead Stock Finder\dead-stock-finder.exe"
if (-not (Test-Path $exe)) { throw "not installed at $exe" }
$rule = "dsf-selftest-block"
New-NetFirewallRule -DisplayName $rule -Direction Outbound -Program $exe -Action Block `
  -RemoteAddress @("0.0.0.0-126.255.255.255", "128.0.0.0-255.255.255.255", "::-::0", "::2-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff") | Out-Null
try {
  $p = Start-Process -FilePath $exe -ArgumentList "--self-test", "--json-out", "`"$Out`"" -Wait -PassThru
  Get-Content $Out
  "self-test exit $($p.ExitCode)"
  if ($p.ExitCode -ne 0) { exit 1 }
} finally {
  Remove-NetFirewallRule -DisplayName $rule
}
