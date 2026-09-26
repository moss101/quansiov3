/**
 * Line icons for the desktop renderer (APP-003), drawn on a 24-unit grid with round joins.
 * Decorative only: every control that shows one carries its own accessible name, so each
 * icon is `aria-hidden` and never focusable.
 */

import type { ReactElement, ReactNode } from "react";

interface IconProps {
  readonly size?: number;
}

function Icon({ children, size = 16 }: { readonly children: ReactNode; readonly size?: number | undefined }): ReactElement {
  return (
    <svg
      className="icon"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.9}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {children}
    </svg>
  );
}

export const SearchIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <circle cx="11" cy="11" r="6.5" />
    <path d="m20 20-4.2-4.2" />
  </Icon>
);

export const PlusIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M12 5v14M5 12h14" />
  </Icon>
);

export const ArrowUpIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M12 19V5M6 11l6-6 6 6" />
  </Icon>
);

export const ArrowDownIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M12 5v14M6 13l6 6 6-6" />
  </Icon>
);

export const StopIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <rect x="7" y="7" width="10" height="10" rx="2" fill="currentColor" stroke="none" />
  </Icon>
);

export const PanelIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <rect x="3.5" y="4.5" width="17" height="15" rx="3" />
    <path d="M14.5 4.5v15" />
  </Icon>
);

export const PulseIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M3 12h4l2.5-6 5 12 2.5-6h4" />
  </Icon>
);

export const CloseIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M6 6l12 12M18 6 6 18" />
  </Icon>
);

export const ChevronRightIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="m9 6 6 6-6 6" />
  </Icon>
);

export const ChevronLeftIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="m15 6-6 6 6 6" />
  </Icon>
);

export const InfoIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <circle cx="12" cy="12" r="8.5" />
    <path d="M12 11v5M12 8h.01" />
  </Icon>
);

/** A ring with a gap: spun by the stylesheet while something is waiting. */
export const SpinnerIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M12 3.5a8.5 8.5 0 1 0 8.5 8.5" />
  </Icon>
);

export const GearIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <circle cx="12" cy="12" r="3" />
    <path d="M12 2.8v2.4M12 18.8v2.4M4.2 7.5l2.1 1.2M17.7 15.3l2.1 1.2M4.2 16.5l2.1-1.2M17.7 8.7l2.1-1.2" />
    <circle cx="12" cy="12" r="6.6" />
  </Icon>
);

export const LeaveIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M14 4.5h3.5a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H14" />
    <path d="M10 8l-4 4 4 4M6 12h9" />
  </Icon>
);

export const GridIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <rect x="4" y="4" width="6.5" height="6.5" rx="1.8" />
    <rect x="13.5" y="4" width="6.5" height="6.5" rx="1.8" />
    <rect x="4" y="13.5" width="6.5" height="6.5" rx="1.8" />
    <rect x="13.5" y="13.5" width="6.5" height="6.5" rx="1.8" />
  </Icon>
);

export const ChatIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M5 18.5V7a2.5 2.5 0 0 1 2.5-2.5h9A2.5 2.5 0 0 1 19 7v6.5a2.5 2.5 0 0 1-2.5 2.5H8.5z" />
  </Icon>
);

export const TargetIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <circle cx="12" cy="12" r="8" />
    <circle cx="12" cy="12" r="4.5" />
    <circle cx="12" cy="12" r="1" fill="currentColor" />
  </Icon>
);

export const PeopleIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <circle cx="9" cy="9" r="3.2" />
    <path d="M3.5 19c.8-3 3-4.5 5.5-4.5s4.7 1.5 5.5 4.5" />
    <path d="M15.5 6.2a3 3 0 0 1 0 5.6M17 14.8c1.8.5 3 1.9 3.5 4.2" />
  </Icon>
);

export const BranchIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <circle cx="6.5" cy="6" r="2" />
    <circle cx="6.5" cy="18" r="2" />
    <circle cx="17.5" cy="12" r="2" />
    <path d="M6.5 8v8M8.5 6h3a4 4 0 0 1 4 4v0M8.5 18h3a4 4 0 0 0 4-4v0" />
  </Icon>
);

export const GaugeIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M4.5 16a7.5 7.5 0 1 1 15 0" />
    <path d="m12 16 3.5-4.5" />
  </Icon>
);

export const ShieldIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M12 3.5 19 6v5.5c0 4.3-3 7.6-7 9-4-1.4-7-4.7-7-9V6z" />
    <path d="m9 12 2 2 4-4" />
  </Icon>
);

export const ListIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M9 6.5h10.5M9 12h10.5M9 17.5h10.5" />
    <circle cx="5" cy="6.5" r=".6" fill="currentColor" />
    <circle cx="5" cy="12" r=".6" fill="currentColor" />
    <circle cx="5" cy="17.5" r=".6" fill="currentColor" />
  </Icon>
);

export const GlobeIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <circle cx="12" cy="12" r="8.5" />
    <path d="M3.5 12h17M12 3.5c2.3 2.4 3.4 5.2 3.4 8.5s-1.1 6.1-3.4 8.5c-2.3-2.4-3.4-5.2-3.4-8.5S9.7 5.9 12 3.5" />
  </Icon>
);

export const FileIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M13.5 3.5H7.5a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h9a2 2 0 0 0 2-2V8.5z" />
    <path d="M13.5 3.5v5h5" />
  </Icon>
);

export const BookIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M5 5.5A2 2 0 0 1 7 3.5h12v14H7a2 2 0 0 0-2 2z" />
    <path d="M5 19.5a2 2 0 0 0 2 1h12v-3" />
  </Icon>
);

export const ThreadsIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M4 15V6.5A2.5 2.5 0 0 1 6.5 4h7A2.5 2.5 0 0 1 16 6.5V11a2.5 2.5 0 0 1-2.5 2.5H7z" />
    <path d="M19 10.5a1.5 1.5 0 0 1 1 1.4V20l-3-2.5h-6a2 2 0 0 1-2-2" />
  </Icon>
);

export const ClockIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <circle cx="12" cy="12" r="8.5" />
    <path d="M12 7.5V12l3 2" />
  </Icon>
);

export const PlugIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <path d="M9 3.5v4M15 3.5v4M6.5 7.5h11V11a5.5 5.5 0 0 1-11 0z" />
    <path d="M12 16.5v4" />
  </Icon>
);

export const KeyIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <circle cx="8" cy="15" r="4" />
    <path d="m11 12 8.5-8.5M16 7l2.5 2.5M14 9l2 2" />
  </Icon>
);

export const ChipIcon = ({ size }: IconProps): ReactElement => (
  <Icon size={size}>
    <rect x="6.5" y="6.5" width="11" height="11" rx="2" />
    <path d="M10 10h4v4h-4zM9.5 3.5v3M14.5 3.5v3M9.5 17.5v3M14.5 17.5v3M3.5 9.5h3M3.5 14.5h3M17.5 9.5h3M17.5 14.5h3" />
  </Icon>
);
