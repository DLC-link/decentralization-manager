import { useState, type ReactNode } from "react";

import type { HostStatus } from "../types";

export function Card({
  title,
  eyebrow,
  aside,
  children,
}: {
  title: string;
  eyebrow?: string;
  aside?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="card">
      <header className="card-header">
        <div>
          {eyebrow ? <div className="t-eyebrow">{eyebrow}</div> : null}
          <h2 className="t-h4">{title}</h2>
        </div>
        {aside}
      </header>
      {children}
    </section>
  );
}

/** A labelled machine-readable value: mono, wrapping, copyable. */
export function Fact({ label, value }: { label: string; value: string }) {
  return (
    <div className="fact">
      <span className="t-eyebrow">{label}</span>
      <div className="fact-value">
        <code>{value}</code>
        <Copy value={value} />
      </div>
    </div>
  );
}

export function Copy({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);

  const onCopy = () => {
    // Clipboard access needs a secure context; the demo runs on plain http
    // loopback, where browsers still allow it. If it is refused, say so rather
    // than showing a false confirmation.
    navigator.clipboard
      .writeText(value)
      .then(() => setCopied(true))
      .catch(() => setCopied(false));
    window.setTimeout(() => setCopied(false), 1500);
  };

  return (
    <button type="button" className="copy" onClick={onCopy} title="Copy">
      {copied ? "✓" : "Copy"}
    </button>
  );
}

const STATUS_LABELS: Record<HostStatus, { label: string; className: string }> = {
  hosted: { label: "Hosted", className: "badge-hosted" },
  pending: { label: "Authorizing", className: "badge-pending" },
  not_hosted: { label: "Absent", className: "badge-absent" },
};

export function StatusBadge({
  status,
  error,
}: {
  status: HostStatus | null;
  error: string | null;
}) {
  if (error) {
    return (
      <span className="badge badge-error" title={error}>
        <span className="dot" />
        Unreachable
      </span>
    );
  }
  if (!status) {
    return (
      <span className="badge badge-absent">
        <span className="dot" />
        Unknown
      </span>
    );
  }
  const { label, className } = STATUS_LABELS[status];
  return (
    <span className={`badge ${className}`}>
      <span className={`dot ${status === "pending" ? "dot-live" : ""}`} />
      {label}
    </span>
  );
}

export function Notice({
  kind,
  children,
}: {
  kind: "error" | "info";
  children: ReactNode;
}) {
  return (
    <div className={`notice notice-${kind}`} role={kind === "error" ? "alert" : "status"}>
      <span className="notice-glyph">{kind === "error" ? "⚠" : "◇"}</span>
      <div>{children}</div>
    </div>
  );
}
