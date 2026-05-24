import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";

export type Theme = "dark" | "light" | "system";

export type AccentPreset = {
  key: string;
  label: string;
  hex: string;
  /** Slightly lighter shade used for hover states. */
  hover: string;
};

/** 10 accent presets — first entry (`coral`) is the Tokoru default brand
 * color. Picked to read well on both dark graphite and light off-white
 * surfaces (saturation tuned, no neons that bloom on white). */
export const ACCENT_PRESETS: AccentPreset[] = [
  { key: "coral", label: "Coral", hex: "#FF4633", hover: "#FF5747" },
  { key: "orange", label: "Orange", hex: "#FF8C00", hover: "#FF9D1F" },
  { key: "magenta", label: "Magenta", hex: "#E024C4", hover: "#E63ECF" },
  { key: "rose", label: "Rose", hex: "#F43F5E", hover: "#F55872" },
  { key: "purple", label: "Purple", hex: "#A855F7", hover: "#B36AF8" },
  { key: "blue", label: "Blue", hex: "#3B82F6", hover: "#5896F7" },
  { key: "cyan", label: "Cyan", hex: "#00D1FF", hover: "#1FD8FF" },
  { key: "mint", label: "Mint", hex: "#14B8A6", hover: "#2AC4B3" },
  { key: "green", label: "Green", hex: "#10B981", hover: "#26C291" },
  { key: "yellow", label: "Yellow", hex: "#EAB308", hover: "#F0BE26" },
];

const STORAGE_KEY = "tokoru.theme";
const ACCENT_KEY = "tokoru.accent";
const REDUCE_MOTION_KEY = "tokoru.reduce_motion";
const DENSITY_KEY = "tokoru.density";

export type Density = "compact" | "comfortable" | "spacious";

/** Cycle order used by the Library grid icon: compact → comfortable → spacious → compact. */
export const DENSITY_CYCLE: Density[] = ["compact", "comfortable", "spacious"];

/** Blend a hex color toward white to derive a hover shade. */
function lighten(hex: string, amount = 0.1): string {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex);
  if (!m) return hex;
  const n = parseInt(m[1], 16);
  let r = (n >> 16) & 0xff;
  let g = (n >> 8) & 0xff;
  let b = n & 0xff;
  r = Math.round(r + (255 - r) * amount);
  g = Math.round(g + (255 - g) * amount);
  b = Math.round(b + (255 - b) * amount);
  return "#" + ((r << 16) | (g << 8) | b).toString(16).padStart(6, "0");
}

/** Resolved active accent — either one of the presets, or a user-picked
 * arbitrary hex (kind === "custom"). `hex` and `hover` are always set so
 * callers don't need to branch on `kind`. */
export type ActiveAccent = {
  kind: "preset" | "custom";
  /** Preset key when `kind === "preset"`, else literal `"custom"`. */
  key: string;
  label: string;
  hex: string;
  hover: string;
};

type Ctx = {
  theme: Theme;
  resolved: "dark" | "light";
  setTheme: (t: Theme) => void;
  accent: ActiveAccent;
  /** Pick a built-in preset by its key (`"coral"`, `"orange"`, …). */
  setAccent: (key: string) => void;
  /** Pick any custom hex color; hover is derived automatically. */
  setCustomAccent: (hex: string) => void;
  /** When true, all CSS animations and transitions are disabled globally
   *  via a `data-reduce-motion` attribute on `<html>`. */
  reduceMotion: boolean;
  setReduceMotion: (v: boolean) => void;
  /** Card layout density — drives a `data-density` attribute on `<html>`
   *  that components can react to (e.g. GameCard padding / row gaps). */
  density: Density;
  setDensity: (d: Density) => void;
};

const DEFAULT_ACCENT: ActiveAccent = {
  kind: "preset",
  ...ACCENT_PRESETS[0],
};

const ThemeCtx = createContext<Ctx>({
  theme: "dark",
  resolved: "dark",
  setTheme: () => {},
  accent: DEFAULT_ACCENT,
  setAccent: () => {},
  setCustomAccent: () => {},
  reduceMotion: false,
  setReduceMotion: () => {},
  density: "comfortable",
  setDensity: () => {},
});

function readSystem(): "dark" | "light" {
  return window.matchMedia("(prefers-color-scheme: light)").matches
    ? "light"
    : "dark";
}

function readStored(): Theme {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (v === "dark" || v === "light" || v === "system") return v;
  } catch {}
  return "dark";
}

