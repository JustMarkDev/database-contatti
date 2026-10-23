import type { ComponentProps, CSSProperties } from "react";
import { useState } from "react";
import { mergeProps } from "@base-ui/react/merge-props";
import { useRender } from "@base-ui/react/use-render";
import { cva, type VariantProps } from "class-variance-authority";
import { MorphIcon } from "morphicons/react";
import { SidebarLeft01Icon, SidebarRight01Icon } from "@hugeicons/core-free-icons";
import { cn } from "../../lib/utils";
import { Button } from "./button";
import { Separator } from "./separator";
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "./sheet";
import { Skeleton } from "./skeleton";
import { Tooltip, TooltipContent, TooltipTrigger } from "./tooltip";
import { SIDEBAR_WIDTH_MOBILE, useSidebar } from "./sidebar-context";

export { SidebarProvider, useSidebar } from "./sidebar-context";

export function Sidebar({
  side = "left",
  collapsible = "icon",
  className,
  children,
  ...props
}: ComponentProps<"div"> & {
  side?: "left" | "right";
  collapsible?: "offcanvas" | "icon" | "none";
}) {
  const { isMobile, state, openMobile, setOpenMobile } = useSidebar();

  if (collapsible === "none") {
    return (
      <div
        data-slot="sidebar"
        className={cn(
          "flex h-full w-(--sidebar-width) flex-col bg-sidebar text-sidebar-foreground",
          className,
        )}
        {...props}
      >
        {children}
      </div>
    );
  }

  if (isMobile) {
    return (
      <Sheet open={openMobile} onOpenChange={setOpenMobile}>
        <SheetContent
          data-slot="sidebar"
          className="w-(--sidebar-width) bg-sidebar p-0 text-sidebar-foreground"
          style={{ "--sidebar-width": SIDEBAR_WIDTH_MOBILE } as CSSProperties}
          side={side}
        >
          <SheetHeader className="sr-only">
            <SheetTitle>Menu</SheetTitle>
            <SheetDescription>Menu di navigazione principale</SheetDescription>
          </SheetHeader>
          <div className="flex h-full w-full flex-col">{children}</div>
        </SheetContent>
      </Sheet>
    );
  }

  return (
    <div
      className="group peer hidden text-sidebar-foreground md:block"
      data-state={state}
      data-collapsible={state === "collapsed" ? collapsible : ""}
      data-side={side}
      data-slot="sidebar"
    >
      <div
        data-slot="sidebar-gap"
        className={cn(
          "relative w-[calc(var(--sidebar-width)+var(--sidebar-island-inset)+var(--sidebar-island-gap))] bg-transparent transition-[width] duration-200 ease-linear",
          "group-data-[collapsible=offcanvas]:w-0",
          "group-data-[collapsible=icon]:w-[calc(var(--sidebar-width-icon)+var(--sidebar-island-inset)+var(--sidebar-island-gap))]",
        )}
      />
      <div
        data-slot="sidebar-container"
        data-side={side}
        className={cn(
          "sidebar-island fixed top-(--sidebar-island-inset-top) bottom-(--sidebar-island-inset) left-(--sidebar-island-inset) z-10 hidden h-auto w-(--sidebar-width) transition-[left,right,width] duration-200 ease-linear group-data-[collapsible=icon]:w-(--sidebar-width-icon) group-data-[collapsible=offcanvas]:left-[calc((var(--sidebar-width)+var(--sidebar-island-inset))*-1)] md:flex",
          className,
        )}
        {...props}
      >
        <div
          data-slot="sidebar-inner"
          className="flex size-full flex-col overflow-hidden rounded-(--sidebar-island-radius) bg-sidebar"
        >
          {children}
        </div>
      </div>
    </div>
  );
}

export function SidebarTrigger({ className, onClick, ...props }: ComponentProps<typeof Button>) {
  const { toggleSidebar, state, isMobile, openMobile } = useSidebar();
  const expanded = isMobile ? openMobile : state === "expanded";
  return (
    <Button
      data-slot="sidebar-trigger"
      variant="ghost"
      size="icon"
      className={cn("sidebar-chrome-trigger size-6 rounded-md p-0", className)}
      onClick={(event) => {
        onClick?.(event);
        toggleSidebar();
      }}
      {...props}
    >
      <MorphIcon
        icon={expanded ? SidebarLeft01Icon : SidebarRight01Icon}
        size={14}
        strokeWidth={1.75}
        spring="snappy"
        reducedMotion="user"
      />
      <span className="sr-only">Comprimi/espandi menu</span>
    </Button>
  );
}

export function SidebarInset({ className, ...props }: ComponentProps<"main">) {
  return (
    <main
      data-slot="sidebar-inset"
      className={cn(
        "relative flex h-full min-h-0 w-full min-w-0 flex-1 flex-col overflow-hidden bg-background",
        className,
      )}
      {...props}
    />
  );
}

export function SidebarHeader({ className, ...props }: ComponentProps<"div">) {
  return (
    <div
      data-slot="sidebar-header"
      className={cn("flex flex-col gap-2 p-2 pb-0", className)}
      {...props}
    />
  );
}

export function SidebarFooter({ className, ...props }: ComponentProps<"div">) {
  return (
    <div
      data-slot="sidebar-footer"
      className={cn("flex flex-col gap-2 px-0 pt-2 pb-1", className)}
      {...props}
    />
  );
}

