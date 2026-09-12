import React, { useEffect, useId, useRef } from 'react';

// Every status message in this app appears after a fetch resolves, so assistive
// tech only learns about it through a live region: `role="alert"` for failures,
// `role="status"` (polite) for success.
export function Banner({ kind = 'error', children }) {
  if (!children) return null;
  if (kind === 'ok') {
    return (
      <div className="ok" role="status" aria-live="polite">
        {children}
      </div>
    );
  }
  return (
    <div className="err" role="alert">
      {children}
    </div>
  );
}

/** A decorative spinner; its label is always rendered as text beside it. */
export function Spinner() {
  return <span className="spinner" aria-hidden="true" />;
}

/**
 * A button that reports in-flight work: disabled so the request cannot be sent
 * twice, `aria-busy` so the state is announced, and a changing label ending in
 * an ellipsis so the wait is visible.
 */
export function BusyButton({ busy, busyText, className = 'btn', children, ...rest }) {
  return (
    <button type="button" className={className} disabled={busy} aria-busy={busy || undefined} {...rest}>
      {busy ? (
        <>
          <Spinner /> {busyText}
        </>
      ) : (
        children
      )}
    </button>
  );
}

/**
 * Modal confirmation for an action that loses work or cuts a device off. Escape
 * and a backdrop click cancel; focus starts on the safe action and is kept
 * inside the dialog while it is open.
 */
export function ConfirmDialog({ title, body, confirmLabel, cancelLabel = '取消', danger, onConfirm, onCancel }) {
  const titleId = useId();
  const bodyId = useId();
  const cardRef = useRef(null);
  const cancelRef = useRef(null);
  const restoreRef = useRef(null);

  useEffect(() => {
    restoreRef.current = document.activeElement;
    cancelRef.current?.focus();
    return () => restoreRef.current?.focus?.();
  }, []);

  useEffect(() => {
    const onKey = (e) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onCancel();
        return;
      }
      if (e.key !== 'Tab' || !cardRef.current) return;
      const nodes = [...cardRef.current.querySelectorAll('button, [href], input, select, textarea')].filter((n) => !n.disabled);
      if (nodes.length === 0) return;
      const first = nodes[0];
      const last = nodes[nodes.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onCancel]);

  return (
    <div
      className="modal-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onCancel();
      }}
    >
      <div className="modal" role="alertdialog" aria-modal="true" aria-labelledby={titleId} aria-describedby={body ? bodyId : undefined} ref={cardRef}>
        <h2 className="modal-title" id={titleId}>{title}</h2>
        {body && <p className="muted" id={bodyId}>{body}</p>}
        <div className="row modal-actions">
          <button type="button" className="btn secondary" ref={cancelRef} onClick={onCancel}>
            {cancelLabel}
          </button>
          <button type="button" className={danger ? 'btn danger' : 'btn'} onClick={onConfirm}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
