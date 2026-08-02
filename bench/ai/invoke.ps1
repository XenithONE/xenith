# Dispatch one prompt to one subscription CLI and print its reply.
#
# The bench runner calls this instead of spawning the CLIs directly: the
# flag conventions differ per tool and two of them break on subtle argument
# order, so the knowledge lives in exactly one place. Requires the CLIs to be
# installed and authenticated; runs are local by design — see README.md for
# why the benchmark never runs in CI.
#
# Every CLI runs from a neutral, empty directory outside the repository.
# Several of these tools are agents with ambient workspace access even in
# their "print one answer" modes; run from the repo, Cursor's ask mode read
# the field guide and the reference solutions out of the working tree and
# scored a perfect bare run — falsified by re-asking from an empty directory,
# where its Xenith reverted to guessed syntax. The measurement is the prompt,
# so the filesystem must have nothing to say.

param(
    [Parameter(Mandatory)][ValidateSet(
        'codex', 'grok', 'agy', 'opencode',
        'opencode-deepseek', 'opencode-nemotron', 'cursor'
    )][string]$Cli,
    [Parameter(Mandatory)][string]$PromptFile
)

$prompt = Get-Content -Raw $PromptFile

$neutral = Join-Path $env:LOCALAPPDATA 'xenith-bench\neutral'
$null = New-Item -ItemType Directory -Force $neutral
Set-Location $neutral

switch ($Cli) {
    # `--skip-git-repo-check` is required outside a git repository, which the
    # neutral directory deliberately is.
    'codex' { codex exec --skip-git-repo-check $prompt }
    # Web search is on by default and the benchmark language has a public
    # repository — a searching model could find the documentation this
    # condition withholds.
    'grok' { grok --disable-web-search -p $prompt }
    # `--print` must come after any other flags; putting `-p` first makes it
    # swallow the next flag as its value.
    'agy' { agy --print $prompt }
    # The default model is a video model; a text model must be named. The
    # extra variants reach different model families through the same CLI.
    'opencode' { opencode run --model openai/gpt-5.6-terra $prompt }
    'opencode-deepseek' { opencode run --model opencode/deepseek-v4-flash-free $prompt }
    'opencode-nemotron' { opencode run --model opencode/nemotron-3-ultra-free $prompt }
    # Auto mode (no --model) routes each call to whichever model Cursor
    # picks — a mixture by design. `--mode ask` keeps the reply textual, and
    # `--trust` answers the workspace-trust prompt for the (empty) neutral
    # directory, which headless mode cannot ask interactively.
    'cursor' { cursor-agent --trust --mode ask --output-format text -p $prompt }
}
