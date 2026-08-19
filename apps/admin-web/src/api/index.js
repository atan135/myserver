import axios from "axios";
import { createAdminRequestId } from "../operations/high-risk.js";

const api = axios.create({
  baseURL: "/api/v1",
  timeout: 10000
});

function highRiskRequestBody(data = {}) {
  const body = data && typeof data === "object" && !Array.isArray(data) ? data : {};
  return {
    ...body,
    requestId: typeof body.requestId === "string" && body.requestId.trim()
      ? body.requestId.trim()
      : createAdminRequestId()
  };
}

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

export const assetApi = {
  getLedger: (params) => api.get("/assets/ledger", { params })
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
    api.post("/gm/ban-player", data),
  setCharacterElements: (characterId, data) =>
    api.post(`/gm/characters/${characterId}/elements`, data),
  applyCharacterTitle: (characterId, data) =>
    api.post(`/gm/characters/${characterId}/titles`, data),
  setCharacterDiscipline: (characterId, data) =>
    api.post(`/gm/characters/${characterId}/disciplines`, data),
  runCharacterUnlockCheck: (characterId, data) =>
    api.post(`/gm/characters/${characterId}/unlock-check`, data)
};

export const playerApi = {
  getPlayers: (params) =>
    api.get("/players", { params }),
  getPlayer: (playerId) =>
    api.get(`/players/${playerId}`),
  getPlayerCharacters: (playerId, params) =>
    api.get(`/players/${playerId}/characters`, { params }),
  getCharacterProfile: (characterId, params) =>
    api.get(`/players/characters/${characterId}/profile`, { params }),
  getCharacterTitles: (characterId, params) =>
    api.get(`/players/characters/${characterId}/titles`, { params }),
  updatePlayerStatus: (playerId, data) =>
    api.put(`/players/${playerId}/status`, data)
};

export const maintenanceApi = {
  getStatus: () =>
    api.get("/maintenance"),
  setStatus: (data) =>
    api.post("/maintenance", data)
};

export const monitoringApi = {
  getServices: (config = {}) =>
    api.get("/services", { ...config, baseURL: "/api/admin/monitoring" }),
  getServiceMetrics: (name, window, config = {}) =>
    api.get(`/services/${name}/metrics`, { ...config, baseURL: "/api/admin/monitoring", params: { ...config.params, window } }),
  getRolloutDrain: (config = {}) =>
    api.get("/rollout-drain", { ...config, baseURL: "/api/admin/monitoring" }),
  getRegistry: (config = {}) =>
    api.get("/registry", { ...config, baseURL: "/api/admin/monitoring" }),
  triggerArchive: () =>
    api.post("/archive", undefined, { baseURL: "/api/admin/monitoring" })
};

// These clients stay on the authenticated admin-api boundary. They never accept
// or expose game-server endpoints, assertion tokens, or downstream credentials.
export const rolloutApi = {
  setDrain: (instanceId, data = {}, config = {}) =>
    api.post(
      `/rollouts/game-server/${encodeURIComponent(String(instanceId))}/drain`,
      highRiskRequestBody(data),
      config
    )
};

export const adminOperationApi = {
  decideApproval: (requestId, data = {}, config = {}) => {
    const normalizedRequestId = String(requestId ?? "").trim();
    return api.post(
      `/admin-operations/${encodeURIComponent(normalizedRequestId)}/approval`,
      { ...data, requestId: normalizedRequestId },
      config
    );
  },
  approve: (requestId, evidenceSummary = {}, config = {}) =>
    adminOperationApi.decideApproval(requestId, {
      status: "approved",
      evidenceSummary
    }, config),
  reject: (requestId, rejectionReason, evidenceSummary = {}, config = {}) =>
    adminOperationApi.decideApproval(requestId, {
      status: "rejected",
      evidenceSummary,
      rejectionReason
    }, config)
};

export { highRiskRequestBody };

export const globalIdApi = {
  decode: (id) =>
    api.get("/global-id/decode", { params: { id } }),
  getOrigins: (params) =>
    api.get("/global-id/origins", { params }),
  getWorlds: (params) =>
    api.get("/global-id/worlds", { params }),
  getMergeEvents: (params) =>
    api.get("/global-id/merge-events", { params })
};

export const myforgeApi = {
  getAgents: (params) =>
    api.get("/myforge/agents", { params }),
  getTasks: (params) =>
    api.get("/myforge/tasks", { params }),
  getTask: (requestId) =>
    api.get(`/myforge/tasks/${encodeURIComponent(requestId)}`),
  createFangyuanTask: (data) =>
    api.post("/myforge/tasks/fangyuan-blueprint", data),
  cancelTask: (requestId, data = {}) =>
    api.post(`/myforge/tasks/${encodeURIComponent(requestId)}/cancel`, data)
};

export default api;
