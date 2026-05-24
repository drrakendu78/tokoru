import { ButtonHTMLAttributes, ReactNode } from "react";

interface CoralButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: "primary" | "secondary" | "ghost" | "danger-outline";
  size?: "sm" | "md" | "lg";
  children: ReactNode;
}

export function CoralButton({
  variant = "primary",
  size = "md",
  children,
  className = "",
  ...rest
}: CoralButtonProps) {
  const base = "inline-flex items-center justify-center gap-2 rounded-xl font-semibold transition-all active:scale-95 disabled:opacity-40 disabled:cursor-not-allowed disabled:active:scale-100 disabled:hover:bg-[unset]";
  const sizes: Record<string, string> = {
    sm: "px-3 py-1.5 text-[12px]",
    md: "px-4 py-2 text-[13px]",
    lg: "px-6 py-3 text-[14px]",
  };
  const variants: Record<string, string> = {
    primary: "bg-accent hover:bg-accent-hover text-white shadow-glow",
    secondary:
      "bg-white/[0.04] hover:bg-white/[0.08] text-white border border-white/[0.06]",
    ghost: "text-text-sec hover:text-white hover:bg-white/[0.04]",
    "danger-outline":
      "text-accent border border-accent/40 hover:text-white hover:bg-accent",
  };
  return (
    <button
      {...rest}
      className={`${base} ${sizes[size]} ${variants[variant]} ${className}`}
    >
      {children}
    </button>
  );
}
