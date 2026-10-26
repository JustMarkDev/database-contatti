import { useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import { ArrowDown01Icon } from "@hugeicons/core-free-icons";
import { cn } from "../lib/utils";
import { Button } from "./ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";
import {
  Command,
  CommandCheckIcon,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "./ui/command";

export type SingleSelectOption = {
  value: string;
  label: string;
  /** Shown at the right of the option, e.g. how many sites it holds. */
  count?: number;
  group?: string;
};

/** One choice out of many, with search and groups (a native <select> cannot do either). */
export function SingleSelect({
  label,
  placeholder,
  searchPlaceholder = "Cerca…",
  value,
  options,
  onChange,
  disabled,
}: {
  label: string;
  placeholder: string;
  searchPlaceholder?: string;
  value: string;
  options: SingleSelectOption[];
  onChange: (value: string) => void;
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const selected = options.find((option) => option.value === value);
  const groups = [...new Set(options.map((option) => option.group ?? ""))];
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger
        render={
          <Button
            variant="outline"
            disabled={disabled}
            aria-label={label}
            className="h-10 w-full justify-between px-3 font-normal"
          />
        }
      >
        <span className={cn("truncate", !selected && "text-faint")}>
          {selected?.label ?? placeholder}
        </span>
        <HugeiconsIcon
          icon={ArrowDown01Icon}
          size={16}
          strokeWidth={1.5}
          color="currentColor"
          aria-hidden="true"
          className="size-4 shrink-0 text-faint"
        />
      </PopoverTrigger>
      <PopoverContent className="w-[max(var(--anchor-width),260px)] p-0" align="start">
        <Command>
          <CommandInput placeholder={searchPlaceholder} />
          <CommandList className="max-h-72 p-1">
            <CommandEmpty>Nessun risultato</CommandEmpty>
            {groups.map((group) => (
              <CommandGroup key={group} heading={group || undefined}>
                {options
                  .filter((option) => (option.group ?? "") === group)
                  .map((option) => (
                    <CommandItem
                      key={option.value}
                      value={`${group} ${option.label}`}
                      onSelect={() => {
                        onChange(option.value);
                        setOpen(false);
                      }}
                    >
                      <span className="min-w-0 flex-1 truncate">{option.label}</span>
                      {option.count !== undefined && (
                        <span className="text-[11px] text-faint tabular-nums">
                          {option.count.toLocaleString("it-IT")}
                        </span>
                      )}
                      <CommandCheckIcon checked={option.value === value} />
                    </CommandItem>
                  ))}
              </CommandGroup>
            ))}
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  );
}
