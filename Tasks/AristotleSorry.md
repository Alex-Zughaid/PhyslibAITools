# Task: Close a proof with Aristotle

You're working in a Lean 4 repository (Physlib by default). Your job is to take
EXACTLY ONE proof and have **Aristotle** — Harmonic's Lean prover, which is
considerably stronger at closing Lean goals than you are — write it, then verify
the result and prepare it for a pull request.

You are the navigator and the reviewer here, not the prover. Do not try to prove
the goal yourself before asking Aristotle; that's the whole point of this task.

1. Choose a target — but FIRST, before you claim anything, check it isn't already
   being worked on. Run
     gh pr list --repo leanprover-community/physlib --state open --limit 1000
   and inspect likely candidates (gh pr view <n> --json files, or gh pr diff <n>).
   Don't pick a declaration an open PR is already touching. If gh can't reach the
   API, say so and carry on. Then pick ONE of:
     - an existing `sorry` in the source, or
     - a single long or ugly proof worth re-deriving from scratch.
   If I gave you directions naming a file or declaration, use that instead of
   searching — those directions override this step.
   This run is about that one proof and nothing else.

2. Stub it. In the file, replace the proof body of your chosen declaration with
   `sorry`, leaving the STATEMENT (name, binders, type) byte-for-byte identical.
   Change nothing else in the file.

3. Hand it to Aristotle:

     Scripts/aristotle-prove.sh <path/to/File.lean> --repo-root <repo root>

   Add `--prompt "..."` if the default ("fill in every sorry, don't touch any
   statement") needs narrowing — for instance to name the one declaration you
   care about, or to say which lemmas are available.

   The script prints a unified diff of what Aristotle sent back and writes
   NOTHING to the working tree. This can take a long time — a hard goal can run
   for hours. Do not give up on it early, and do not background it and walk
   away: this is a one-shot session, so poll it in the foreground until it
   actually returns.

4. Apply the proof by hand, from the diff. Read it before you paste it. Reject it
   and say so if it changes any statement, adds declarations, introduces new
   imports, or leans on `sorry`/`native_decide` to get through.

5. Verify: run `lake build` (or the relevant `lake build Physlib` /
   `lake build QuantumInfo` target) and confirm it succeeds. The proof must close
   all goals with no `sorry`, no new axioms, and no new errors or warnings. If
   Lean LSP tools (lean-lsp-mcp) are available, use them to read goal states and
   diagnostics rather than guessing.

6. If Aristotle's proof doesn't build, you may go back to step 3 once with a
   sharper prompt (tell it what failed). If it still doesn't build, restore the
   original proof, leave the tree clean, and report what happened — do not
   quietly fall back to writing the proof yourself, and do not leave the file
   half-edited.

When you're done, tell me which declaration you targeted (and in which file),
show the before/after proof, and paste the final build output. Do NOT commit,
push, or open a pull request yourself — the harness does that after you exit.
