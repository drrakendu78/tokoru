interface ToggleProps {
  checked: boolean;
  onChange: (next: boolean) => void;
  ariaLabel?: string;
  disabled?: boolean;
}

export function Toggle({ checked, onChange, ariaLabel, disabled }: ToggleProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={ariaLabel}
      disabled={disabled}
      onClick={() => !disabled && onChange(!checked)}
      className={`relative inline-flex h-6 w-11 flex-shrink-0 rounded-full border-2 border-transparent transition-colors duration-200 ease-in-out focus:outline-none ${
        disabled
          ? "bg-white/5 cursor-not-allowed"
          : checked
          ? "bg-accent shadow-glow cursor-pointer"
          : "bg-white/10 hover:bg-white/15 cursor-pointer"
      }`}
    >
      <span
        className={`pointer-events-none inline-block h-5 w-5 transform rounded-full shadow ring-0 transition duration-200 ease-in-out ${
          checked
            ? "translate-x-5 bg-white"
            : disabled
            ? "translate-x-0 bg-white/20"
            : "translate-x-0 bg-text-sec"
        }`}
      />
    </button>
  );
}
