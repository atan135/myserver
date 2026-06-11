import { defineStore } from "pinia";
import { authApi } from "../api";
import {
  ADMIN_PERMISSIONS as P,
  hasAnyPermission,
  hasPermission,
  permissionsForRole
} from "../auth/permissions";

export const useAuthStore = defineStore("auth", {
  state: () => ({
    token: localStorage.getItem("admin_token") || null,
    user: JSON.parse(localStorage.getItem("admin_user") || "null"),
    loading: false
  }),

  getters: {
    isAuthenticated: (state) => !!state.token,
    role: (state) => state.user?.role || null,
    username: (state) => state.user?.username || null,
    displayName: (state) => state.user?.displayName || null,
    permissions: (state) => permissionsForRole(state.user?.role),
    isAdmin: (state) => ["admin", "super_admin"].includes(state.user?.role),
    isOperator: (state) => hasAnyPermission(state.user, [
      P.PLAYERS_STATUS_UPDATE,
      P.GM_BROADCAST,
      P.GM_SEND_ITEM,
      P.GM_KICK_PLAYER
    ]),
    hasPermission: (state) => (permission) => hasPermission(state.user, permission),
    hasAnyPermission: (state) => (permissions) => hasAnyPermission(state.user, permissions)
  },

  actions: {
    async login(username, password) {
      this.loading = true;
      try {
        const { data } = await authApi.login(username, password);
        this.token = data.accessToken;
        this.user = data.admin;
        localStorage.setItem("admin_token", data.accessToken);
        localStorage.setItem("admin_user", JSON.stringify(data.admin));
        return data;
      } finally {
        this.loading = false;
      }
    },

    async logout() {
      try {
        await authApi.logout();
      } catch {
        // Ignore errors on logout
      } finally {
        this.token = null;
        this.user = null;
        localStorage.removeItem("admin_token");
        localStorage.removeItem("admin_user");
      }
    }
  }
});
