$ErrorActionPreference = "Stop"
$root = "C:\CorbanuQA\closeout-20261022"
$fixture = Join-Path $root ("fixture-" + [Guid]::NewGuid().ToString("N"))
$qaHome = Join-Path $fixture "profile"
$workspace = Join-Path $fixture "workspace"
$binary = "$root\target\dev-small\corbanu.exe"
$script:canary = "qa-synthetic-" + [Guid]::NewGuid().ToString("N")
$script:defaultCanary = "qa-default-" + [Guid]::NewGuid().ToString("N")
$results = New-Object System.Collections.Generic.List[string]
$utf8 = New-Object System.Text.UTF8Encoding($false)
foreach ($dir in @($qaHome,$workspace,"$fixture\tmp","$fixture\appdata","$fixture\localappdata")) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
}
$config = @'
model = "gpt-6"
model_provider = "qa"
cli_auth_credentials_store = "file"
suppress_unstable_features_warning = true
[features]
named_accounts = true
[model_providers.qa]
name = "OpenAI-compatible isolated QA transport"
base_url = "http://127.0.0.1:1/v1"
wire_api = "responses"
requires_openai_auth = false
env_key = "QA_DEFAULT_API_KEY"
request_max_retries = 0
stream_max_retries = 0
'@
[IO.File]::WriteAllText("$qaHome\config.toml", $config, $utf8)
$auth = @{ OPENAI_API_KEY = $script:defaultCanary } | ConvertTo-Json
[IO.File]::WriteAllText("$qaHome\auth.json", $auth, $utf8)
$beforeAuth = (Get-FileHash "$qaHome\auth.json" -Algorithm SHA256).Hash

function Invoke-Corbanu([string[]]$Argv, [string]$InputValue = "") {
    $start = New-Object Diagnostics.ProcessStartInfo
    $start.FileName = $binary
    $start.Arguments = ($Argv | ForEach-Object { '"' + $_.Replace('"','\"') + '"' }) -join " "
    $start.WorkingDirectory = $workspace
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.EnvironmentVariables.Clear()
    foreach ($name in @("SystemRoot","WINDIR","COMSPEC","PATH")) {
        $value = [Environment]::GetEnvironmentVariable($name)
        if ($value) { $start.EnvironmentVariables[$name] = $value }
    }
    foreach ($name in @("HOME","USERPROFILE")) { $start.EnvironmentVariables[$name] = $fixture }
    foreach ($name in @("CORBANU_HOME","CODEX_HOME","PFTERMINAL_HOME")) { $start.EnvironmentVariables[$name] = $qaHome }
    $start.EnvironmentVariables["APPDATA"] = "$fixture\appdata"
    $start.EnvironmentVariables["LOCALAPPDATA"] = "$fixture\localappdata"
    $start.EnvironmentVariables["TEMP"] = "$fixture\tmp"
    $start.EnvironmentVariables["TMP"] = "$fixture\tmp"
    $start.EnvironmentVariables["CORBANU_TEST_NO_NATIVE_KEYRING"] = "1"
    $start.EnvironmentVariables["OPENAI_API_KEY"] = $script:defaultCanary
    $start.EnvironmentVariables["QA_DEFAULT_API_KEY"] = $script:defaultCanary
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $start
    [void]$process.Start()
    $outTask = $process.StandardOutput.ReadToEndAsync()
    $errTask = $process.StandardError.ReadToEndAsync()
    if ($InputValue) { $process.StandardInput.WriteLine($InputValue) }
    $process.StandardInput.Close()
    if (!$process.WaitForExit(60000)) {
        $process.Kill()
        throw "Command exceeded the bounded 60-second deadline"
    }
    $text = $outTask.Result + $errTask.Result
    $exitCode = $process.ExitCode
    $process.Dispose()
    if ($text.Contains($script:canary) -or $text.Contains($script:defaultCanary)) {
        throw "Synthetic credential appeared in command output; output withheld"
    }
    return @{ Code=$exitCode; Text=$text }
}
function Assert-Result([bool]$Condition, [string]$Label) {
    if (!$Condition) { throw "FAIL: $Label" }
    $results.Add("PASS: $Label")
}
function Vault-Metadata {
    return ((Get-ChildItem "$qaHome\secrets" -File -Recurse | Sort-Object FullName | ForEach-Object {
        $_.FullName + "|" + $_.Length + "|" + $_.LastWriteTimeUtc.Ticks
    }) -join "`n")
}

