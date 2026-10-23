import { useEffect, useState } from "react";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "./alert-dialog";

type ConfirmOptions = {
  title: string;
  description?: string;
  actionLabel?: string;
  cancelLabel?: string;
  variant?: "default" | "destructive";
};

type CheckboxConfirmOptions = ConfirmOptions & {
  checkboxLabel: string;
  initialChecked?: boolean;
};

type ConfirmResult = { confirmed: boolean; checked: boolean };
type ConfirmState =
  | (ConfirmOptions & {
      checkboxLabel?: string;
      initialChecked?: boolean;
      resolve: (value: ConfirmResult) => void;
    })
  | null;

let notify: ((state: ConfirmState) => void) | null = null;

export function confirmDialog(options: ConfirmOptions): Promise<boolean> {
  return new Promise((resolve) => {
    notify?.({ ...options, resolve: (result) => resolve(result.confirmed) });
  });
}

export function confirmDialogWithCheckbox(options: CheckboxConfirmOptions): Promise<ConfirmResult> {
  return new Promise((resolve) => {
    notify?.({ ...options, resolve });
  });
}

export function ConfirmDialogHost() {
  const [state, setState] = useState<ConfirmState>(null);
  const [checked, setChecked] = useState(false);
  useEffect(() => {
    notify = (next) => {
      setChecked(next?.initialChecked ?? false);
      setState(next);
    };
    return () => {
      notify = null;
    };
  }, []);

  function settle(result: boolean) {
    state?.resolve({ confirmed: result, checked });
    setState(null);
  }

  return (
    <AlertDialog
      open={state !== null}
      onOpenChange={(open) => {
        if (!open) settle(false);
      }}
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{state?.title}</AlertDialogTitle>
          {state?.description && (
            <AlertDialogDescription>{state.description}</AlertDialogDescription>
          )}
        </AlertDialogHeader>
        {state?.checkboxLabel && (
          <label className="actions-row">
            <input
              type="checkbox"
              checked={checked}
              onChange={(event) => setChecked(event.currentTarget.checked)}
            />
            <span>{state.checkboxLabel}</span>
          </label>
        )}
        <AlertDialogFooter>
          <AlertDialogCancel onClick={() => settle(false)}>
            {state?.cancelLabel ?? "Annulla"}
          </AlertDialogCancel>
          <AlertDialogAction
            variant={state?.variant === "destructive" ? "destructive" : "default"}
            onClick={() => settle(true)}
          >
            {state?.actionLabel ?? "Conferma"}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
