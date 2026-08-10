//! The Aristotle-only run path: Harmonic's Lean prover does the proving, with
//! no agent in the loop at all. The native equivalent of
//! `Scripts/physlib-auto-task.sh --prover aristotle`.
//!
//! It deliberately mirrors `run_task.rs`'s shape (politeness cap -> clone ->
//! branch -> spawn -> `task-run:finished`) and emits the *same*
//! `task-run:event` / `task-run:finished` events, so the run view, activity
//! feed, diff review and PR confirmation on the frontend are reused unchanged
//! and don't need to know which engine produced the diff.
//!
//! Nothing here calls `claude`. The PR text is composed from the diff, and
//! `lake build` is the only thing vetting Aristotle's output - so a failed
//! build ends the run rather than opening a pull request.

use super::run_task::RunTaskFinished;
use super::workspace;
use crate::process;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};

/// The wrapper script, embedded at compile time rather than copied into
/// `resources/`: one source of truth, so a packaged app can never ship a
/// wrapper that's drifted from the one in `Scripts/`.
const ARISTOTLE_PROVE_SH: &str = include_str!("../../../../Scripts/aristotle-prove.sh");

/// How many `sorry`-bearing files to hand over when the user hasn't named one.
/// Every file listed is uploaded and worked on, so this stays small.
const MAX_AUTO_TARGETS: usize = 3;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AristotleRunRequest {
    pub workspace_dir: String,
    pub task_name: String,
    /// Optional free-text steering. When it names `.lean` files that exist in
    /// the checkout, those are the targets; otherwise it's passed along as
    /// extra instructions and the targets are found by looking for `sorry`.
    pub directions: String,
    pub max_open_auto_prs: u32,
    pub aristotle_api_key: Option<String>,
}

/// One line of wrapper/build output, shaped like the `{"type":"raw"}` wrapper
/// `spawn_claude_streaming` falls back to, so `describeEvent` on the frontend
/// renders it with no special-casing.
fn emit_line(app: &AppHandle, text: impl Into<String>) {
    let _ = app.emit("task-run:event", serde_json::json!({ "type": "raw", "text": text.into() }));
}

fn finish_with_error(app: &AppHandle, branch: String, error: Option<String>) {
    let _ = app.emit(
        "task-run:finished",
        RunTaskFinished { could_finish: false, branch, diff: None, pr_title: None, pr_body: None, error },
    );
}

/// True if `text` contains `sorry` as a standalone Lean token, rather than as
/// part of a longer identifier.
///
/// A plain `contains("sorry")` is not good enough, and got this wrong in a
/// real run: Physlib's own lint tooling is full of `sorryAx`, `sorryful` and
/// `sorryPseudoCheck`, so the naive check happily handed Aristotle three lint
/// scripts with no proofs in them at all. Mirrors the word-boundary regex the
/// shell harness already used: `(^|[^a-zA-Z_])sorry([^a-zA-Z_]|$)`.
fn has_bare_sorry(text: &str) -> bool {
    let boundary = |c: Option<char>| !matches!(c, Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == '\'');
    text.match_indices("sorry").any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + "sorry".len()..].chars().next();
        boundary(before) && boundary(after)
    })
}

/// Directories never worth searching or proving in: build output, VCS, and
/// the metaprogramming/lint scripts (which are tooling, not physics).
fn is_skipped_dir(name: &str) -> bool {
    matches!(name, ".lake" | ".git" | ".github" | "scripts" | "Scripts" | ".vscode" | "docs")
}

