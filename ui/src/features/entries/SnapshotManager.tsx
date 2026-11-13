import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { rpcCall } from "../../api/client";
import modal from "./EntriesView.module.scss";
import styles from "./SnapshotManager.module.scss";

type Action = { kind: "rollback" | "delete"; name: string };
interface Props {
  path: string;
  snapshots: Array<{ name: string }>;
  listError: string | null;
  onChanged: (action: "create" | "rollback" | "delete", name: string) => void;
}

export function SnapshotManager({ path, snapshots, listError, onChanged }: Props) {
  const [name, setName] = useState("");
  const [action, setAction] = useState<Action | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const dialog = useRef<HTMLFormElement>(null);
  const opener = useRef<HTMLElement | null>(null);
  const valid = /^[A-Za-z0-9._-]+$/.test(name);
  const sorted = [...snapshots].sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0);

  useEffect(() => {
    if (!action) return;
    const previous = opener.current;
    dialog.current?.querySelector<HTMLButtonElement>('[data-testid="snapshot-confirm-cancel"]')?.focus();
    return () => { previous?.focus(); };
  }, [action]);

  async function mutate(kind: "create" | "rollback" | "delete", snapshotName: string) {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await rpcCall(kind === "create" ? "create_snapshot" : kind === "rollback" ? "rollback_snapshot" : "delete_snapshot",
        { path, name: snapshotName });
      if (kind === "create") setName("");
      setAction(null);
      onChanged(kind, snapshotName);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return <div className={styles.panel} data-testid="snapshot-manager" onKeyDown={e => e.stopPropagation()}>
    <form className={styles.create} onSubmit={e => { e.preventDefault(); if (valid) void mutate("create", name); }}>
      <label htmlFor="new-snapshot-name">New snapshot:</label>
      <input id="new-snapshot-name" data-testid="snapshot-name" value={name}
        className={modal.modalInput} disabled={busy} autoComplete="off"
        aria-invalid={name !== "" && !valid}
        onChange={e => { setName(e.target.value); setError(null); }} />
      <button className={modal.modalBtn} disabled={busy || !valid} type="submit" data-testid="snapshot-create">Create</button>
    </form>
    {name && !valid ? <p className={modal.modalError}>Use letters A–Z, digits, dot, hyphen or underscore.</p> : null}
    {listError ? <p role="alert" className={modal.modalError}>{listError}</p> : null}
    {error && !action ? <p role="alert" className={modal.modalError}>{error}</p> : null}
    <table className={styles.table} data-testid="snapshot-table">
      <thead><tr><th>Name</th><th className={styles.actions}>Actions</th></tr></thead>
      <tbody>{sorted.map(snapshot => <tr key={snapshot.name}>
        <td>{snapshot.name}</td>
        <td className={styles.actions}>{(["rollback", "delete"] as const).map(kind =>
          <button key={kind} type="button" className={modal.modalBtn} disabled={busy}
            aria-label={`${kind === "rollback" ? "Rollback" : "Delete"} ${snapshot.name}`}
            onClick={e => { opener.current = e.currentTarget; setError(null); setAction({ kind, name: snapshot.name }); }}>
            {kind === "rollback" ? "Rollback" : "Delete"}
          </button>)}</td>
      </tr>)}</tbody>
    </table>
    {sorted.length === 0 && !listError ? <p className={styles.empty}>No snapshots.</p> : null}
    {action ? createPortal(<div className={modal.modalOverlay} data-testid="snapshot-confirm"
      onKeyDown={e => {
        e.stopPropagation();
        if (e.key === "Escape" && !busy) { e.preventDefault(); setAction(null); setError(null); }
        if (e.key === "Tab") {
          const buttons = dialog.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)");
          if (!buttons?.length) { e.preventDefault(); return; }
          const first = buttons[0], last = buttons[buttons.length - 1];
          if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
          else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
        }
      }}>
      <form ref={dialog} className={modal.modal} role="dialog" aria-modal="true" aria-labelledby="snapshot-confirm-title"
        onSubmit={e => { e.preventDefault(); void mutate(action.kind, action.name); }}>
        <h3 id="snapshot-confirm-title" className={modal.modalTitle}>{action.kind === "rollback" ? "Rollback snapshot" : "Delete snapshot"}</h3>
        <p className={modal.modalText}>{action.kind === "rollback"
          ? `Replace the current document with ${path}@${action.name}? All snapshots will remain.`
          : `Delete ${path}@${action.name}? The current document will not change.`}</p>
        {error ? <p role="alert" className={modal.modalError}>{error}</p> : null}
        <div className={modal.modalButtons}>
          <button type="button" className={modal.modalBtn} disabled={busy} data-testid="snapshot-confirm-cancel"
            onClick={() => { setAction(null); setError(null); }}>Cancel</button>
          <button type="submit" className={`${modal.modalBtn} ${modal.modalBtnDanger}`} disabled={busy}
            data-testid="snapshot-confirm-submit">{busy ? "Working…" : action.kind === "rollback" ? "Rollback" : "Delete"}</button>
        </div>
      </form>
    </div>, document.body) : null}
  </div>;
}
