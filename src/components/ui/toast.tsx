import type { ReactNode } from "react";
import { Toast as ToastPrimitive } from "@base-ui/react/toast";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  Alert02Icon,
  AlertDiamondIcon,
  Cancel01Icon,
  CheckmarkCircle02Icon,
  InformationCircleIcon,
  Loading03Icon,
} from "@hugeicons/core-free-icons";
import { Button } from "./button";
import { cn } from "../../lib/utils";

const manager = ToastPrimitive.createToastManager();

function ToastPortal({ ...props }: ToastPrimitive.Portal.Props) {
  return <ToastPrimitive.Portal data-slot="toast-portal" {...props} />;
}

function ToastViewport({ className, ...props }: ToastPrimitive.Viewport.Props) {
  return (
    <ToastPrimitive.Viewport
      data-slot="toast-viewport"
      className={cn(
        "pointer-events-none fixed inset-x-4 bottom-4 z-50 mx-auto w-auto max-w-sm outline-none sm:right-4 sm:left-auto sm:mx-0 sm:w-full",
        className,
      )}
      {...props}
    />
  );
}

function Toast({ className, ...props }: ToastPrimitive.Root.Props) {
  return (
    <ToastPrimitive.Root
      data-slot="toast"
      className={cn(
        "group/toast pointer-events-auto absolute right-0 bottom-0 z-[calc(1000-var(--toast-index))] w-full origin-bottom rounded-xl border border-border bg-popover text-[13px] text-popover-foreground shadow-[0_16px_40px_-12px_rgb(0_0_0/0.7)] outline-none focus-visible:ring-2 focus-visible:ring-ring/60",
        "[--gap:0.75rem] [--height:var(--toast-frontmost-height,var(--toast-height))] [--offset-y:calc(var(--toast-offset-y)*-1+calc(var(--toast-index)*var(--gap)*-1)+var(--toast-swipe-movement-y))] [--peek:0.75rem] [--scale:calc(max(0,1-(var(--toast-index)*0.1)))] [--shrink:calc(1-var(--scale))]",
        "h-(--height) [transform:translateX(var(--toast-swipe-movement-x))_translateY(calc(var(--toast-swipe-movement-y)-(var(--toast-index)*var(--peek))-(var(--shrink)*var(--height))))_scale(var(--scale))] [transition:transform_500ms_cubic-bezier(0.22,1,0.36,1),opacity_500ms,height_150ms]",
        "after:absolute after:top-full after:left-0 after:h-[calc(var(--gap)+1px)] after:w-full after:content-['']",
        "data-expanded:h-(--toast-height) data-expanded:[transform:translateX(var(--toast-swipe-movement-x))_translateY(var(--offset-y))]",
        "data-limited:opacity-0 data-starting-style:[transform:translateY(150%)]",
        "[&[data-ending-style]:not([data-limited]):not([data-swipe-direction])]:[transform:translateY(150%)]",
        className,
      )}
      {...props}
    />
  );
}

function ToastContent({ className, ...props }: ToastPrimitive.Content.Props) {
  return (
    <ToastPrimitive.Content
      data-slot="toast-content"
      className={cn(
        "flex h-full items-center gap-3 overflow-hidden p-4 transition-opacity duration-250 ease-[cubic-bezier(0.22,1,0.36,1)] data-behind:opacity-0 data-expanded:opacity-100",
        className,
      )}
      {...props}
    />
  );
}

function ToastTitle({ className, ...props }: ToastPrimitive.Title.Props) {
  return (
    <ToastPrimitive.Title
      data-slot="toast-title"
      className={cn("text-sm font-medium", className)}
      {...props}
    />
  );
}

function ToastDescription({ className, ...props }: ToastPrimitive.Description.Props) {
  return (
    <ToastPrimitive.Description
      data-slot="toast-description"
      className={cn("text-sm text-muted-foreground", className)}
      {...props}
    />
  );
}