$ok = $false
try {
    $r = Invoke-Corbanu @("account","list")
    Assert-Result ($r.Code -eq 0 -and $r.Text.Contains("No named accounts")) "empty named-account list"
    $r = Invoke-Corbanu @("account","add","qa","work") $script:canary
    Assert-Result ($r.Code -eq 0 -and $r.Text.Contains("Saved")) "add API-key account through stdin"
    Assert-Result (Test-Path "$qaHome\secrets\keyring-fallback") "debug fixture uses profile-local keyring fallback"
    $r = Invoke-Corbanu @("account","list","qa")
    Assert-Result ($r.Code -eq 0 -and $r.Text.Contains("work") -and !$r.Text.Contains($script:canary)) "list shows account metadata without its credential"
    $metaBefore = Vault-Metadata
    $r = Invoke-Corbanu @("account","list","qa")
    $metaAfter = Vault-Metadata
    Assert-Result ($r.Code -eq 0 -and $metaBefore -eq $metaAfter) "repeated account list does not rewrite vault files"
    $r = Invoke-Corbanu @("account","rename","qa","work","renamed")
    Assert-Result ($r.Code -eq 0 -and $r.Text.Contains("renamed")) "rename existing account"
    $r = Invoke-Corbanu @("account","list","qa")
    Assert-Result ($r.Code -eq 0 -and $r.Text.Contains("renamed") -and !$r.Text.Contains("work")) "renamed account listed under its new name"
    foreach ($invalid in @("default","BadName","../escape")) {
        $r = Invoke-Corbanu @("account","add","qa",$invalid) $script:canary
        Assert-Result ($r.Code -ne 0) ("reject invalid/reserved account name " + $invalid)
    }
    $r = Invoke-Corbanu @("login","--account","signed","--with-api-key") $script:canary
    Assert-Result ($r.Code -eq 0) "named OpenAI API-key login through stdin"
    $r = Invoke-Corbanu @("account","add","qa","spare") $script:canary
    Assert-Result ($r.Code -eq 0) "add a second account for the same provider"
    $r = Invoke-Corbanu @("account","rename","qa","renamed","spare")
    Assert-Result ($r.Code -ne 0) "rename refuses to overwrite an existing account"
    $r = Invoke-Corbanu @("account","list","qa")
    Assert-Result ($r.Code -eq 0 -and $r.Text.Contains("renamed") -and $r.Text.Contains("spare")) "both named accounts survive a rejected rename"
    Assert-Result ((Get-FileHash "$qaHome\auth.json" -Algorithm SHA256).Hash -eq $beforeAuth) "named login leaves disposable default auth unchanged"
    $r = Invoke-Corbanu @("account","remove","qa","renamed")
    Assert-Result ($r.Code -eq 0) "remove renamed account"
    $r = Invoke-Corbanu @("account","remove","openai","signed")
    Assert-Result ($r.Code -eq 0) "remove named login account"
    $r = Invoke-Corbanu @("exec","--account","openai:signed","--skip-git-repo-check","Reply with exactly QA")
    Assert-Result ($r.Code -ne 0 -and $r.Text -match "account.*signed.*not signed in") "removed explicit OpenAI account refuses at startup despite a default credential"
    $r = Invoke-Corbanu @("account","add","qa","scratch") $script:canary
    Assert-Result ($r.Code -eq 0) "add named account for the loopback session provider"
    $r = Invoke-Corbanu @("account","remove","qa","scratch")
    Assert-Result ($r.Code -eq 0) "remove the loopback session provider account"
    $r = Invoke-Corbanu @("exec","--account","scratch","--skip-git-repo-check","Reply with exactly QA")
    Assert-Result ($r.Code -ne 0 -and $r.Text -match "account.*scratch.*not configured") "removed session-provider account refuses rather than using its default credential"
    $r = Invoke-Corbanu @("account","remove","qa","spare")
    Assert-Result ($r.Code -eq 0) "remove second account after collision check"
    $r = Invoke-Corbanu @("account","list")
    Assert-Result ($r.Code -eq 0 -and $r.Text.Contains("No named accounts")) "list is empty after all removals"
    Assert-Result ((Get-FileHash "$qaHome\auth.json" -Algorithm SHA256).Hash -eq $beforeAuth) "default auth remains unchanged after all checks"
    $ok = $true
} catch {
    # Exception text is our own bounded assertion labels; never print child output.
    $results.Add($_.Exception.Message)
    if ($r -and $r.Text) {
        $safeError = $r.Text.Replace($script:canary, "[synthetic redacted]").Replace($script:defaultCanary, "[synthetic redacted]")
        [IO.File]::WriteAllText("$root\\last-command-error.txt", $safeError, $utf8)
        $results.Add("Command exit code: " + $r.Code + "; sanitized diagnostic saved")
    }
} finally {
    if ($fixture.StartsWith("$root\fixture-")) {
        Remove-Item -LiteralPath $fixture -Recurse -Force
        $results.Add("CLEANUP: disposable fixture removed")
    }
    $results | Set-Content "$root\account-checks.txt"
    $script:canary = $null
    $script:defaultCanary = $null
}
$results | ForEach-Object { Write-Output $_ }
if (!$ok) { exit 1 }
