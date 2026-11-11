import { useCallback, useEffect } from "react";
import { authenticatedFetch } from "../api";
import { useSnackbar } from "../contexts";
import type { WorkflowStatusResponse } from "../types";

interface WorkflowStatusPolling {
  /** The status endpoint of the workflow. */
  path: string;
  /** The snackbar text when the workflow was cancelled. */
  cancelledMessage: string;
  /** The dialog's status state. Polling runs while it is in progress. */
  status: WorkflowStatusResponse | null;
  setStatus: (status: WorkflowStatusResponse) => void;
  setLoading: (loading: boolean) => void;
  onComplete: () => void;
  onClose: () => void;
}

/**
 * Poll a workflow's status every two seconds while the dialog's status is in
 * progress. A cancelled workflow shows `cancelledMessage` and closes the
 * dialog. Any other answer replaces the status. An answer that is not in
 * progress clears the loading flag, and a completed one calls `onComplete`.
 * Failed requests are ignored; the next tick tries again.
 */
export function useWorkflowStatus({
  path,
  cancelledMessage,
  status,
  setStatus,
  setLoading,
  onComplete,
  onClose,
}: WorkflowStatusPolling): void {
  const { showSnackbar } = useSnackbar();

  const pollStatus = useCallback(async () => {
    try {
      const res = await authenticatedFetch(path);
      if (res.ok) {
        const data: WorkflowStatusResponse = await res.json();
        if (data.status === "cancelled") {
          showSnackbar(cancelledMessage);
          onClose();
          return;
        }
        setStatus(data);
        if (data.status !== "inprogress") {
          setLoading(false);
          if (data.status === "completed") {
            onComplete();
          }
        }
      }
    } catch {
      // Ignore polling errors
    }
  }, [
    path,
    cancelledMessage,
    setStatus,
    setLoading,
    onComplete,
    onClose,
    showSnackbar,
  ]);

  useEffect(() => {
    let interval: number | undefined;

    if (status?.status === "inprogress") {
      // Poll immediately, then every 2 seconds
      pollStatus();
      interval = window.setInterval(pollStatus, 2000);
    }

    return () => {
      if (interval) clearInterval(interval);
    };
  }, [status?.status, pollStatus]);
}
