import axios from "axios";

const api = axios.create({
  baseURL: "/api/v1",
  timeout: 10000
});

const _monitoringApi = axios.create({
  baseURL: "/api/admin/monitoring",
  timeout: 10000
});

// Add auth token to requests
api.interceptors.request.use((config) => {
  const token = localStorage.getItem("admin_token");
  if (token) {
    config.headers.Authorization = `Bearer ${token}`;
  }
  return config;
});

// Handle auth errors
api.interceptors.response.use(
  (response) => response,
  (error) => {
    if (error.response?.status === 401) {
      localStorage.removeItem("admin_token");
      localStorage.removeItem("admin_user");
      window.location.href = "/login";
    }
    return Promise.reject(error);
  }
);

export const authApi = {
  login: (username, password) =>
    api.post("/auth/login", { username, password }),

  logout: () =>
    api.post("/auth/logout"),

  me: () =>
    api.get("/auth/me")
};

export const auditApi = {
  getLogs: (params) =>
    api.get("/audit-logs", { params })
};

export const securityApi = {
  getLogs: (params) =>
    api.get("/security-logs", { params })
};

export const gmApi = {
  broadcast: (data) =>
    api.post("/gm/broadcast", data),
  sendItem: (data) =>
    api.post("/gm/send-item", data),
  kickPlayer: (data) =>
    api.post("/gm/kick-player", data),
  banPlayer: (data) =>
    api.post("/gm/ban-player", data)
};

export const playerApi = {
  getPlayers: (params) =>
    api.get("/players", { params }),
  getPlayer: (playerId) =>
    api.get(`/players/${playerId}`),
  updatePlayerStatus: (playerId, status) =>
    api.put(`/players/${playerId}/status`, { status })
};

export const maintenanceApi = {
  getStatus: () =>
    api.get("/maintenance"),
  setStatus: (enabled, reason) =>
    api.post("/maintenance", { enabled, reason })
};

export const monitoringApi = {
  getServices: () =>
    _monitoringApi.get("/services"),
  getServiceMetrics: (name, window) =>
    _monitoringApi.get(`/services/${name}/metrics`, { params: { window } }),
  triggerArchive: () =>
    _monitoringApi.post("/archive")
};

export default api;
