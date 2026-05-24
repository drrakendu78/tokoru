import { useTranslation } from "react-i18next";
import { useRouter, Route } from "../router";

// Route → translation key for the nav label. Keys live under `nav.*` in
// the locale files so a single edit there flips every language.
const NAV: { tKey: string; route: Route }[] = [
  { tKey: "nav.library", route: "library" },
  { tKey: "nav.downloads", route: "downloads" },
  { tKey: "nav.sources", route: "sources" },
  { tKey: "nav.stats", route: "stats" },
  { tKey: "nav.settings", route: "settings" },
];

export function NavPills() {
  const { t } = useTranslation();
  const { route, setRoute } = useRouter();
  return (
    <nav className="flex p-1 bg-white/[0.03] rounded-full border border-white/[0.02]">
      {NAV.map((item) => {
        const selected = route === item.route;
        return (
          <button
            key={item.route}
            onClick={() => setRoute(item.route)}
            className={`px-4 py-1.5 rounded-full text-sm font-medium transition-colors ${
              selected
                ? "bg-white/[0.08] text-white shadow-sm cursor-default"
                : "text-text-sec hover:text-white hover:bg-white/[0.04] cursor-pointer"
            }`}
          >
            {t(item.tKey)}
          </button>
        );
      })}
    </nav>
  );
}
