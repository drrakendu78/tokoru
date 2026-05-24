import { createContext, useContext, useState, useCallback, ReactNode } from "react";

export type Route =
  | "library"
  | "downloads"
  | "sources"
  | "stats"
  | "settings"
  | "onboarding";

interface RouterContextValue {
  route: Route;
  setRoute: (r: Route) => void;
  detailOpen: string | null;
  openDetail: (id: string) => void;
  closeDetail: () => void;
}

const RouterContext = createContext<RouterContextValue | undefined>(undefined);

export function RouterProvider({
  initial,
  children,
}: {
  initial: Route;
  children: ReactNode;
}) {
  const [route, setRoute] = useState<Route>(initial);
  const [detailOpen, setDetailOpen] = useState<string | null>(null);

  const openDetail = useCallback((id: string) => setDetailOpen(id), []);
  const closeDetail = useCallback(() => setDetailOpen(null), []);

  return (
    <RouterContext.Provider
      value={{ route, setRoute, detailOpen, openDetail, closeDetail }}
    >
      {children}
    </RouterContext.Provider>
  );
}

export function useRouter(): RouterContextValue {
  const ctx = useContext(RouterContext);
  if (!ctx) throw new Error("useRouter must be used inside RouterProvider");
  return ctx;
}