/// Files the run should target: whatever the directions point at if that
/// resolves to something real, otherwise up to `MAX_AUTO_TARGETS` files with a
/// genuine `sorry` in them.
///
/// Directions may name a file *or* a directory - "prove the sorries in
/// Physlib/Electromagnetism/Distributional" is a perfectly reasonable thing to
/// type, and an earlier version silently ignored it because the token didn't
/// end in `.lean`, then fell back to the search and picked the wrong files.
///
/// Walked in Rust rather than shelling out to `grep -rl`, which isn't present
/// on Windows.
fn find_targets(dir: &Path, directions: &str) -> Vec<PathBuf> {
    let mut named = Vec::new();
    for token in directions.split_whitespace() {
        let token = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '.' | '/' | '_' | '-'));
        if token.is_empty() || !token.contains('/') && !token.ends_with(".lean") {
            continue;
        }
        let candidate = dir.join(token);
        if candidate.is_file() && token.ends_with(".lean") {
            named.push(candidate);
        } else if candidate.is_dir() {
            // A directory: take the .lean files under it that actually have a
            // sorry, so naming a folder doesn't upload proofs that are done.
            collect_sorry_files(&candidate, &mut named, MAX_AUTO_TARGETS);
        }
    }
    if !named.is_empty() {
        named.truncate(MAX_AUTO_TARGETS);
        return named;
    }

    let mut found = Vec::new();
    collect_sorry_files(dir, &mut found, MAX_AUTO_TARGETS);
    found
}

fn collect_sorry_files(dir: &Path, out: &mut Vec<PathBuf>, limit: usize) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        if out.len() >= limit {
            return;
        }
        let Ok(entries) = std::fs::read_dir(&current) else { continue };
        for entry in entries.flatten() {
            if out.len() >= limit {
                return;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !is_skipped_dir(&name) {
                    stack.push(path);
                }
            } else if name.ends_with(".lean") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if has_bare_sorry(&text) {
                        out.push(path);
                    }
                }
            }
        }
    }
}

/// Checks a pasted Aristotle key actually works, before it's saved - the
/// counterpart to `verify_claude_oauth_token`, and for the same reason: a
/// typo'd key that's accepted silently doesn't surface until a run has already
/// cloned, built, and reached the proving step.
///
/// `aristotle list` is the cheapest authenticated call available (it just
/// enumerates your projects) and starts nothing chargeable.
#[tauri::command]
pub async fn verify_aristotle_key(key: String) -> Result<(), String> {
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err("No key to verify.".into());
    }

    let (program, args) = aristotle_cli();
    let mut full: Vec<&str> = args.to_vec();
    full.extend(["list", "--limit", "1"]);

    let mut cmd = process::command(program, &full, None);
    cmd.env("ARISTOTLE_API_KEY", &key);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // First use also downloads the CLI via uvx, so allow for a slow start
    // while still failing rather than hanging forever.
    let output = tokio::time::timeout(std::time::Duration::from_secs(180), cmd.output())
        .await
        .map_err(|_| "Timed out checking the key (is the network up?).".to_string())?
        .map_err(|e| format!("Couldn't run the Aristotle CLI: {e}. Is uv installed?"))?;

    if output.status.success() {
        return Ok(());
    }
    let last_line = |bytes: &[u8]| -> Option<String> {
        String::from_utf8_lossy(bytes).lines().map(str::trim).filter(|l| !l.is_empty()).last().map(str::to_string)
    };
    let detail = last_line(&output.stderr)
        .or_else(|| last_line(&output.stdout))
        .unwrap_or_else(|| "no error detail returned".into());
    Err(format!("That key didn't work: {detail}"))
}

