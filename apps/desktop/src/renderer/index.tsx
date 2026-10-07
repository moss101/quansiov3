/**
 * Renderer entry (APP-003, ADP-004): mounts the surface this window is for; the bundle is loaded from the `app:` scheme.
 *
 * Every window loads this same bundle. Which surface it shows is what main says the window is (`windowContext`, answered
 * from the type main recorded when it created the window), never anything in the page's URL. If main cannot say, the
 * page is the main window, which is also what a build without the shell's windows would show.
 */

import { createRoot } from "react-dom/client";

import { decodeWindowContext, type WindowContext } from "../shell-ui.js";
import { App } from "./App.js";
import { Indicator } from "./Indicator.js";
import { Popout } from "./Popout.js";
import { QuickEntry } from "./QuickEntry.js";

const container = document.getElementById("root");
if (!container) {
  throw new Error("renderer root element is missing");
}
const root = createRoot(container);

function surface(context: WindowContext | null): React.ReactElement {
  switch (context?.type) {
    case "popout":
      return context.thread ? <Popout threadId={context.thread.threadId} inspectorOpen={context.thread.tab === "inspector"} /> : <App />;
    case "quick_entry":
      return <QuickEntry />;
    case "indicator":
      return <Indicator />;
    default:
      return <App />;
  }
}

void window.quansio
  .windowContext()
  .then(decodeWindowContext, () => null)
  .then((context) => {
    document.documentElement.dataset["window"] = context?.type ?? "main";
    root.render(surface(context));
  });
