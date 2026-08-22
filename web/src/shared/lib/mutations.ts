import type { QueryClient } from "@tanstack/react-query";

/** Remove completed mutation records that may otherwise remain cached for GC. */
export function removeMutationFromCache(
  queryClient: QueryClient,
  mutationKey: readonly unknown[],
  variables?: unknown,
) {
  const mutationCache = queryClient.getMutationCache();
  for (const mutation of mutationCache.findAll({ mutationKey, exact: true })) {
    if (variables !== undefined && mutation.state.variables !== variables) continue;
    mutationCache.remove(mutation);
  }
}
