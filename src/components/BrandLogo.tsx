// Brand logos from SimpleIcons (MIT) downloaded into /public/logos.
// Rendered as CSS-mask silhouettes so we can recolor them per source
// (white on a colored disc, or in their own brand color on a dark surface).

interface BrandLogoProps {
  source: string;
  size?: number;
  color?: string;
  className?: string;
}

const BRAND_FILE: Record<string, string | null> = {
  steam: "/logos/steam.svg",
  epic: "/logos/epicgames.svg",
  gog: "/logos/gogdotcom.svg",
  ubi: "/logos/ubisoft.svg",
  ea: "/logos/ea.svg",
  itch: "/logos/itchdotio.svg",
  custom: null,
};

const FALLBACK_INITIALS: Record<string, string> = {
  custom: ".EXE",
};

export function BrandLogo({
  source,
  size = 24,
  color = "#FFFFFF",
  className = "",
}: BrandLogoProps) {
  const file = BRAND_FILE[source];
  if (!file) {
    const text = FALLBACK_INITIALS[source] ?? source.slice(0, 3).toUpperCase();
    return (
      <span
        className={`font-bold tracking-tight ${className}`}
        style={{ color, fontSize: Math.max(8, Math.floor(size * 0.42)) }}
      >
        {text}
      </span>
    );
  }
  return (
    <span
      className={`inline-block shrink-0 ${className}`}
      style={{
        width: size,
        height: size,
        backgroundColor: color,
        WebkitMaskImage: `url(${file})`,
        WebkitMaskRepeat: "no-repeat",
        WebkitMaskSize: "contain",
        WebkitMaskPosition: "center",
        maskImage: `url(${file})`,
        maskRepeat: "no-repeat",
        maskSize: "contain",
        maskPosition: "center",
      }}
      aria-label={source}
    />
  );
}
