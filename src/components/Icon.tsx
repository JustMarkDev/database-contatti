import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react";
import {
  AiBrain01Icon,
  AiSparklesIcon,
  ArrowLeft01Icon,
  ArrowRight01Icon,
  BanIcon,
  Delete02Icon,
  Download01Icon,
  FolderOpenIcon,
  File01Icon,
  PlusSignIcon,
  ViewIcon,
  PauseIcon,
  PlayIcon,
  Search01Icon,
  Settings01Icon,
  StopIcon,
  Upload01Icon,
  UserMultipleIcon,
} from "@hugeicons/core-free-icons";

export type IconName =
  | "contacts"
  | "ai"
  | "blacklist"
  | "collapse"
  | "expand"
  | "settings"
  | "upload"
  | "sparkle"
  | "pause"
  | "play"
  | "stop"
  | "search"
  | "view"
  | "delete"
  | "download"
  | "file"
  | "folder"
  | "plus";

const icons: Record<IconName, IconSvgElement> = {
  contacts: UserMultipleIcon,
  ai: AiBrain01Icon,
  blacklist: BanIcon,
  collapse: ArrowLeft01Icon,
  expand: ArrowRight01Icon,
  settings: Settings01Icon,
  upload: Upload01Icon,
  sparkle: AiSparklesIcon,
  pause: PauseIcon,
  play: PlayIcon,
  stop: StopIcon,
  search: Search01Icon,
  view: ViewIcon,
  delete: Delete02Icon,
  download: Download01Icon,
  file: File01Icon,
  folder: FolderOpenIcon,
  plus: PlusSignIcon,
};

export function Icon({
  name,
  size = 18,
  strokeWidth = 1.5,
  className = "icon",
}: {
  name: IconName;
  size?: number;
  strokeWidth?: number;
  className?: string;
}) {
  return (
    <HugeiconsIcon
      icon={icons[name]}
      size={size}
      strokeWidth={strokeWidth}
      color="currentColor"
      className={className}
      aria-hidden="true"
    />
  );
}
