import { useQuery } from "@tanstack/react-query";
import { Navigate, Outlet, Route, Routes, useLocation } from "react-router";
import { Shell } from "@/app/Shell";
import { ApiError, api, type Project } from "@/lib/api";
import { useMe } from "@/lib/me";
import { savedProject, useSelectionVersion, WorkspaceContext, workspaceFor } from "@/lib/workspace";
import {
  ForgotPasswordPage,
  LoginPage,
  RegisterCompletePage,
  RegisterPage,
  ResetPasswordPage,
  SetupPage,
} from "@/pages/auth";
import { AgentsPage } from "@/pages/agents";
import { ApiTokensPage } from "@/pages/api-tokens";
import { AnalyticsPage, VisitorPage } from "@/pages/analytics";
import { DashboardPage } from "@/pages/dashboard";
import { HomePage } from "@/pages/home";
import { LinkDetailPage, LinksPage } from "@/pages/links";
import { LogSourcesPage, LogsPage } from "@/pages/logs";
import { LogSourceDetailPage } from "@/pages/telemetry-sources";
import { ErrorAppDetailPage, ErrorAppsPage } from "@/pages/error-apps";
import { IncidentDetailPage, IncidentsPage } from "@/pages/incidents";
import { IssueDetailPage } from "@/pages/issue-detail";
import { IssuesPage } from "@/pages/issues";
import { ReplayPage } from "@/pages/replays";
import { MaintenancePage } from "@/pages/maintenance";
import { MonitorDetailPage, MonitorsPage } from "@/pages/monitors";
import { NotificationsPage } from "@/pages/notifications";
import { PageEditor, PagesPage } from "@/pages/pages";
import { PublicStatusPage } from "@/pages/public";
import { SharePage } from "@/pages/share";
import { ProfilePage, SecurityPage, SessionsPage } from "@/pages/profile";
import { AppearancePage } from "@/pages/appearance";
import { InvitePage } from "@/pages/invite";
import { OnboardingPage } from "@/pages/onboarding";
import { AdminEmailPage, AdminGeneralPage, AdminQuotasPage, AdminUsersPage } from "@/pages/admin";
import {
  WorkspaceDangerZonePage,
  WorkspaceGeneralPage,
  WorkspaceMembersPage,
  WorkspacePrivacyPage,
  WorkspaceProjectsPage,
} from "@/pages/workspace-settings";

function RequireAuth() {
  // A workspace switch clears the cache; re-rendering here refetches `me` and remounts the app.
  useSelectionVersion();
  const location = useLocation();
  const me = useMe();
  if (me.isLoading) {
    return <div className="grid min-h-svh place-items-center text-sm text-muted-foreground">Opening Owlee…</div>;
  }
  if (me.error instanceof ApiError && me.error.status === 401) {
    return <Navigate to="/login" replace state={{ from: `${location.pathname}${location.search}` }} />;
  }
  if (me.error) {
    return <div className="grid min-h-svh place-items-center px-6 text-sm text-destructive">{me.error.message}</div>;
  }
  return <Outlet />;
}

/** Picks the current workspace and project for everything below it. Users without a workspace go to onboarding. */
function RequireWorkspace() {
  useSelectionVersion();
  const me = useMe();
  const workspace = workspaceFor(me.data);
  const projects = useQuery({
    queryKey: ["projects", workspace?.id],
    queryFn: () => api<Project[]>("/api/projects"),
    enabled: workspace != null,
  });
  if (!me.data) return null;
  if (!workspace) return <Navigate to="/onboarding" replace />;
  const saved = savedProject(workspace.id);
  // A deleted project falls back to "all" once the list proves it is gone.
  const project = saved === "all" || !projects.data || projects.data.some((item) => item.id === saved) ? saved : "all";
  return (
    <WorkspaceContext.Provider value={{ workspace, projects: projects.data ?? [], project }}>
      <Outlet />
    </WorkspaceContext.Provider>
  );
}

export function App() {
  return (
    <Routes>
      <Route path="/login" element={<LoginPage />} />
      <Route path="/register" element={<RegisterPage />} />
      <Route path="/register/complete" element={<RegisterCompletePage />} />
      <Route path="/forgot-password" element={<ForgotPasswordPage />} />
      <Route path="/reset-password" element={<ResetPasswordPage />} />
      <Route path="/setup" element={<SetupPage />} />
      <Route path="/status/:slug" element={<PublicStatusPage />} />
      <Route path="/share/:kind/:slug" element={<SharePage />} />
      <Route path="/invite/:token" element={<InvitePage />} />
      <Route element={<RequireAuth />}>
        <Route path="onboarding" element={<OnboardingPage />} />
        <Route element={<RequireWorkspace />}>
        <Route element={<Shell />}>
          <Route index element={<HomePage />} />
          <Route path="status" element={<DashboardPage />} />
          <Route path="monitors" element={<MonitorsPage />} />
          <Route path="monitors/:id" element={<MonitorDetailPage />} />
          <Route path="pages" element={<PagesPage />} />
          <Route path="pages/:id" element={<PageEditor />} />
          <Route path="incidents" element={<IncidentsPage />} />
          <Route path="incidents/:id" element={<IncidentDetailPage />} />
          <Route path="maintenance" element={<MaintenancePage />} />
          <Route path="notifications" element={<NotificationsPage />} />
          <Route path="agents" element={<AgentsPage />} />
          <Route path="analytics" element={<AnalyticsPage />} />
          <Route path="analytics/:siteId" element={<AnalyticsPage />} />
          <Route path="analytics/:siteId/visitors/:visitorId" element={<VisitorPage />} />
          <Route path="links" element={<LinksPage />} />
          <Route path="links/:linkId" element={<LinkDetailPage />} />
          <Route path="errors" element={<IssuesPage />} />
          <Route path="errors/:id" element={<IssueDetailPage />} />
          <Route path="errors/:id/events/:eventId" element={<IssueDetailPage />} />
          <Route path="errors/apps" element={<ErrorAppsPage />} />
          <Route path="errors/apps/:id" element={<ErrorAppDetailPage />} />
          <Route path="errors/apps/:id/replays/:replayId" element={<ReplayPage />} />
          <Route path="logs" element={<LogsPage />} />
          <Route path="logs/sources" element={<LogSourcesPage />} />
          <Route path="logs/sources/:id" element={<LogSourceDetailPage />} />
          <Route path="profile" element={<ProfilePage />} />
          <Route path="profile/security" element={<SecurityPage />} />
          <Route path="profile/sessions" element={<SessionsPage />} />
          <Route path="profile/appearance" element={<AppearancePage />} />
          <Route path="settings" element={<WorkspaceGeneralPage />} />
          <Route path="settings/projects" element={<WorkspaceProjectsPage />} />
          <Route path="settings/members" element={<WorkspaceMembersPage />} />
          <Route path="settings/privacy" element={<WorkspacePrivacyPage />} />
          <Route path="settings/api" element={<ApiTokensPage />} />
          <Route path="settings/danger-zone" element={<WorkspaceDangerZonePage />} />
          <Route path="admin" element={<AdminGeneralPage />} />
          <Route path="admin/users" element={<AdminUsersPage />} />
          <Route path="admin/email" element={<AdminEmailPage />} />
          <Route path="admin/quotas" element={<AdminQuotasPage />} />
        </Route>
        </Route>
      </Route>
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  );
}
