export const platformQueryKeys = {
  modules: ["platform-modules"] as const,
  configuration: (moduleId: string) => ["platform-module-configuration", moduleId] as const,
} as const;
