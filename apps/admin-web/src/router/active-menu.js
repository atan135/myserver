export function resolveActiveMenu(path = "") {
  const value = String(path);
  const prefixes = ["/monitoring", "/rollout-drain", "/operation-approvals", "/global-id", "/myforge", "/activities"];
  return prefixes.find((prefix) => value === prefix || value.startsWith(`${prefix}/`)) || value;
}
