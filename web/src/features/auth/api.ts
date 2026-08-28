import { ApiError, request } from "../../shared/api/client";

export interface AuthIdentity {
  username: string;
  /** Permission grants are UI hints; every module API still enforces RBAC server-side. */
  permissions?: string[];
}

export const authApi = {
  authenticate: () => request<AuthIdentity>("/api/auth/me", { suppressAuthExpired: true }),
  login: async (username: string, password: string) => {
    try {
      return await request<AuthIdentity>("/api/auth/login", {
        method: "POST",
        body: JSON.stringify({ username, password }),
        suppressAuthExpired: true,
      });
    } catch (error) {
      if (error instanceof ApiError && error.status === 401) {
        throw new ApiError("账号或密码错误", error.code, 401);
      }
      throw error;
    }
  },
  logout: () => request<void>("/api/auth/logout", { method: "POST", expectedStatus: 204 }),
  changePassword: (current_password: string, new_password: string) => request<void>(
    "/api/auth/change-password",
    { method: "POST", body: JSON.stringify({ current_password, new_password }), expectedStatus: 204 },
  ),
};
