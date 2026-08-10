import { parse as parseYaml } from "yaml";
import type { ParsedTask, TaskFile } from "../lib/types";

function firstLine(text: string): string {
  const line = text.split("\n").find((l) => l.trim().length > 0);
  return line?.trim() ?? "";
}

/** Parses a raw task file exactly like `Scripts/physlib-auto-task.sh` does:
 * YAML tasks carry `prompt:`/`description:`/`input_questions:`/
 * `challenge_questions:`; Markdown tasks are the prompt verbatim, with the
 * description taken from the first meaningful line. */
export function parseTask(file: TaskFile): ParsedTask {
  if (file.format === "yaml") {
    let doc: Record<string, unknown> = {};
    try {
      doc = (parseYaml(file.content) as Record<string, unknown>) ?? {};
    } catch {
      // Malformed YAML - surface as an empty/unusable task rather than throwing,
      // so one bad file doesn't take down the whole task list.
    }
    const prompt = typeof doc.prompt === "string" ? doc.prompt.trim() : "";
    const description =
      typeof doc.description === "string" && doc.description.trim() ? doc.description.trim() : firstLine(prompt);
    const inputQuestions = Array.isArray(doc.input_questions) ? doc.input_questions.map(String) : [];
    const challengeQuestions = Array.isArray(doc.challenge_questions) ? doc.challenge_questions.map(String) : [];
    return { name: file.name, description, prompt, inputQuestions, challengeQuestions, source: file.source };
  }

  const prompt = file.content.trim();
  let description = firstLine(prompt)
    .replace(/^#\s*Task:\s*/i, "")
    .replace(/^#\s*/, "")
    .replace(/^Task:\s*/i, "");
  if (description.length > 120) description = description.slice(0, 119) + "…";
  return { name: file.name, description, prompt, inputQuestions: [], challengeQuestions: [], source: file.source };
}

/** Assembles the prompt Claude actually receives, in the same order and with
 * the same wording `Scripts/physlib-auto-task.sh` uses, so both harnesses
 * produce identical prompts:
 *
 *   1. the task's `input_questions` answers, as a Markdown bullet list
 *      (the script's `ask_questions`);
 *   2. the run's free-text directions, if any (the script's `--direct`);
 *   3. the task prompt itself.
 *
 * Directions go last of the two preambles - immediately before the prompt -
 * because they're the most specific thing the model has been told, and they
 * deliberately outrank the task's own "pick a target" instruction. With no
 * answers and no directions the prompt is returned untouched, which is what
 * keeps an undirected run exactly as autonomous as it has always been. */
export function buildPrompt(
  basePrompt: string,
  answers: { question: string; answer: string }[],
  directions = "",
): string {
  let prompt = basePrompt;
  if (directions.trim()) {
    prompt = `Before the task below, here are my specific directions for this run. Follow them,\nand treat them as overriding any instruction in the task to choose a target freely:\n\n${directions.trim()}\n\n${prompt}`;
  }
  if (answers.length > 0) {
    const bullets = answers
      .map(({ question, answer }) => `- **${question}**\n  ${answer.trim() || "(no answer)"}`)
      .join("\n");
    prompt = `Before the task below, here is the input I provided - take it into account as you carry out the task:\n\n${bullets}\n\n${prompt}`;
  }
  return prompt;
}

/** Appends challenge-question verdicts to a PR body under a "## Human
 * review" section, matching the script's PR-body convention. */
export function appendChallengeVerdicts(body: string, verdicts: { question: string; verdict: "Yes" | "No" }[]): string {
  if (verdicts.length === 0) return body;
  const lines = verdicts.map(({ question, verdict }) => `- **${question}** ${verdict}`).join("\n");
  return `${body}\n\n---\n\n## Human review\n\n${lines}\n`;
}
