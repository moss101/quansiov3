/**
 * Line icons for the desktop shell (APP-003). Decorative only: every control that shows one
 * carries its own accessible name, so each icon is `aria-hidden`.
 */

import type { ReactElement, ReactNode } from "react";

function Icon({ children, size = 18 }: { readonly children: ReactNode; readonly size?: number }): ReactElement {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {children}
    </svg>
  );
}

export const SearchIcon = (): ReactElement => (
  <Icon>
    <circle cx="11" cy="11" r="6.5" />
    <path d="m20 20-4.2-4.2" />
  </Icon>
);

export const PlusIcon = (): ReactElement => (
  <Icon>
    <path d="M12 5v14M5 12h14" />
  </Icon>
);

export const ArrowUpIcon = (): ReactElement => (
  <Icon>
    <path d="M12 19V5M6 11l6-6 6 6" />
  </Icon>
);

export const StopIcon = (): ReactElement => (
  <Icon>
    <rect x="7" y="7" width="10" height="10" rx="1.5" fill="currentColor" stroke="none" />
  </Icon>
);

export const PanelIcon = (): ReactElement => (
  <Icon>
    <rect x="3.5" y="4.5" width="17" height="15" rx="2.5" />
    <path d="M14.5 4.5v15" />
  </Icon>
);

export const PulseIcon = (): ReactElement => (
  <Icon>
    <path d="M3 12h4l2.5-6 5 12 2.5-6h4" />
  </Icon>
);

export const PersonIcon = (): ReactElement => (
  <Icon>
    <circle cx="12" cy="8.5" r="3.5" />
    <path d="M5 20c1.2-3.5 4-5 7-5s5.8 1.5 7 5" />
  </Icon>
);

export const CloseIcon = (): ReactElement => (
  <Icon size={16}>
    <path d="M6 6l12 12M18 6 6 18" />
  </Icon>
);
