import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import App from "./App";
import { isDesktop } from "./api/transport";

// Apply desktop class early so CSS scoping is available before first paint.
if (isDesktop()) {
  document.documentElement.classList.add("desktop");
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
