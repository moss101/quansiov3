/**
 * The web shell (APP-012): sign-in, the workspace, the navigation the session's roles
 * allow, and the page for the route — responsive by {@link layoutFor}.
 *
 * The shell offers what the roles allow (`offered(roles)`); everything it offers the
 * server decides again, and a refusal renders as the refusal. There is no host API on
 * this side: the shell holds a bearer session and talks to `/v1/` only.
 */

import { useCallback, useEffect, useMemo, useState, type ReactElement } from "react";

import { type WebClient } from "../client.js";
import { layoutFor, type Layout } from "../layout.js";
import { rolesFromMe, type Roles } from "../rbac.js";
import { type WebSession } from "../session.js";
import { WEB_SURFACES, offered, type WebSurfaceId } from "../surfaces.js";
import { Face, initials } from "./faces.js";
import { CloseIcon, LeaveIcon, MenuIcon, SearchIcon, StackIcon, SURFACE_ICONS } from "./icons.js";
import { Approvals, Artifacts, Audit, Capabilities, Connectors, Policies, Refusal, Usage, Users, WorkReview } from "./pages.js";
import { Palette, type PaletteItem } from "./Palette.js";
import { pathFor, routeFor, type Route } from "./router.js";
import { t, type StringKey } from "./strings.js";

const TITLES: Record<Exclude<WebSurfaceId, "sign-in">, StringKey> = {
  "work-review": "workReview",
  approvals: "approvals",
  artifacts: "artifacts",
  policies: "policies",
  connectors: "connectors",
  users: "users",
  capabilities: "capabilities",
  "audit-and-privacy": "audit",
  usage: "usage",
};

interface MeView {
  readonly user?: { readonly id?: string; readonly display_name?: string; readonly primary_email?: string };
  readonly workspaces?: readonly { readonly id: string; readonly name: string; readonly role: string }[];
}

