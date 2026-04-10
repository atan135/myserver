import { createRouter, createWebHistory } from "vue-router";
import { useAuthStore } from "../stores/auth";

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
    meta: { requiresAuth: true, roles: ["admin", "operator", "viewer"] }
  },
  {
    path: "/security-logs",
    name: "SecurityLogs",
    component: () => import("../views/SecurityLogs.vue"),
    meta: { requiresAuth: true, roles: ["admin", "operator", "viewer"] }
  },
  {
    path: "/gm",
    name: "GM",
    component: () => import("../views/GM.vue"),
    meta: { requiresAuth: true, roles: ["admin", "operator"] }
  },
  {
    path: "/players",
    name: "Players",
    component: () => import("../views/Players.vue"),
    meta: { requiresAuth: true, roles: ["admin", "operator", "viewer"] }
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

  if (to.meta.roles && !to.meta.roles.includes(authStore.role)) {
    next({ name: "Dashboard" });
    return;
  }

  next();
});

export default router;