function ToastAction({
  className,
  render = <Button variant="outline" size="sm" />,
  ...props
}: ToastPrimitive.Action.Props) {
  return (
    <ToastPrimitive.Action
      data-slot="toast-action"
      render={render}
      className={cn("shrink-0", className)}
      {...props}
    />
  );
}

function ToastClose({
  className,
  children,
  render = <Button variant="ghost" size="icon" />,
  ...props
}: ToastPrimitive.Close.Props) {
  return (
    <ToastPrimitive.Close
      data-slot="toast-close"
      aria-label="Chiudi notifica"
      render={render}
      className={cn(
        "relative shrink-0 text-muted-foreground after:absolute after:-inset-2 after:content-[''] hover:text-foreground",
        className,
      )}
      {...props}
    >
      {children ?? (
        <HugeiconsIcon
          icon={Cancel01Icon}
          size={16}
          strokeWidth={1.5}
          color="currentColor"
          aria-hidden="true"
          className="size-4"
        />
      )}
    </ToastPrimitive.Close>
  );
}

function ToastIcon({ type }: { type: string | undefined }) {
  let icon: ReactNode = null;
  if (type === "success")
    icon = (
      <HugeiconsIcon
        icon={CheckmarkCircle02Icon}
        size={16}
        strokeWidth={1.5}
        color="currentColor"
        aria-hidden="true"
      />
    );
  if (type === "info")
    icon = (
      <HugeiconsIcon
        icon={InformationCircleIcon}
        size={16}
        strokeWidth={1.5}
        color="currentColor"
        aria-hidden="true"
      />
    );
  if (type === "warning")
    icon = (
      <HugeiconsIcon
        icon={Alert02Icon}
        size={16}
        strokeWidth={1.5}
        color="currentColor"
        aria-hidden="true"
      />
    );
  if (type === "error")
    icon = (
      <HugeiconsIcon
        icon={AlertDiamondIcon}
        size={16}
        strokeWidth={1.5}
        color="currentColor"
        aria-hidden="true"
        className="text-destructive"
      />
    );
  if (type === "loading")
    icon = (
      <HugeiconsIcon
        icon={Loading03Icon}
        size={16}
        strokeWidth={1.5}
        color="currentColor"
        aria-hidden="true"
        className="animate-spin"
      />
    );
  if (!icon) return null;
  return (
    <span data-slot="toast-icon" className="shrink-0 [&_svg]:pointer-events-none [&_svg]:size-4">
      {icon}
    </span>
  );
}

function ToastList() {
  const { toasts } = ToastPrimitive.useToastManager();
  return toasts.map((item) => (
    <Toast key={item.id} toast={item}>
      <ToastContent>
        <ToastIcon type={item.type} />
        <div className="flex min-w-0 flex-1 flex-col gap-1">
          <ToastTitle />
          <ToastDescription />
        </div>
        <ToastAction />
        <ToastClose />
      </ToastContent>
    </Toast>
  ));
}

export function Toaster({
  children,
  toastManager = manager,
  ...props
}: ToastPrimitive.Provider.Props) {
  return (
    <ToastPrimitive.Provider toastManager={toastManager} {...props}>
      {children}
      <ToastPortal>
        <ToastViewport>
          <ToastList />
        </ToastViewport>
      </ToastPortal>
    </ToastPrimitive.Provider>
  );
}

type ToastShorthandOptions = { description?: string; id?: string };

function shorthand(type: "success" | "error" | "warning" | "info") {
  return (title: string, options?: ToastShorthandOptions) => {
    if (options?.id) {
      manager.update(options.id, {
        type,
        title,
        description: options.description,
        timeout: 5000,
      });
      return options.id;
    }
    return manager.add({ type, title, description: options?.description });
  };
}

export const toast = Object.assign(manager.add, {
  add: manager.add,
  close: manager.close,
  update: manager.update,
  promise: manager.promise,
  success: shorthand("success"),
  error: shorthand("error"),
  warning: shorthand("warning"),
  info: shorthand("info"),
  loading: (title: string, options?: ToastShorthandOptions) =>
    manager.add({ type: "loading", title, description: options?.description, timeout: 0 }),
});
