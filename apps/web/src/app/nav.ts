import {
  Activity,
  Bell,
  Bug,
  Category,
  CalendarDays,
  ChartLine,
  Element3,
  Global,
  Hierarchy,
  Home,
  Brush,
  Danger,
  Data,
  DocumentText,
  Devices,
  Folder,
  Gauge,
  InfoCircle,
  Key,
  Lock,
  People,
  Profile,
  SecurityUser,
  Sms,
  Link,
  Siren,
  Setting2,
  Sitemap,
  type IconComponent,
} from "reicon-react";

export type NavLinkItem = {
  to: string;
  label: string;
  icon: IconComponent;
  end?: boolean;
  /** Who sees the link; the server still enforces the access. */
  requires?: keyof Access;
};

export type Access = { editor: boolean; owner: boolean; admin: boolean; root: boolean };

export function allowed(link: NavLinkItem, access: Access) {
  return !link.requires || access[link.requires];
}

export type NavGroup = {
  label?: string;
  links: NavLinkItem[];
};

export type Product = {
  id: string;
  label: string;
  description: string;
  icon: IconComponent;
  groups: NavGroup[];
};

export const dashboard: NavLinkItem = { to: "/", label: "Dashboard", icon: Home, end: true };

export const products: Product[] = [
  {
    id: "status",
    label: "Status",
    description: "Uptime monitors, agents, status pages, and incidents.",
    icon: Activity,
    groups: [
      {
        label: "Workspace",
        links: [
          { to: "/status", label: "Overview", icon: Category, end: true },
          { to: "/monitors", label: "Monitors", icon: Activity },
          { to: "/agents", label: "Agents", icon: Sitemap },
          { to: "/pages", label: "Status pages", icon: Global },
        ],
      },
      {
        label: "Operations",
        links: [
          { to: "/incidents", label: "Incidents", icon: Siren },
          { to: "/maintenance", label: "Maintenance", icon: CalendarDays },
          { to: "/notifications", label: "Notifications", icon: Bell, requires: "editor" },
        ],
      },
    ],
  },
  {
    id: "errors",
    label: "Errors",
    description: "Exceptions from your apps, grouped into issues.",
    icon: Bug,
    groups: [
      {
        links: [
          { to: "/errors", label: "Issues", icon: Bug },
          { to: "/errors/apps", label: "Apps", icon: Element3 },
        ],
      },
    ],
  },
  {
    id: "observability",
    label: "Observability",
    description: "Logs and traces from OpenTelemetry sources.",
    icon: DocumentText,
    groups: [
      {
        links: [
          { to: "/logs", label: "Logs", icon: DocumentText },
          { to: "/traces", label: "Traces", icon: Hierarchy },
        ],
      },
    ],
  },
  {
    id: "analytics",
    label: "Analytics",
    description: "Cookieless site analytics and short links.",
    icon: ChartLine,
    groups: [
      {
        links: [
          { to: "/analytics", label: "Sites", icon: ChartLine },
          { to: "/links", label: "Links", icon: Link },
        ],
      },
    ],
  },
];

export const manage: NavLinkItem[] = [{ to: "/settings", label: "Settings", icon: Setting2 }];

/** The workspace: shown in the sidebar instead of the main menu on its own pages. */
export const settings: Product = {
  id: "settings",
  label: "Settings",
  description: "This workspace.",
  icon: Setting2,
  groups: [
    {
      label: "Workspace",
      links: [
        { to: "/settings", label: "General", icon: Setting2, end: true },
        { to: "/settings/projects", label: "Projects", icon: Folder },
        { to: "/settings/members", label: "Members", icon: People },
      ],
    },
    {
      label: "Data",
      links: [{ to: "/settings/privacy", label: "Privacy requests", icon: Data, requires: "editor" }],
    },
    {
      label: "Developers",
      links: [{ to: "/settings/api", label: "API & MCP", icon: Key }],
    },
    {
      label: "Advanced",
      links: [{ to: "/settings/danger-zone", label: "Danger zone", icon: Danger, requires: "owner" }],
    },
  ],
};

/** The signed-in user, opened from the profile menu. */
export const account: Product = {
  id: "account",
  label: "Account settings",
  description: "Your account and this browser.",
  icon: Profile,
  groups: [
    {
      label: "Account",
      links: [
        { to: "/profile", label: "Profile", icon: Profile, end: true },
        { to: "/profile/security", label: "Security", icon: Lock },
        { to: "/profile/sessions", label: "Sessions", icon: Devices },
      ],
    },
    {
      label: "Preferences",
      links: [{ to: "/profile/appearance", label: "Appearance", icon: Brush }],
    },
  ],
};

/** The Owlee install, not the workspace: root and installation admins only. */
export const admin: Product = {
  id: "admin",
  label: "Admin",
  description: "This Owlee install.",
  icon: SecurityUser,
  groups: [
    {
      label: "Instance",
      links: [
        { to: "/admin", label: "General", icon: InfoCircle, end: true, requires: "admin" },
        { to: "/admin/users", label: "Users", icon: People, requires: "admin" },
      ],
    },
    {
      label: "Configuration",
      links: [
        { to: "/admin/email", label: "Email", icon: Sms, requires: "root" },
        { to: "/admin/quotas", label: "Ingest quotas", icon: Gauge, requires: "root" },
      ],
    },
  ],
};