/// How to invoke the Aristotle CLI: via `uvx` (or `uv tool run`), matching
/// what `Scripts/aristotle-prove.sh` does, so there's nothing extra to install.
fn aristotle_cli() -> (&'static str, &'static [&'static str]) {
    if crate::paths::has_tool("uvx") {
        ("uvx", &["--from", "aristotlelib@latest", "aristotle"])
    } else {
        ("uv", &["tool", "run", "--from", "aristotlelib@latest", "aristotle"])
    }
}

#[tauri::command]
pub async fn start_aristotle_run(app: AppHandle, req: AristotleRunRequest) -> Result<super::run_task::RunTaskStarted, String> {
    let key = req.aristotle_api_key.clone().unwrap_or_default();
    if key.trim().is_empty() {
        return Err("No Aristotle API key configured. Add one in Settings to use the Aristotle prover.".into());
    }

    super::run_task::check_open_pr_cap(req.max_open_auto_prs).await?;

    let dir = PathBuf::from(&req.workspace_dir);
    workspace::ensure_cloned(app.clone(), &dir).await?;
    let branch = workspace::create_task_branch(&dir, &req.task_name.to_lowercase()).await?;

    spawn_aristotle_run(app, dir, branch.clone(), req.directions, key);
    Ok(super::run_task::RunTaskStarted { branch })
}

fn spawn_aristotle_run(app: AppHandle, dir: PathBuf, branch: String, directions: String, api_key: String) {
    tokio::spawn(async move {
        // Materialize the embedded wrapper next to nothing in particular - a
        // temp file, removed at the end - and make it executable.
        let script_path = std::env::temp_dir().join(format!("aristotle-prove-{}.sh", std::process::id()));
        if let Err(e) = std::fs::write(&script_path, ARISTOTLE_PROVE_SH) {
            finish_with_error(&app, branch, Some(format!("Couldn't stage the Aristotle helper: {e}")));
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755));
        }

        let targets = find_targets(&dir, &directions);
        if targets.is_empty() {
            let _ = std::fs::remove_file(&script_path);
            finish_with_error(
                &app,
                branch,
                Some("Nothing to prove: the directions named no .lean file, and no `sorry` was found in the checkout.".into()),
            );
            return;
        }
        emit_line(&app, format!("Handing {} file(s) to Aristotle:", targets.len()));
        for t in &targets {
            emit_line(&app, format!("  {}", t.strip_prefix(&dir).unwrap_or(t).display()));
        }
        emit_line(&app, "This can take a long time - hours, on a hard goal.");

        let prompt = format!(
            "Fill in every sorry in the Lean files in this project. Keep every theorem, lemma and definition \
             STATEMENT byte-for-byte identical - change only the proofs. Do not add new declarations or change \
             imports. Return the complete files.{}",
            if directions.trim().is_empty() {
                String::new()
            } else {
                format!(" Additional instructions: {}", directions.trim())
            }
        );

        // bash, not a direct exec: the wrapper is a shell script, and on
        // Windows this is what Git Bash (already a tracked prerequisite, see
        // ToolStatus.gitBash) provides.
        let dir_str = dir.to_string_lossy().into_owned();
        let script_str = script_path.to_string_lossy().into_owned();
        let mut args: Vec<String> = vec![script_str.clone()];
        args.extend(targets.iter().map(|t| t.to_string_lossy().into_owned()));
        args.extend(["--repo-root".into(), dir_str.clone(), "--apply".into(), "--prompt".into(), prompt]);
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        let ok = run_streaming(&app, "bash", &arg_refs, &dir, Some(&api_key)).await;
        let _ = std::fs::remove_file(&script_path);
        if !ok {
            finish_with_error(&app, branch, Some("Aristotle didn't return a usable proof. Nothing was committed.".into()));
            return;
        }

        // With no agent to notice a broken proof, the build is the only gate
        // between Aristotle's output and a pull request.
        emit_line(&app, "Verifying with `lake build`…");
        if !run_streaming(&app, "lake", &["build"], &dir, None).await {
            finish_with_error(
                &app,
                branch,
                Some("The build FAILED with Aristotle's proof in place, so no PR was prepared. The changes are still on the branch.".into()),
            );
            return;
        }
        emit_line(&app, "Build is green.");

        let diff = match workspace::staged_diff(&dir).await {
            Ok(d) if d.has_changes => d,
            Ok(_) => {
                finish_with_error(&app, branch, Some("Aristotle returned no actual change to commit.".into()));
                return;
            }
            Err(e) => {
                finish_with_error(&app, branch, Some(e));
                return;
            }
        };

        let (pr_title, pr_body) = compose_pr_text(&diff, &directions);
        let _ = app.emit(
            "task-run:finished",
            RunTaskFinished {
                could_finish: true,
                branch,
                diff: Some(diff),
                pr_title: Some(pr_title),
                pr_body: Some(pr_body),
                error: None,
            },
        );
    });
}

