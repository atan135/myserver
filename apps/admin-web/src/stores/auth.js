import { defineStore } from "pinia";
import { authApi } from "../api";

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
    isAdmin: (state) => state.user?.role === "admin",
    isOperator: (state) => ["admin", "operator"].includes(state.user?.role)
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
