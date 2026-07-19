import { defineStore } from "pinia";
import { authApi } from "../api";
import {
  ADMIN_PERMISSIONS as P,
  effectivePermissions,
  hasAnyPermission,
  hasPermission
} from "../auth/permissions";

export const useAuthStore = defineStore("auth", {
  state: () => ({
    token: localStorage.getItem("admin_token") || null,
    user: JSON.parse(localStorage.getItem("admin_user") || "null"),
    loading: false,
    capabilitiesLoaded: false
  }),

  getters: {
    isAuthenticated: (state) => !!state.token,
    role: (state) => state.user?.role || null,
    username: (state) => state.user?.username || null,
    displayName: (state) => state.user?.displayName || null,
    permissions: (state) => effectivePermissions(state.user),
    isAdmin: (state) => effectivePermissions(state.user).length > 0,
    isOperator: (state) => hasAnyPermission(state.user, [
      P.PLAYERS_STATUS_UPDATE,
      P.GM_BROADCAST,
      P.GM_SEND_ITEM,
      P.GM_KICK_PLAYER,
      P.GM_CHARACTER_ELEMENTS_WRITE,
      P.GM_CHARACTER_TITLES_WRITE,
      P.GM_CHARACTER_DISCIPLINES_WRITE
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
        this.capabilitiesLoaded = true;
        localStorage.setItem("admin_token", data.accessToken);
        localStorage.setItem("admin_user", JSON.stringify(data.admin));
        return data;
      } finally {
        this.loading = false;
      }
    },

    async refreshCapabilities() {
      if (!this.token) return null;
      const { data } = await authApi.me();
      this.user = data.admin;
      this.capabilitiesLoaded = true;
      localStorage.setItem("admin_user", JSON.stringify(data.admin));
      return data.admin;
    },

    async logout() {
      try {
        await authApi.logout();
      } catch {
        // Ignore errors on logout
      } finally {
        this.token = null;
        this.user = null;
        this.capabilitiesLoaded = false;
        localStorage.removeItem("admin_token");
        localStorage.removeItem("admin_user");
      }
    }
  }
});
