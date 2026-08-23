import { useId, useRef, useState } from "react";

export function InlineEditableField({
  value,
  label,
  validate,
  onSave,
  compact = false,
  displayValue,
  inputType = "text",
  normalize = (next) => next.trim(),
  cancelEmpty = false,
  maxLength,
  disabled = false,
}: {
  value: string;
  label: string;
  validate: (value: string) => string | null;
  onSave: (value: string) => Promise<void>;
  compact?: boolean;
  displayValue?: string;
  inputType?: "text" | "password";
  normalize?: (value: string) => string;
  cancelEmpty?: boolean;
  maxLength?: number;
  disabled?: boolean;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const [error, setError] = useState("");
  const errorId = useId();
  const committingRef = useRef(false);
  const skipBlurRef = useRef(false);

  const cancel = () => {
    skipBlurRef.current = true;
    setDraft(value);
    setError("");
    setEditing(false);
  };

  const commit = async () => {
    if (committingRef.current) return;
    const next = normalize(draft);
    if (cancelEmpty && next.length === 0) {
      setDraft(value);
      setError("");
      setEditing(false);
      return;
    }
    const validationError = validate(next);
    if (validationError) {
      setError(validationError);
      return;
    }
    if (next === value) {
      setEditing(false);
      return;
    }
    committingRef.current = true;
    try {
      await onSave(next);
      setDraft(inputType === "password" ? value : next);
      setError("");
      setEditing(false);
    } catch (saveError) {
      setError(saveError instanceof Error ? saveError.message : "保存失败");
    } finally {
      committingRef.current = false;
    }
  };

  if (editing) {
    return <>
      <input
        className={`sunshine-inline-input${compact ? " compact" : ""}${error ? " input-error" : ""}`}
        value={draft}
        type={inputType}
        aria-label={label}
        aria-invalid={Boolean(error)}
        aria-errormessage={error ? errorId : undefined}
        title={error || undefined}
        maxLength={maxLength}
        autoFocus
        onClick={(event) => event.stopPropagation()}
        onChange={(event) => { setDraft(event.target.value); setError(""); }}
        onBlur={() => {
          if (skipBlurRef.current) {
            skipBlurRef.current = false;
            return;
          }
          void commit();
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter") { event.preventDefault(); void commit(); }
          if (event.key === "Escape") { event.preventDefault(); cancel(); }
        }}
      />
      {error ? <span className="sr-only" id={errorId} role="alert">{error}</span> : null}
    </>;
  }

  return (
    <button
      type="button"
      className={`sunshine-inline-editable${compact ? " compact" : ""}`}
      title={disabled ? "正在保存，请稍候" : `修改${label}`}
      aria-label={`修改${label}，当前值：${displayValue ?? value}`}
      disabled={disabled}
      onClick={(event) => {
        event.stopPropagation();
        if (disabled) return;
        skipBlurRef.current = false;
        setDraft(value);
        setEditing(true);
      }}
    >
      {displayValue ?? value}
    </button>
  );
}
