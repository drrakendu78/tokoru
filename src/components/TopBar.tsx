import { ReactNode } from "react";
import { NavPills } from "./NavPill";
import { WindowControls } from "./WindowControls";
import { useTheme } from "../lib/useTheme";

interface TopBarProps {
  rightSlot?: ReactNode;
  withBorder?: boolean;
  acrylic?: boolean;
}

export function TopBar({
  rightSlot,
  withBorder = true,
  acrylic = false,
}: TopBarProps) {
  const { resolved } = useTheme();
  return (
    <header
      className={`relative z-50 h-[56px] w-full flex items-center justify-between px-4 window-drag ${
        withBorder ? "border-b border-white/[0.04]" : ""
      } ${acrylic ? "acrylic" : "bg-shell/80 backdrop-blur-md"}`}
    >
      {/* Left: Logo + Nav */}
      <div className="flex items-center gap-6 no-drag">
        <div className="flex items-center gap-2 text-text-main pl-2">
          <img
            src={resolved === "light" ? "/logo-light.png" : "/logo-dark.png"}
            alt="Tokoru"
            width={28}
            height={28}
            className="shrink-0 select-none"
            draggable={false}
            data-theme-asset="logo"
          />
          <span className="font-semibold tracking-wide text-[15px] mr-2">
            Tokoru
          </span>
        </div>
        <NavPills />
      </div>

      {/* Right: page-specific tools + window controls */}
      <div className="flex items-center gap-4 no-drag">
        {rightSlot}
        <WindowControls />
      </div>
    </header>
  );
}
