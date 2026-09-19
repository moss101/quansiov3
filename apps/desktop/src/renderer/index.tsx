/** Renderer entry (APP-003): mounts the app; the bundle is loaded from the `app:` scheme. */

import { createRoot } from "react-dom/client";

import { App } from "./App.js";

const container = document.getElementById("root");
if (!container) {
  throw new Error("renderer root element is missing");
}
createRoot(container).render(<App />);