export function SidebarSeparator({ className, ...props }: ComponentProps<typeof Separator>) {
  return (
    <Separator
      data-slot="sidebar-separator"
      className={cn("mx-2 w-auto bg-sidebar-border", className)}
      {...props}
    />
  );
}

export function SidebarContent({ className, ...props }: ComponentProps<"div">) {
  return (
    <div
      data-slot="sidebar-content"
      className={cn(
        "flex min-h-0 flex-1 flex-col gap-4 overflow-auto pb-3 group-data-[collapsible=icon]:overflow-hidden",
        className,
      )}
      {...props}
    />
  );
}

export function SidebarGroup({ className, ...props }: ComponentProps<"div">) {
  return (
    <div
      data-slot="sidebar-group"
      className={cn("relative flex w-full min-w-0 flex-col p-0", className)}
      {...props}
    />
  );
}

export function SidebarGroupLabel({ className, ...props }: ComponentProps<"div">) {
  return (
    <div
      data-slot="sidebar-group-label"
      className={cn(
        "sidebar-group-label flex h-7 shrink-0 items-center px-3 text-xs font-medium text-faint group-data-[collapsible=icon]:hidden",
        className,
      )}
      {...props}
    />
  );
}

export function SidebarMenu({ className, ...props }: ComponentProps<"ul">) {
  return (
    <ul
      data-slot="sidebar-menu"
      className={cn("flex w-full min-w-0 flex-col gap-0.5 px-1", className)}
      {...props}
    />
  );
}

export function SidebarMenuItem({ className, ...props }: ComponentProps<"li">) {
  return (
    <li
      data-slot="sidebar-menu-item"
      className={cn("group/menu-item relative", className)}
      {...props}
    />
  );
}

const sidebarMenuButtonVariants = cva(
  "peer/menu-button flex w-full items-center gap-2.5 overflow-hidden rounded-lg px-2.5 text-left font-medium text-muted-foreground outline-none transition-colors group-data-[collapsible=icon]:size-9! group-data-[collapsible=icon]:justify-center group-data-[collapsible=icon]:p-0! hover:bg-white/[0.04] hover:text-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring disabled:pointer-events-none disabled:opacity-50 data-active:bg-white/[0.07] data-active:text-foreground data-active:shadow-[inset_0_0_0_1px_rgb(255_255_255/0.035)] data-active:[&_svg]:text-brand [&_svg]:size-[18px] [&_svg]:shrink-0 [&>span:last-child]:truncate",
  {
    variants: {
      size: {
        default: "h-9 text-[13.5px]",
        sm: "h-8 text-sm",
      },
    },
    defaultVariants: { size: "default" },
  },
);

export function SidebarMenuButton({
  render,
  isActive = false,
  size = "default",
  tooltip,
  className,
  ...props
}: useRender.ComponentProps<"button"> &
  ComponentProps<"button"> & {
    isActive?: boolean;
    tooltip?: string | ComponentProps<typeof TooltipContent>;
  } & VariantProps<typeof sidebarMenuButtonVariants>) {
  const { isMobile, state, setOpenMobile } = useSidebar();
  const { onClick, ...buttonProps } = props;
  const handleClick: NonNullable<ComponentProps<"button">["onClick"]> = (event) => {
    onClick?.(event);
    if (isMobile) setOpenMobile(false);
  };
  const comp = useRender({
    defaultTagName: "button",
    props: mergeProps<"button">(
      {
        className: cn(sidebarMenuButtonVariants({ size }), className),
        onClick: handleClick,
      },
      buttonProps,
    ),
    render: !tooltip ? render : <TooltipTrigger render={render} />,
    state: { slot: "sidebar-menu-button", active: isActive },
  });

  if (!tooltip) return comp;

  const tooltipProps = typeof tooltip === "string" ? { children: tooltip } : tooltip;
  return (
    <Tooltip>
      {comp}
      <TooltipContent
        side="right"
        align="center"
        hidden={state !== "collapsed" || isMobile}
        {...tooltipProps}
      />
    </Tooltip>
  );
}

export function SidebarMenuBadge({ className, ...props }: ComponentProps<"div">) {
  return (
    <div
      data-slot="sidebar-menu-badge"
      className={cn(
        "pointer-events-none absolute right-2 top-1/2 flex h-[18px] min-w-[18px] -translate-y-1/2 items-center justify-center rounded-md bg-brand/15 px-1.5 text-[11px] font-semibold text-brand tabular-nums select-none group-data-[collapsible=icon]:hidden",
        className,
      )}
      {...props}
    />
  );
}

export function SidebarMenuSkeleton({
  className,
  showIcon = false,
  ...props
}: ComponentProps<"div"> & { showIcon?: boolean }) {
  const [width] = useState(() => `${Math.floor(Math.random() * 40) + 50}%`);
  return (
    <div
      data-slot="sidebar-menu-skeleton"
      className={cn("flex h-9 items-center gap-2 rounded-md px-2", className)}
      {...props}
    >
      {showIcon && <Skeleton className="size-4 rounded-md" />}
      <Skeleton
        className="h-4 max-w-(--skeleton-width) flex-1"
        style={{ "--skeleton-width": width } as CSSProperties}
      />
    </div>
  );
}
