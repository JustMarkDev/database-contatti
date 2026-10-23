import React from "react";
import ReactDOM from "react-dom/client";
import "./App.css";
import App from "./App";
import { resolveWindowChrome } from "./lib/windowChrome";

// Set before the first render so the sidebar island lands at its final inset
// without a 14px -> 44px layout jump once effects run.
document.documentElement.dataset.windowChrome = resolveWindowChrome();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
