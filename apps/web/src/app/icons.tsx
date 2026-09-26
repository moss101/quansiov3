/**
 * Line icons for the web app (APP-012), drawn on a 24-unit grid with round joins. Decorative
 * only: every control that shows one carries its own accessible name, so each icon is
 * `aria-hidden` and never focusable.
 */

import type { ReactElement, ReactNode } from "react";

import type { WebSurfaceId } from "../surfaces.js";

interface IconProps {
  readonly size?: number;
}

function Icon({ children, size = 16 }: { readonly children: ReactNode; readonly size?: number | undefined }): ReactElement {
  return (
    <svg className="icon" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.9} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">
      {children}
    </svg>
  );
}

export const MenuIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M4 7h16M4 12h16M4 17h16" />
  </Icon>
);

export const CloseIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M6 6l12 12M18 6 6 18" />
  </Icon>
);

export const LeaveIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M14 4.5h3.5a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H14" />
    <path d="M10 8l-4 4 4 4M6 12h9" />
  </Icon>
);

export const LockIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <rect x="5" y="10.5" width="14" height="10" rx="2.5" />
    <path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5" />
  </Icon>
);

export const DownloadIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M12 4v11M7 10.5l5 5 5-5M5 20h14" />
  </Icon>
);

const TargetIcon = (): ReactElement => (
  <Icon>
    <circle cx="12" cy="12" r="8" />
    <circle cx="12" cy="12" r="4.5" />
    <circle cx="12" cy="12" r="1" fill="currentColor" />
  </Icon>
);

const ShieldIcon = (): ReactElement => (
  <Icon>
    <path d="M12 3.5 19 6v5.5c0 4.3-3 7.6-7 9-4-1.4-7-4.7-7-9V6z" />
    <path d="m9 12 2 2 4-4" />
  </Icon>
);

export const FileIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M13.5 3.5H7.5a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h9a2 2 0 0 0 2-2V8.5z" />
    <path d="M13.5 3.5v5h5" />
  </Icon>
);

const SlidersIcon = (): ReactElement => (
  <Icon>
    <path d="M5 7h9M18 7h1M5 17h1M10 17h9" />
    <circle cx="16" cy="7" r="2" />
    <circle cx="8" cy="17" r="2" />
  </Icon>
);

export const PlugIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M9 3.5v4M15 3.5v4M6.5 7.5h11V11a5.5 5.5 0 0 1-11 0z" />
    <path d="M12 16.5v4" />
  </Icon>
);

const PeopleIcon = (): ReactElement => (
  <Icon>
    <circle cx="9" cy="9" r="3.2" />
    <path d="M3.5 19c.8-3 3-4.5 5.5-4.5s4.7 1.5 5.5 4.5" />
    <path d="M15.5 6.2a3 3 0 0 1 0 5.6M17 14.8c1.8.5 3 1.9 3.5 4.2" />
  </Icon>
);

const KeyIcon = (): ReactElement => (
  <Icon>
    <circle cx="8" cy="15" r="4" />
    <path d="m11 12 8.5-8.5M16 7l2.5 2.5M14 9l2 2" />
  </Icon>
);

const ListIcon = (): ReactElement => (
  <Icon>
    <path d="M9 6.5h10.5M9 12h10.5M9 17.5h10.5" />
    <circle cx="5" cy="6.5" r=".6" fill="currentColor" />
    <circle cx="5" cy="12" r=".6" fill="currentColor" />
    <circle cx="5" cy="17.5" r=".6" fill="currentColor" />
  </Icon>
);

const GaugeIcon = (): ReactElement => (
  <Icon>
    <path d="M4.5 16a7.5 7.5 0 1 1 15 0" />
    <path d="m12 16 3.5-4.5" />
  </Icon>
);

const PersonIcon = (): ReactElement => (
  <Icon>
    <circle cx="12" cy="8.5" r="3.5" />
    <path d="M5 20c1.2-3.5 4-5 7-5s5.8 1.5 7 5" />
  </Icon>
);

/** Each surface's icon in the navigation. */
export const SURFACE_ICONS: Record<WebSurfaceId, () => ReactElement> = {
  "sign-in": () => <PersonIcon />,
  "work-review": () => <TargetIcon />,
  approvals: () => <ShieldIcon />,
  artifacts: () => <FileIcon />,
  policies: () => <SlidersIcon />,
  connectors: () => <PlugIcon />,
  users: () => <PeopleIcon />,
  capabilities: () => <KeyIcon />,
  "audit-and-privacy": () => <ListIcon />,
  usage: () => <GaugeIcon />,
};