/// Strips ANSI escape sequences from a line and trims it, returning `None` if
/// nothing readable is left.
///
/// The Aristotle CLI draws a live progress bar by clearing and repainting the
/// screen (`ESC[2J`, `ESC[H`). Forwarded verbatim into a line-based activity
/// feed those become literal `[2J[H` noise in front of every status block,
/// which is exactly how the first real run rendered.
fn clean_line(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // ESC [ ... <final byte in @-~>, or a short two-character sequence.
        if chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Runs a child, forwarding both streams into the activity feed. True if it
/// exited zero.
///
/// Both streams go to the normal feed rather than stderr going to
/// `task-run:stderr`: this child logs all its *progress* to stderr, so routing
/// that to the danger-styled stream painted an ordinary healthy run entirely
/// in red warnings.
async fn run_streaming(app: &AppHandle, program: &str, args: &[&str], cwd: &Path, api_key: Option<&str>) -> bool {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut cmd = process::command(program, args, Some(cwd));
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(key) = api_key {
        cmd.env("ARISTOTLE_API_KEY", key);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            emit_line(app, format!("Couldn't start {program}: {e}"));
            return false;
        }
    };
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    // The progress bar repaints the same status block every few seconds;
    // without this the feed fills with hundreds of identical lines.
    let dedupe = |last: &mut Option<String>, line: String| -> Option<String> {
        if last.as_deref() == Some(line.as_str()) {
            return None;
        }
        *last = Some(line.clone());
        Some(line)
    };

    let app_out = app.clone();
    let out_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut last = None;
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(clean) = clean_line(&line).and_then(|l| dedupe(&mut last, l)) {
                emit_line(&app_out, clean);
            }
        }
    });
    let app_err = app.clone();
    let err_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut last = None;
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(clean) = clean_line(&line).and_then(|l| dedupe(&mut last, l)) {
                emit_line(&app_err, clean);
            }
        }
    });

    let status = child.wait().await;
    let _ = out_task.await;
    let _ = err_task.await;
    status.map(|s| s.success()).unwrap_or(false)
}

