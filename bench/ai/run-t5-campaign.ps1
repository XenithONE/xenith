# 0011 tier-5 campaign runner — resume-safe: completed runs are skipped by
# the ledger, so this script can be stopped and rerun any number of times.
# Usage:  powershell -File bench\ai\run-t5-campaign.ps1   (from the repo root
# or anywhere — paths are absolute below).
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$exe = Join-Path $root "compiler\target-bench\debug\xenith-bench.exe"
if (-not (Test-Path $exe)) {
    Write-Error "xenith-bench.exe not found — build it first: cargo build -p xenith-bench --target-dir target-bench (in compiler/)"
    exit 1
}
$conds = "t5-guide-on", "t5-guide-off", "t5-api-on", "t5-api-off", "t5-none-on", "t5-none-off"
$models = "codex", "grok", "agy", "opencode", "opencode-deepseek", "opencode-nemotron", "cursor"
$jobs = foreach ($m in $models) {
    Start-Job -Name "t5-$m" -ArgumentList $root, $exe, $m, ($conds -join ",") -ScriptBlock {
        param($root, $exe, $m, $c)
        Set-Location $root
        foreach ($cond in $c -split ",") { & $exe run --model $m --condition $cond 2>&1 }
    }
}
Write-Host "7 model jobs running in parallel. Ctrl+C stops the waiter; jobs keep running until done."
$jobs | Wait-Job | Receive-Job
Write-Host "campaign burst finished — run 'xenith-bench summarize' to regenerate the tables."
