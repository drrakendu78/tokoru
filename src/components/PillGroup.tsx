interface Option<T extends string> {
  value: T;
  label: string;
}
interface PillGroupProps<T extends string> {
  options: Option<T>[];
  value: T;
  onChange: (v: T) => void;
  size?: "sm" | "md";
}

export function PillGroup<T extends string>({
  options,
  value,
  onChange,
  size = "md",
}: PillGroupProps<T>) {
  const sizeClasses =
    size === "sm"
      ? "px-3 py-1 text-[11px]"
      : "px-3 py-1.5 text-[12px]";
  return (
    <div className="p-1 bg-white/[0.03] border border-white/[0.04] rounded-xl flex items-center">
      {options.map((opt) => {
        const selected = opt.value === value;
        return (
          <button
            key={opt.value}
            onClick={() => onChange(opt.value)}
            className={`${sizeClasses} font-medium rounded-lg transition-colors ${
              selected
                ? "text-accent bg-accent/15 border border-accent/20 font-bold shadow-sm cursor-default"
                : "text-text-sec hover:text-white"
            }`}
          >
            {opt.label}
          </button>
        );
      })}
    </div>
  );
}
