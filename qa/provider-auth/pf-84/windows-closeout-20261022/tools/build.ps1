$ErrorActionPreference = "Continue" # Windows PowerShell 5 wraps native stderr as errors; native exit codes are checked below.
$root = "C:\CorbanuQA\closeout-20261022"
$commit = "36ca28ce2ef00d9aae0e448c8f1ba264b5103608"
New-Item -ItemType Directory -Force -Path $root | Out-Null
$env:CARGO_HOME = "C:\CorbanuQA\cargo"
$env:RUSTUP_HOME = "C:\CorbanuQA\rustup"
$env:CARGO_TARGET_DIR = "$root\target"
Remove-Item Env:CORBANU_HOME,Env:CODEX_HOME,Env:PFTERMINAL_HOME -ErrorAction SilentlyContinue
$env:CORBANU_TEST_NO_NATIVE_KEYRING = "1"
$env:Path = "C:\CorbanuQA\cargo\bin;C:\CorbanuQA\tools\bin;" + $env:Path
& "C:\CorbanuQA\cargo\bin\rustup.exe" toolchain install 1.99.1 --profile minimal --no-self-update *> "$root\toolchain.log"
if ($LASTEXITCODE -ne 0) { throw "Toolchain install failed; inspect toolchain.log" }
& git -C C:\CorbanuQA\src fetch origin $commit *> "$root\fetch.log"
if ($LASTEXITCODE -ne 0) { throw "Source fetch failed" }
if (!(Test-Path "$root\src")) {
    & git -C C:\CorbanuQA\src worktree add --detach "$root\src" $commit *> "$root\worktree.log"
    if ($LASTEXITCODE -ne 0) { throw "Worktree creation failed" }
}
$actual = (& git -C "$root\src" rev-parse HEAD).Trim()
if ($actual -ne $commit) { throw "Unexpected candidate commit" }
# A reused worktree must be clean (no tracked changes, no untracked non-ignored files) so BUILD_OK is attributable to $commit.
$dirty = & git -C "$root\src" status --porcelain --untracked-files=all
if ($LASTEXITCODE -ne 0) { throw "Source status check failed" }
if ($dirty) { throw "Source worktree is not clean; refusing to build. Inspect or recreate $root\src" }
Set-Location "$root\src"
Write-Output "BUILD_START $commit"
& "C:\CorbanuQA\cargo\bin\cargo.exe" +1.99.1 build --locked --manifest-path codex-rs/Cargo.toml --profile dev-small -j8 -p codex-cli --bin corbanu *> "$root\build.log"
$buildExit = $LASTEXITCODE
"BUILD_EXIT=$buildExit" | Set-Content "$root\build-status.txt"
if ($buildExit -ne 0) { throw "Build failed; inspect build.log" }
Write-Output "BUILD_OK $commit"
& "$root\target\dev-small\corbanu.exe" --version
if ($LASTEXITCODE -ne 0) { throw "Version smoke failed" }
Get-FileHash "$root\target\dev-small\corbanu.exe" -Algorithm SHA256 | Select-Object Hash | Format-List
