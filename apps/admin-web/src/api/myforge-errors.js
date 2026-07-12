const TIMEOUT_CODES = new Set(["ECONNABORTED", "ETIMEDOUT"]);

export function normalizeMyforgeError(error, options = {}) {
  const status = error?.response?.status;
  const errorCode = error?.response?.data?.error;

  if (TIMEOUT_CODES.has(error?.code)) {
    return {
      title: "接口请求超时",
      description: "MyForge 接口响应超时，请稍后重试。"
    };
  }

  if (status === 403) {
    return {
      title: "无权限访问",
      description: "当前账号缺少所需的 MyForge 权限，请联系管理员。"
    };
  }

  if (errorCode === "MYFORGE_TASK_NOT_FOUND" || (status === 404 && options.taskDetailLookup)) {
    return {
      title: "任务不存在",
      description: options.notFoundMessage || "未找到该任务，请检查任务 ID。"
    };
  }

  if (errorCode === "MYFORGE_DISABLED") {
    return {
      title: "MyForge 未启用",
      description: "管理接口当前未启用 MyForge，请检查服务配置。"
    };
  }

  if (!error?.response) {
    return {
      title: "管理接口不可达",
      description: "无法连接 MyForge 管理接口，请检查网络或服务状态。"
    };
  }

  return {
    title: options.title || "MyForge 数据加载失败",
    description: options.fallbackMessage || "请求处理失败，请稍后重试。"
  };
}
