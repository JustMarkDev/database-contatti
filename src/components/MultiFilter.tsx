import { useMemo, useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import { ArrowDown01Icon } from "@hugeicons/core-free-icons";
import { cn } from "../lib/utils";
import type { FieldFilter, FilterOption } from "../types";
import { ScrollIndicators, useScrollActivity } from "./ScrollIndicators";
import { Button } from "./ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";
import {
  Command,
  CommandCheckIcon,
  CommandEmpty,
  CommandInput,
  CommandItem,
  CommandList,
} from "./ui/command";

export function MultiFilter({
  label,
  options = [],
  value = { include: [], exclude: [] },
  onChange,
  formatValue = (value) => value,
}: {
  label: string;
  options?: FilterOption[];
  value?: FieldFilter;
  onChange: (next: FieldFilter) => void;
  formatValue?: (value: string) => string;
}) {
  const [open, setOpen] = useState(false);
  // Edits stay local while the menu is open and are committed once on close, so
  // toggling options is instant and triggers a single contacts query.
  const [draft, setDraft] = useState<FieldFilter | null>(null);
  const current = draft ?? value;
  const scrollActivity = useScrollActivity();
  function handleOpenChange(next: boolean) {
    if (next) setDraft(value);
    else {
      if (draft && !sameFilter(draft, value)) onChange(draft);
      setDraft(null);
    }
    setOpen(next);
  }
  function cycle(option: string) {
    if (current.include.includes(option))
      setDraft({
        include: current.include.filter((v) => v !== option),
        exclude: [...current.exclude, option],
      });
    else if (current.exclude.includes(option))
      setDraft({ include: current.include, exclude: current.exclude.filter((v) => v !== option) });
    else setDraft({ include: [...current.include, option], exclude: current.exclude });
  }
  const activeCount = current.include.length + current.exclude.length;
  const summary = [
    current.include.length
      ? `${current.include.length} inclus${current.include.length === 1 ? "o" : "i"}`
      : "",
    current.exclude.length
      ? `${current.exclude.length} esclus${current.exclude.length === 1 ? "o" : "i"}`
      : "",
  ]
    .filter(Boolean)
    .join(", ");
  const stateOf = useMemo(
    () => (option: string) =>
      current.include.includes(option)
        ? "include"
        : current.exclude.includes(option)
          ? "exclude"
          : "neutral",
    [current],
  );
  return (
    <div className="multi-filter" data-active={activeCount > 0}>
      <Popover open={open} onOpenChange={handleOpenChange}>
        <PopoverTrigger
          render={
            <Button
              variant="outline"
              size="sm"
              className={cn(
                "h-7 px-2.5 font-normal shadow-none",
                activeCount > 0 && "border-brand/40 bg-brand/10 hover:bg-brand/15",
              )}
            />
          }
        >
          <span className="filter-chip-label">{label}</span>
          {summary && <span className="filter-chip-value">{summary}</span>}
          <HugeiconsIcon
            icon={ArrowDown01Icon}
            size={14}
            strokeWidth={1.75}
            color="currentColor"
            aria-hidden="true"
            className="-mr-1 size-3.5 text-faint"
          />
        </PopoverTrigger>
        <PopoverContent className="filter-menu-shell" align="start" keepMounted>
          <Command>
            <CommandInput placeholder={`Cerca ${label.toLocaleLowerCase("it")}…`} />
            <div className="filter-menu-scroll-shell">
              <CommandList
                ref={scrollActivity.ref}
                onScroll={scrollActivity.onScroll}
                className="filter-menu"
              >
                <FilterOptions
                  options={options}
                  formatValue={formatValue}
                  stateOf={stateOf}
                  onSelect={cycle}
                />
              </CommandList>
              <ScrollIndicators activity={scrollActivity} />
            </div>
          </Command>
        </PopoverContent>
      </Popover>
    </div>
  );
}

const sameSet = (a: string[], b: string[]) =>
  a.length === b.length && a.every((v) => b.includes(v));
const sameFilter = (a: FieldFilter, b: FieldFilter) =>
  sameSet(a.include, b.include) && sameSet(a.exclude, b.exclude);

function FilterOptions({
  options = [],
  formatValue,
  stateOf,
  onSelect,
}: {
  options?: FilterOption[];
  formatValue: (value: string) => string;
  stateOf: (option: string) => "include" | "exclude" | "neutral";
  onSelect: (option: string) => void;
}) {
  if (!options || options.length === 0)
    return <CommandEmpty>Nessun valore disponibile</CommandEmpty>;
  return (
    <>
      <CommandEmpty>Nessun valore corrispondente</CommandEmpty>
      {options.map((option) => {
        const state = stateOf(option.value);
        return (
          <CommandItem
            key={option.value}
            value={formatValue(option.value)}
            onSelect={() => onSelect(option.value)}
            className={`filter-option ${state}`}
            aria-label={`${formatValue(option.value)}: ${state === "include" ? "incluso" : state === "exclude" ? "escluso" : "neutro"}`}
          >
            <span className="filter-checkbox shrink-0" aria-hidden="true">
              {state === "include" ? "✓" : state === "exclude" ? "−" : ""}
            </span>
            <span className="filter-option-label">{formatValue(option.value)}</span>
            <span className="filter-option-count">{option.count}</span>
            <CommandCheckIcon checked={state === "include"} />
          </CommandItem>
        );
      })}
    </>
  );
}
