import { createRouter, createWebHistory } from "vue-router";
import { ElMessage } from "element-plus";
import { useAuthStore } from "../stores/auth";
import { ADMIN_PERMISSIONS, MYFORGE_ENTRY_PERMISSIONS } from "../auth/permissions";

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
        ADMIN_PERMISSIONS.GM_BAN_PLAYER,
        ADMIN_PERMISSIONS.GM_CHARACTER_ELEMENTS_WRITE,
        ADMIN_PERMISSIONS.GM_CHARACTER_TITLES_WRITE,
        ADMIN_PERMISSIONS.GM_CHARACTER_DISCIPLINES_WRITE
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
  },
  {
    path: "/global-id",
    name: "GlobalId",
    component: () => import("../views/GlobalId.vue"),
    meta: { requiresAuth: true, permission: ADMIN_PERMISSIONS.ID_READ }
  },
  {
    path: "/myforge",
    name: "MyForge",
    component: () => import("../views/MyForge.vue"),
    meta: { requiresAuth: true, anyPermission: MYFORGE_ENTRY_PERMISSIONS }
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
    ElMessage.warning("当前账号无权限访问该页面");
    next({ name: "Dashboard" });
    return;
  }

  if (to.meta.anyPermission && !authStore.hasAnyPermission(to.meta.anyPermission)) {
    ElMessage.warning("当前账号无权限访问该页面");
    next({ name: "Dashboard" });
    return;
  }

  next();
});

export default router;
