# Dispatch one prompt to one subscription CLI and print its reply.
#
# The bench runner calls this instead of spawning the CLIs directly: the
# flag conventions differ per tool and two of them break on subtle argument
# order, so the knowledge lives in exactly one place. Requires the CLIs to be
# installed and authenticated; runs are local by design — see README.md for
# why the benchmark never runs in CI.

param(
    [Parameter(Mandatory)][ValidateSet(
        'codex', 'grok', 'agy', 'opencode',
        'opencode-deepseek', 'opencode-nemotron', 'cursor'
    )][string]$Cli,
    [Parameter(Mandatory)][string]$PromptFile
)

$prompt = Get-Content -Raw $PromptFile

switch ($Cli) {
    # `--skip-git-repo-check` is required outside a trusted directory.
    'codex' { codex exec --skip-git-repo-check $prompt }
    'grok' { grok -p $prompt }
    # `--print` must come after any other flags; putting `-p` first makes it
    # swallow the next flag as its value.
    'agy' { agy --print $prompt }
    # The default model is a video model; a text model must be named. The
    # extra variants reach different model families through the same CLI.
    'opencode' { opencode run --model openai/gpt-5.6-terra $prompt }
    'opencode-deepseek' { opencode run --model opencode/deepseek-v4-flash-free $prompt }
    'opencode-nemotron' { opencode run --model opencode/nemotron-3-ultra-free $prompt }
    # Auto mode (no --model) routes each call to whichever model Cursor
    # picks — a mixture by design. `--mode ask` keeps it a text reply
    # rather than an agent with shell access.
    'cursor' { cursor-agent --mode ask --output-format text -p $prompt }
}
