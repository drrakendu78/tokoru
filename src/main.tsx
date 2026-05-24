import React from "react";
import ReactDOM from "react-dom/client";
import "./index.css";
// Bootstrap i18next BEFORE App so the first render already has the right
// language. Side-effect import — see src/i18n/index.ts.
import "./i18n";
import App from "./App";
import { ThemeProvider } from "./lib/useTheme";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ThemeProvider>
      <App />
    </ThemeProvider>
  </React.StrictMode>
);
