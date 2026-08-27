import { request } from "../../shared/api/client";
import type { PlatformModule } from "./types";

export const platformApi = {
  modules: () => request<PlatformModule[]>("/api/platform/modules"),
};
