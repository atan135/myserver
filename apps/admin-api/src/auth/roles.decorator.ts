import { SetMetadata } from "@nestjs/common";

export const ROLES_KEY = "roles";
export type AdminRole = "viewer" | "operator" | "admin";

export const Roles = (...roles: AdminRole[]) => SetMetadata(ROLES_KEY, roles);