/** Stored format:
 *   - preset:  `"coral"` (the preset key)
 *   - custom:  `"#FF7700"` (literal hex, recognized by the leading `#`) */
function readAccent(): ActiveAccent {
  try {
    const v = localStorage.getItem(ACCENT_KEY);
    if (v && v.startsWith("#") && /^#[0-9a-f]{6}$/i.test(v)) {
      return {
        kind: "custom",
        key: "custom",
        label: "Custom",
        hex: v.toUpperCase(),
        hover: lighten(v),
      };
    }
    const found = ACCENT_PRESETS.find((p) => p.key === v);
    if (found) return { kind: "preset", ...found };
  } catch {}
  return DEFAULT_ACCENT;
}

function readReduceMotion(): boolean {
  try {
    return localStorage.getItem(REDUCE_MOTION_KEY) === "1";
  } catch {
    return false;
  }
}

function readDensity(): Density {
  try {
    const v = localStorage.getItem(DENSITY_KEY);
    if (v === "compact" || v === "comfortable" || v === "spacious") return v;
  } catch {}
  return "comfortable";
}

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setThemeState] = useState<Theme>(readStored);
  const [resolved, setResolved] = useState<"dark" | "light">(
    theme === "system" ? readSystem() : theme
  );
  const [accent, setAccentObj] = useState<ActiveAccent>(readAccent);
  const [reduceMotion, setReduceMotionState] =
    useState<boolean>(readReduceMotion);
  const [density, setDensityState] = useState<Density>(readDensity);

  useEffect(() => {
    const next: "dark" | "light" = theme === "system" ? readSystem() : theme;
    setResolved(next);
    document.documentElement.setAttribute("data-theme", next);
  }, [theme]);

  useEffect(() => {
    if (theme !== "system") return;
    const mql = window.matchMedia("(prefers-color-scheme: light)");
    const handler = () => {
      const next: "dark" | "light" = mql.matches ? "light" : "dark";
      setResolved(next);
      document.documentElement.setAttribute("data-theme", next);
    };
    mql.addEventListener("change", handler);
    return () => mql.removeEventListener("change", handler);
  }, [theme]);

  useEffect(() => {
    document.documentElement.style.setProperty("--color-accent", accent.hex);
    document.documentElement.style.setProperty(
      "--color-accent-hover",
      accent.hover
    );
  }, [accent]);

  useEffect(() => {
    if (reduceMotion) {
      document.documentElement.setAttribute("data-reduce-motion", "true");
    } else {
      document.documentElement.removeAttribute("data-reduce-motion");
    }
  }, [reduceMotion]);

  useEffect(() => {
    document.documentElement.setAttribute("data-density", density);
  }, [density]);

  const setTheme = useCallback((t: Theme) => {
    try {
      localStorage.setItem(STORAGE_KEY, t);
    } catch {}
    setThemeState(t);
  }, []);

  const setAccent = useCallback((key: string) => {
    const found = ACCENT_PRESETS.find((p) => p.key === key);
    if (!found) return;
    try {
      localStorage.setItem(ACCENT_KEY, found.key);
    } catch {}
    setAccentObj({ kind: "preset", ...found });
  }, []);

  const setCustomAccent = useCallback((hex: string) => {
    if (!/^#[0-9a-f]{6}$/i.test(hex)) return;
    const normalized = hex.toUpperCase();
    try {
      localStorage.setItem(ACCENT_KEY, normalized);
    } catch {}
    setAccentObj({
      kind: "custom",
      key: "custom",
      label: "Custom",
      hex: normalized,
      hover: lighten(normalized),
    });
  }, []);

  const setReduceMotion = useCallback((v: boolean) => {
    try {
      localStorage.setItem(REDUCE_MOTION_KEY, v ? "1" : "0");
    } catch {}
    setReduceMotionState(v);
  }, []);

  const setDensity = useCallback((d: Density) => {
    try {
      localStorage.setItem(DENSITY_KEY, d);
    } catch {}
    setDensityState(d);
  }, []);

  return (
    <ThemeCtx.Provider
      value={{
        theme,
        resolved,
        setTheme,
        accent,
        setAccent,
        setCustomAccent,
        reduceMotion,
        setReduceMotion,
        density,
        setDensity,
      }}
    >
      {children}
    </ThemeCtx.Provider>
  );
}

export function useTheme() {
  return useContext(ThemeCtx);
}
