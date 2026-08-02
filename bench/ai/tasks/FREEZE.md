# tier-4 tasks — frozen before implementation

These six tasks are the practical arm of the separation experiment
(design/0007 §5). They are committed **before** the std slices and the
candidate-ranking work land, so the commit hash is the proof that the tasks
were not tuned to the machinery being measured (0007 §5-3, evaluator
independence).

They live outside `tasks/` because the harness and `xenith-bench verify` read
only that directory, and these references cannot compile until the List/Map/
String slices ship. **The move into `tasks/` must not change task prompts or
expected output.** If a frozen reference turns out to be wrong when `verify`
first runs, the fix lands as its own commit explaining the error — amendments
stay visible, never silent.

Prompts are phrased by outcome, not by API verb, and the filenames are blind
ids (0007 §5-3/§5-4).
