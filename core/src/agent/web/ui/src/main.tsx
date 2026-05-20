import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./app/globals.css";
import { setTheme } from "@/lib/theme";

// Apply persisted theme synchronously before React renders to avoid
// a dark-flash on light-mode page loads.
try {
  const stored = localStorage.getItem("cos.theme");
  setTheme((stored === "light" || stored === "system" ? stored : "dark") as any);
} catch {
  setTheme("dark");
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
