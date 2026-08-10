import { useId, useState } from "react";
import { Button } from "../components/Button";
import type { Engine } from "../lib/types";

const fieldStyle = {
  padding: "0.6rem 0.8rem",
  borderRadius: "var(--radius)",
  border: "1px solid var(--border)",
  background: "var(--background)",
  color: "var(--foreground)",
  fontFamily: "var(--font-sans)",
  fontSize: "0.875rem",
  resize: "vertical",
} as const;

export interface PreRunValues {
  answers: { question: string; answer: string }[];
  directions: string;
  engine: Engine;
}

/** The one screen shown before a run starts: optional free-text directions,
 * the choice of prover, and the task's own `input_questions` if it has any.
 *
 * Everything here is optional - submitting untouched reproduces the old
 * behaviour exactly (Claude, no directions, agent picks its own target). It's
 * shown for every task, not just ones with input questions, because a
 * directions box nobody can find is a directions box nobody uses. */
export function PreRunForm({
  questions,
  aristotleAvailable,
  onSubmit,
  onCancel,
}: {
  questions: string[];
  /** False when no Aristotle API key is configured - the direct-prover option
   * is offered but disabled, with the reason shown, rather than hidden. */
  aristotleAvailable: boolean;
  onSubmit: (values: PreRunValues) => void;
  onCancel: () => void;
}) {
  const [answers, setAnswers] = useState<string[]>(() => questions.map(() => ""));
  const [directions, setDirections] = useState("");
  const [engine, setEngine] = useState<Engine>("claude");
  // Radio groups are scoped by `name` across the whole document, so a literal
  // "engine" would make two mounted forms fight over one selection.
  const engineGroup = useId();

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "1rem" }}>
      {questions.length > 0 && (
        <>
          <p style={{ color: "var(--muted)" }}>A couple of quick questions before we start:</p>
          {questions.map((q, i) => (
            <label key={i} style={{ display: "flex", flexDirection: "column", gap: "0.35rem", fontSize: "0.875rem" }}>
              <span>{q}</span>
              <textarea
                value={answers[i]}
                onChange={(e) => {
                  const next = [...answers];
                  next[i] = e.currentTarget.value;
                  setAnswers(next);
                }}
                rows={2}
                style={fieldStyle}
              />
            </label>
          ))}
        </>
      )}

      <label style={{ display: "flex", flexDirection: "column", gap: "0.35rem", fontSize: "0.875rem" }}>
        <span>
          Directions <span style={{ color: "var(--muted)" }}>- optional</span>
        </span>
        <textarea
          value={directions}
          onChange={(e) => setDirections(e.currentTarget.value)}
          rows={3}
          placeholder="e.g. golf the proof of inner_mul_le_norm in Physlib/Analysis/Inner.lean"
          style={fieldStyle}
        />
        <span style={{ color: "var(--muted)", fontSize: "0.75rem" }}>
          Point it at a specific file, theorem, or approach. Leave blank to let it choose its own target.
        </span>
      </label>

      <fieldset style={{ border: "none", padding: 0, margin: 0, display: "flex", flexDirection: "column", gap: "0.35rem" }}>
        <legend style={{ fontSize: "0.875rem", padding: 0, marginBottom: "0.35rem" }}>Prover</legend>
        <label style={{ display: "flex", gap: "0.5rem", alignItems: "flex-start", fontSize: "0.875rem" }}>
          <input type="radio" name={engineGroup} checked={engine === "claude"} onChange={() => setEngine("claude")} />
          <span>
            Claude <span style={{ color: "var(--muted)" }}>- reads the repo, edits, builds, writes the PR</span>
          </span>
        </label>
        <label
          style={{
            display: "flex",
            gap: "0.5rem",
            alignItems: "flex-start",
            fontSize: "0.875rem",
            opacity: aristotleAvailable ? 1 : 0.55,
          }}
        >
          <input
            type="radio"
            name={engineGroup}
            checked={engine === "aristotle"}
            disabled={!aristotleAvailable}
            onChange={() => setEngine("aristotle")}
          />
          <span>
            Aristotle{" "}
            <span style={{ color: "var(--muted)" }}>
              {aristotleAvailable
                ? "- Harmonic's Lean prover closes the sorries directly, no agent in the loop"
                : "- add an Aristotle API key in setup to enable this"}
            </span>
          </span>
        </label>
      </fieldset>

      <div style={{ display: "flex", gap: "0.6rem" }}>
        <Button
          onClick={() =>
            onSubmit({
              answers: questions.map((q, i) => ({ question: q, answer: answers[i] })),
              directions,
              engine,
            })
          }
        >
          Start task
        </Button>
        <Button variant="secondary" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </div>
  );
}
