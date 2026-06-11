import { createRouter, createWebHistory } from "vue-router";
import { useAuthStore } from "../stores/auth";
import { ADMIN_PERMISSIONS } from "../auth/permissions";

const routes = [
  {
    path: "/login",
    name: "Login",
    component: () => import("../views/Login.vue"),
    meta: { requiresAuth: false }
  },
  {
    path: "/",
    name: "Dashboard",
    component: () => import("../views/Dashboard.vue"),
    meta: { requiresAuth: true }
  },
  {
    path: "/audit-logs",
    name: "AuditLogs",
    component: () => import("../views/AuditLogs.vue"),
    meta: { requiresAuth: true, permission: ADMIN_PERMISSIONS.AUDIT_READ }
  },
  {
    path: "/security-logs",
    name: "SecurityLogs",
    component: () => import("../views/SecurityLogs.vue"),
    meta: { requiresAuth: true, permission: ADMIN_PERMISSIONS.SECURITY_READ }
  },
  {
    path: "/gm",
    name: "GM",
    component: () => import("../views/GM.vue"),
    meta: {
      requiresAuth: true,
      anyPermission: [
        ADMIN_PERMISSIONS.GM_BROADCAST,
        ADMIN_PERMISSIONS.GM_SEND_ITEM,
        ADMIN_PERMISSIONS.GM_KICK_PLAYER,
        ADMIN_PERMISSIONS.GM_BAN_PLAYER
      ]
    }
  },
  {
    path: "/players",
    name: "Players",
    component: () => import("../views/Players.vue"),
    meta: { requiresAuth: true, permission: ADMIN_PERMISSIONS.PLAYERS_READ }
  },
  {
    path: "/monitoring",
    name: "Monitoring",
    component: () => import("../views/admin/Monitoring.vue"),
    meta: { requiresAuth: true, permission: ADMIN_PERMISSIONS.MONITORING_READ }
  },
  {
    path: "/monitoring/:service",
    name: "MonitoringDetail",
    component: () => import("../views/admin/MonitoringDetail.vue"),
    meta: { requiresAuth: true, permission: ADMIN_PERMISSIONS.MONITORING_READ }
  }
];

const router = createRouter({
  history: createWebHistory(),
  routes
});

router.beforeEach((to, from, next) => {
  const authStore = useAuthStore();

  if (to.meta.requiresAuth && !authStore.isAuthenticated) {
    next({ name: "Login", query: { redirect: to.fullPath } });
    return;
  }

  if (to.meta.permission && !authStore.hasPermission(to.meta.permission)) {
    next({ name: "Dashboard" });
    return;
  }

  if (to.meta.anyPermission && !authStore.hasAnyPermission(to.meta.anyPermission)) {
    next({ name: "Dashboard" });
    return;
  }

  next();
});

export default router;
