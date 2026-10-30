// Labelled form controls: shadcn base-nova Input and Select (what Pasta UI's
// components are built on) under a Pasta metadata label.

import { useId, type ComponentProps, type ReactNode } from "react";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { cn } from "@/lib/utils";

function Field({ id, label, children, className }: { id: string; label: string; children: ReactNode; className?: string }) {
  return (
    <div className={cn("grid gap-1.5", className)}>
      <label htmlFor={id} className="px-0.5 metadata-label">
        {label}
      </label>
      {children}
    </div>
  );
}

const control = "h-9 rounded-[10px] border-0 bg-foreground/[0.04] text-[13px] font-[450] shadow-none dark:bg-foreground/[0.06]";

export function InputField({ label, className, ...props }: ComponentProps<"input"> & { label: string }) {
  const id = useId();
  return (
    <Field id={id} label={label} className={className}>
      <Input id={id} className={cn(control, "px-3")} {...props} />
    </Field>
  );
}

export type Option = { value: string; label: string };

export function SelectField({ label, value, options, onValueChange, className }: { label: string; value: string; options: Option[]; onValueChange: (v: string) => void; className?: string }) {
  const id = useId();
  return (
    <Field id={id} label={label} className={className}>
      <Select items={options} value={value} onValueChange={v => v != null && onValueChange(v as string)}>
        <SelectTrigger id={id} className={cn(control, "w-full pr-2.5 pl-3")}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent className="rounded-[14px] p-1 shadow-card ring-0">
          {options.map(o => (
            <SelectItem key={o.value} value={o.value} className="rounded-[10px] px-2.5 py-1.5 text-[13px] focus:bg-foreground/6">
              {o.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </Field>
  );
}
