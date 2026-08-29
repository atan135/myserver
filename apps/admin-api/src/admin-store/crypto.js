import bcrypt from "bcrypt";
import crypto from "node:crypto";

import { SALT_ROUNDS } from "./constants.js";

export function hashPassword(password) {
  return bcrypt.hashSync(password, SALT_ROUNDS);
}

export function verifyPassword(password, hash) {
  return bcrypt.compareSync(password, hash);
}

export function hashToken(token) {
  return crypto.createHash("sha256").update(token).digest("hex");
}

export function generatePasswordSalt() {
  return crypto.randomBytes(16).toString("hex");
}
