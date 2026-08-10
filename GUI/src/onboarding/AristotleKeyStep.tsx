import { useState } from "react";
import { Button } from "../components/Button";
import { Badge } from "../components/Badge";
import { openInBrowser, verifyAristotleKey } from "../lib/tauri";

const KEYS_URL = "https://aristotle.harmonic.fun";

/** Optional Aristotle (Harmonic's Lean prover) API key. Deliberately never a
 * gate on readiness: with no key the app behaves exactly as it did before -
 * the Aristotle prover option is disabled and Claude is never told the tool
 * exists. The key is held in the same plain-JSON config as the Claude OAuth
 * token, with the same trade-off (see config.rs). */
export function AristotleKeyStep({
  apiKey,
  onChange,
}: {
  apiKey: string | null;
  onChange: (key: string | null) => void;
}) {
  const [draft, setDraft] = useState("");
  const [editing, setEditing] = useState(false);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Checked against the API before it's stored, the same way the Claude token
  // is - a typo'd key saved silently wouldn't surface until a run had already
  // cloned, built, and reached the proving step.
  const save = async () => {
    const key = draft.trim();
    if (!key) return;
    setChecking(true);
    setError(null);
    try {
      await verifyAristotleKey(key);
      onChange(key);
      setDraft("");
      setEditing(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setChecking(false);
    }
  };

  if (apiKey && !editing) {
    return (
      <div className="setup-done-row">
        <Badge tone="success">Key saved</Badge>
        <Button variant="ghost" size="sm" onClick={() => { setDraft(""); setEditing(true); }}>
          Replace
        </Button>
        <Button variant="ghost" size="sm" onClick={() => onChange(null)}>
          Remove
        </Button>
      </div>
    );
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "0.6rem" }}>
      <input
        type="password"
        value={draft}
        onChange={(e) => setDraft(e.currentTarget.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") void save();
        }}
        placeholder="Paste your Aristotle API key"
        spellCheck={false}
        disabled={checking}
        style={{
          padding: "0.6rem 0.8rem",
          borderRadius: "var(--radius)",
          border: "1px solid var(--border)",
          background: "var(--background)",
          color: "var(--foreground)",
          fontFamily: "var(--font-mono)",
          fontSize: "0.8125rem",
        }}
      />
      <div style={{ display: "flex", gap: "0.6rem", flexWrap: "wrap" }}>
        <Button size="sm" disabled={!draft.trim() || checking} busy={checking} onClick={save}>
          {checking ? "Checking…" : "Save key"}
        </Button>
        <Button variant="ghost" size="sm" onClick={() => openInBrowser(KEYS_URL)}>
          Get a key ↗
        </Button>
        {editing && (
          <Button variant="ghost" size="sm" disabled={checking} onClick={() => { setDraft(""); setError(null); setEditing(false); }}>
            Cancel
          </Button>
        )}
      </div>
      {checking && (
        <span className="caption" style={{ color: "var(--muted)" }}>
          Checking the key with Aristotle. The first check also downloads its CLI, so it can take a minute.
        </span>
      )}
      {error && <span style={{ color: "var(--danger)", fontSize: "0.8125rem" }}>{error}</span>}
      <span className="caption" style={{ color: "var(--muted)" }}>
        Optional. Sign up at aristotle.harmonic.fun, then Dashboard → API Keys. Without one, everything works as it
        does today — Claude just does the proving itself.
      </span>
    </div>
  );
}
