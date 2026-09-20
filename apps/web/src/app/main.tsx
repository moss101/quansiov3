/**
 * The browser entry (APP-012). The origin is the page's own: the app is served by the
 * server it talks to (or by a CDN in front of it). A development harness may pass
 * `?tenant=tn_…&user=usr_…` to use the trusted-header scope the server accepts only when
 * started with `QUANSIO_TRUST_SCOPE_HEADERS=1`.
 */

import { StrictMode, useEffect, useState, type ReactElement } from "react";
import { createRoot } from "react-dom/client";

import { WebSession } from "../session.js";
import { App } from "./App.js";

function harnessScope(): { tenantId: string; userId: string } | null {
  const params = new URLSearchParams(window.location.search);
  const tenantId = params.get("tenant");
  const userId = params.get("user");
  return tenantId?.startsWith("tn_") && userId?.startsWith("usr_") ? { tenantId, userId } : null;
}

function Root(): ReactElement {
  const [width, setWidth] = useState(window.innerWidth);
  const [session] = useState(() => new WebSession(window.location.origin, { storage: safeStorage(), harness: harnessScope() }));
  useEffect(() => {
    const onResize = () => { setWidth(window.innerWidth); };
    window.addEventListener("resize", onResize);
    return () => { window.removeEventListener("resize", onResize); };
  }, []);
  return <App session={session} width={width} />;
}

function safeStorage(): Storage | null {
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
}

const container = document.getElementById("root");
if (container) {
  createRoot(container).render(
    <StrictMode>
      <Root />
    </StrictMode>,
  );
}