export function App({ session, width }: { readonly session: WebSession; readonly width: number }): ReactElement {
  const [route, setRoute] = useState<Route>(() => routeFor(window.location.pathname));
  const [me, setMe] = useState<MeView | null>(null);
  const [meError, setMeError] = useState<unknown>(null);
  const [workspaceId, setWorkspaceId] = useState<string | null>(session.current?.workspace_id ?? null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const layout: Layout = layoutFor(width);
  const client: WebClient | null = useMemo(() => (session.signedIn ? session.client() : null), [session, session.signedIn]);

  useEffect(() => {
    const onPop = () => { setRoute(routeFor(window.location.pathname)); };
    window.addEventListener("popstate", onPop);
    return () => { window.removeEventListener("popstate", onPop); };
  }, []);

  useEffect(() => {
    if (!client) {
      return;
    }
    client.get<MeView>("/v1/me").then(
      (view) => {
        setMe(view);
        if (!workspaceId && view.workspaces && view.workspaces.length > 0) {
          setWorkspaceId(view.workspaces[0]?.id ?? null);
        }
      },
      (error: unknown) => { setMeError(error); },
    );
  }, [client]);

  // The phone drawer closes on Escape, and whenever the layout stops being a drawer.
  useEffect(() => {
    if (!menuOpen) {
      return;
    }
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") {
        setMenuOpen(false);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => { document.removeEventListener("keydown", onKey); };
  }, [menuOpen]);
  useEffect(() => {
    if (layout.navigation !== "drawer") {
      setMenuOpen(false);
    }
  }, [layout.navigation]);

  // Cmd/Ctrl+K opens the palette from anywhere once signed in.
  useEffect(() => {
    if (!client) {
      return;
    }
    const onKey = (event: KeyboardEvent): void => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setMenuOpen(false);
        setPaletteOpen(true);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => { document.removeEventListener("keydown", onKey); };
  }, [client]);

  const navigate = useCallback((next: Route) => {
    window.history.pushState({}, "", pathFor(next));
    setRoute(next);
    setMenuOpen(false);
  }, []);

  if (!session.signedIn || !client) {
    return <SignIn session={session} />;
  }
  const roles: Roles = rolesFromMe(me, workspaceId);
  const surfaces = offered(roles);
  const page = (): ReactElement => {
    if (meError) {
      return <Refusal error={meError} />;
    }
    if (!me) {
      return <p className="loading">{t("loading")}</p>;
    }
    if (!workspaceId) {
      return <p className="muted">{t("noWorkspaceRole")}</p>;
    }
    const props = { client, workspaceId, roles };
    switch (route.surface) {
      case "approvals":
        return <Approvals {...props} />;
      case "artifacts":
        return <Artifacts {...props} />;
      case "policies":
        return <Policies {...props} />;
      case "connectors":
        return <Connectors {...props} />;
      case "users":
        return <Users {...props} />;
      case "capabilities":
        return <Capabilities {...props} />;
      case "audit-and-privacy":
        return <Audit {...props} />;
      case "usage":
        return <Usage {...props} />;
      default:
        return <WorkReview {...props} />;
    }
  };
  const drawer = layout.navigation === "drawer";
  const name = me?.user?.display_name ?? me?.user?.primary_email ?? "";
  const signOut = (): void => { void session.signOut().then(() => { window.location.assign("/app/"); }); };
  const openPalette = (): void => {
    setMenuOpen(false);
    setPaletteOpen(true);
  };
  // What the palette offers is what the shell offers: the pages the roles allow, the person's
  // workspaces, and signing out.
  const paletteItems: readonly PaletteItem[] = [
    ...surfaces.map((descriptor): PaletteItem => {
      const Icon = SURFACE_ICONS[descriptor.id];
      return {
        key: `page-${descriptor.id}`,
        group: t("pages"),
        label: descriptor.id === "sign-in" ? t("signIn") : t(TITLES[descriptor.id]),
        icon: <Icon />,
        run: () => { navigate({ surface: descriptor.id, id: null }); },
      };
    }),
    ...(me?.workspaces ?? []).map((workspace): PaletteItem => ({
      key: `workspace-${workspace.id}`,
      group: t("workspaces"),
      label: workspace.name,
      detail: workspace.role,
      icon: <StackIcon />,
      run: () => { setWorkspaceId(workspace.id); },
    })),
    { key: "action-sign-out", group: t("actions"), label: t("signOut"), icon: <LeaveIcon />, run: signOut },
  ];
  return (
    <div className={`shell layout-${layout.mode} nav-${layout.navigation}${menuOpen ? " menu-open" : ""}`} data-layout={layout.mode} data-columns={String(layout.columns)}>
      {drawer ? (
        <header className="topbar">
          <button type="button" className="icon-button" aria-label={t("menu")} aria-expanded={menuOpen} onClick={() => { setMenuOpen(!menuOpen); }}>
            <MenuIcon size={18} />
          </button>
          <span className="brand-name" aria-hidden="true">{t("appName")}</span>
          <span className="topbar-end">
            <button type="button" className="icon-button" aria-label={t("search")} onClick={openPalette}>
              <SearchIcon size={18} />
            </button>
            <Face id={me?.user?.id ?? "me"} text={initials(me?.user?.display_name, me?.user?.primary_email)} size="sm" neutral />
          </span>
        </header>
      ) : null}
      {drawer && menuOpen ? <div className="scrim" aria-hidden="true" onClick={() => { setMenuOpen(false); }} /> : null}
      <aside className="sidebar" aria-hidden={drawer && !menuOpen ? true : undefined}>
        <div className="brand">
          <span className="brand-mark" aria-hidden="true" />
          <h1>{t("appName")}</h1>
          {drawer ? null : (
            <button type="button" className="icon-button search-button" aria-label={t("search")} data-tip={t("search")} onClick={openPalette}>
              <SearchIcon />
            </button>
          )}
          {drawer ? (
            <button type="button" className="icon-button" aria-label={t("closeMenu")} onClick={() => { setMenuOpen(false); }}>
              <CloseIcon size={16} />
            </button>
          ) : null}
        </div>
        <label className="workspace">
          <span>{t("workspace")}</span>
          <select value={workspaceId ?? ""} onChange={(event) => { setWorkspaceId(event.target.value || null); }}>
            {(me?.workspaces ?? []).map((workspace) => <option key={workspace.id} value={workspace.id}>{workspace.name} ({workspace.role})</option>)}
          </select>
        </label>
        <nav aria-label={t("pages")}>
          <ul>
            {surfaces.map((descriptor) => {
              const Icon = SURFACE_ICONS[descriptor.id];
              const label = descriptor.id === "sign-in" ? t("signIn") : t(TITLES[descriptor.id]);
              const current = route.surface === descriptor.id;
              return (
                <li key={descriptor.id} className={current ? "selected" : undefined} data-surface={descriptor.id} data-admin={descriptor.adminOnly ? "true" : "false"}>
                  <button type="button" aria-label={label} aria-current={current ? "page" : undefined} data-tip={layout.navigation === "rail" ? label : undefined} onClick={() => { navigate({ surface: descriptor.id, id: null }); }}>
                    <Icon />
                    <span className="nav-label">{label}</span>
                  </button>
                </li>
              );
            })}
          </ul>
        </nav>
        <div className="account">
          <Face id={me?.user?.id ?? "me"} text={initials(me?.user?.display_name, me?.user?.primary_email)} size="md" neutral />
          <span className="account-text">
            <span className="account-name">{name}</span>
            <span className="account-roles">{roles.tenant ?? t("noTenantRole")} · {roles.workspace ?? t("noWorkspaceRoleShort")}</span>
          </span>
          <button type="button" className="icon-button" aria-label={t("signOut")} data-tip={t("signOut")} onClick={signOut}>
            <LeaveIcon />
          </button>
        </div>
      </aside>
      <main>{page()}</main>
      {paletteOpen ? <Palette items={paletteItems} onClose={() => { setPaletteOpen(false); }} /> : null}
    </div>
  );
}

function SignIn({ session }: { readonly session: WebSession }): ReactElement {
  const [email, setEmail] = useState("");
  const [name, setName] = useState("");
  const [state, setState] = useState<"idle" | "sent" | "completing" | "error">(() => (new URLSearchParams(window.location.search).get("token") ? "completing" : "idle"));
  const [message, setMessage] = useState<string | null>(null);
  useEffect(() => {
    const token = new URLSearchParams(window.location.search).get("token");
    if (token) {
      session.completeMagicLink(token).then(
        () => { window.location.assign("/app/"); },
        (error: unknown) => { setState("error"); setMessage(error instanceof Error ? error.message : String(error)); },
      );
    }
  }, [session]);
  return (
    <div className="shell sign-in">
      <main>
        <span className="brand-mark large" aria-hidden="true" />
        <h1>{t("appName")}</h1>
        <p className="lede">{t("signInTitle")}</p>
        {state === "completing" ? <p className="loading">{t("completing")}</p> : null}
        {state === "sent" ? <p className="sent" role="status">{t("linkSent")}</p> : null}
        {message ? <p className="notice" role="alert">{message}</p> : null}
        {state === "idle" || state === "error" ? (
          <form onSubmit={(event) => { event.preventDefault(); session.startMagicLink(email, name || email).then(() => { setState("sent"); }, (error: unknown) => { setState("error"); setMessage(error instanceof Error ? error.message : String(error)); }); }}>
            <input aria-label={t("email")} placeholder={t("email")} value={email} onChange={(event) => { setEmail(event.target.value); }} />
            <input aria-label={t("displayName")} placeholder={t("displayName")} value={name} onChange={(event) => { setName(event.target.value); }} />
            <button type="submit">{t("sendLink")}</button>
          </form>
        ) : null}
      </main>
    </div>
  );
}

export { WEB_SURFACES };