/// The PR title and body, built from the diff alone - no model is involved
/// anywhere on this path.
fn compose_pr_text(diff: &workspace::StagedDiff, directions: &str) -> (String, String) {
    let files: Vec<&str> = diff
        .stat
        .lines()
        .filter_map(|l| l.split('|').next())
        .map(str::trim)
        .filter(|s| s.ends_with(".lean"))
        .collect();
    let subject = files
        .first()
        .and_then(|f| f.rsplit('/').next())
        .map(|f| f.trim_end_matches(".lean"))
        .unwrap_or("lean");

    let title = format!("auto-aristotle({subject}): close proof with Aristotle");
    let mut body = String::from("## What changed\n\n");
    body.push_str(&format!(
        "Proof(s) in `{}` written by [Aristotle](https://aristotle.harmonic.fun), Harmonic's Lean 4 prover, \
         run directly from the Physlib AI Tools app with no agent in the proof loop.\n\n",
        if files.is_empty() { "the files below".to_string() } else { files.join("`, `") }
    ));
    if !directions.trim().is_empty() {
        body.push_str(&format!("Directions given for this run: {}\n\n", directions.trim()));
    }
    body.push_str("No statement was modified - only proof bodies. Verified with `lake build`.\n\n");
    body.push_str(&format!("## Diff stat\n\n```\n{}\n```\n", diff.stat.trim()));
    (title, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn directions_naming_a_real_file_win() {
        let tmp = std::env::temp_dir().join(format!("arist-test-a-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write(&tmp, "Physlib/Target.lean", "theorem t : True := by sorry");
        write(&tmp, "Physlib/Other.lean", "theorem u : True := by sorry");
        let got = find_targets(&tmp, "please golf foo_bar in Physlib/Target.lean, thanks");
        assert_eq!(got, vec![tmp.join("Physlib/Target.lean")]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Regression: a real run handed Aristotle three of Physlib's lint scripts
    /// because `contains("sorry")` matched `sorryAx` / `sorryful` /
    /// `sorryPseudoCheck`. None of them contain a provable goal.
    #[test]
    fn identifiers_containing_sorry_are_not_sorries() {
        assert!(!has_bare_sorry("let sorryPseudoCheck <- IO.Process.output"));
        assert!(!has_bare_sorry("declarations which depend on `sorryAx`"));
        assert!(!has_bare_sorry("marked with the `sorryful` attribute"));
        assert!(!has_bare_sorry("lake exe sorry_lint"));
        // ...but genuine ones still count, in every form they actually appear.
        assert!(has_bare_sorry("theorem t : True := by sorry"));
        assert!(has_bare_sorry("  sorry\n"));
        assert!(has_bare_sorry(":= sorry"));
        assert!(has_bare_sorry("by\n  intro h\n  sorry"));
    }

    #[test]
    fn directions_naming_a_directory_are_honoured() {
        let tmp = std::env::temp_dir().join(format!("arist-test-d-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write(&tmp, "Physlib/Electromagnetism/Distributional/Basic.lean", "theorem t : True := by sorry");
        write(&tmp, "Physlib/Elsewhere/Other.lean", "theorem u : True := by sorry");
        let got = find_targets(&tmp, "prove the sorries in Physlib/Electromagnetism/Distributional");
        assert_eq!(got, vec![tmp.join("Physlib/Electromagnetism/Distributional/Basic.lean")]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn lint_scripts_are_never_chosen() {
        let tmp = std::env::temp_dir().join(format!("arist-test-e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write(&tmp, "scripts/lint_all.lean", "let sorryPseudoCheck := 1\n-- sorry\n");
        write(&tmp, "Physlib/Real.lean", "theorem t : True := by sorry");
        let got = find_targets(&tmp, "");
        assert_eq!(got, vec![tmp.join("Physlib/Real.lean")]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ansi_progress_repaints_are_stripped() {
        assert_eq!(clean_line("\u{1b}[2J\u{1b}[H[█░░░] 1% (started 1m 35s ago)").as_deref(), Some("[█░░░] 1% (started 1m 35s ago)"));
        assert_eq!(clean_line("\u{1b}[2J\u{1b}[H"), None);
        assert_eq!(clean_line("   "), None);
        assert_eq!(clean_line("THINKING: exploring").as_deref(), Some("THINKING: exploring"));
    }

    #[test]
    fn falls_back_to_sorry_search_and_skips_lake() {
        let tmp = std::env::temp_dir().join(format!("arist-test-b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write(&tmp, "Physlib/HasSorry.lean", "theorem t : True := by sorry");
        write(&tmp, "Physlib/Clean.lean", "theorem u : True := by trivial");
        write(&tmp, ".lake/packages/mathlib/Huge.lean", "theorem v : True := by sorry");
        let got = find_targets(&tmp, "no file named here");
        assert_eq!(got, vec![tmp.join("Physlib/HasSorry.lean")], "must find the sorry and never descend into .lake");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn directions_naming_a_nonexistent_file_fall_back_rather_than_failing() {
        let tmp = std::env::temp_dir().join(format!("arist-test-c-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write(&tmp, "Physlib/HasSorry.lean", "theorem t : True := by sorry");
        let got = find_targets(&tmp, "prove Physlib/DoesNotExist.lean");
        assert_eq!(got, vec![tmp.join("Physlib/HasSorry.lean")]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn pr_text_names_the_changed_file() {
        let diff = workspace::StagedDiff {
            stat: " Physlib/Analysis/Inner.lean | 4 +--\n 1 file changed".into(),
            full: String::new(),
            has_changes: true,
        };
        let (title, body) = compose_pr_text(&diff, "focus on Mechanics");
        assert_eq!(title, "auto-aristotle(Inner): close proof with Aristotle");
        assert!(body.contains("Physlib/Analysis/Inner.lean"), "body: {body}");
        assert!(body.contains("focus on Mechanics"));
    }
}
