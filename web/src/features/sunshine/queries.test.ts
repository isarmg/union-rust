import { QueryClient } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";
import { sunshineApi as api } from "./api";
import { sunshineQueryKeys as queryKeys } from "./queryKeys";
import { removeSunshineHost, replaceSunshineHost, sunshineHostMutationKeys } from "./data";
import { querySunshineHosts } from "./queries";
import type { SunshineHostInfo, SunshineHostSaveRequest } from "./types";

function host(id: string, name = id): SunshineHostInfo {
  return {
    id,
    name,
    host: `${id}.example.test`,
    web_port: 47990,
    username: "admin",
    password_set: true,
    verify_tls: true,
    web_url: `https://${id}.example.test:47990`,
    probe_status: "complete",
    reachable: true,
    connected: true,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

afterEach(() => vi.restoreAllMocks());

describe("querySunshineHosts mutation barriers", () => {
  it("keeps a newly created host when an older GET resolves after CREATE", async () => {
    const queryClient = new QueryClient();
    const createResponse = deferred<SunshineHostInfo>();
    const staleGet = deferred<SunshineHostInfo[]>();
    vi.spyOn(api, "sunshineHosts").mockReturnValue(staleGet.promise);
    const saved = host("created", "Created host");
    const request: SunshineHostSaveRequest = {
      name: saved.name,
      host: saved.host,
      web_port: saved.web_port,
      username: saved.username,
      verify_tls: saved.verify_tls,
    };
    const mutation = queryClient.getMutationCache().build<
      SunshineHostInfo,
      Error,
      SunshineHostSaveRequest,
      unknown
    >(queryClient, {
      mutationKey: sunshineHostMutationKeys.create,
      mutationFn: () => createResponse.promise,
      onSuccess: (created) => queryClient.setQueryData<SunshineHostInfo[]>(
        queryKeys.sunshine.hosts,
        (current) => replaceSunshineHost(current ?? [], created),
      ),
    });

    const create = mutation.execute(request);
    const refresh = querySunshineHosts(queryClient);
    createResponse.resolve(saved);
    await create;
    staleGet.resolve([]);

    expect(await refresh).toEqual([saved]);
  });

  it("does not resurrect a deleted host when an older GET resolves after DELETE", async () => {
    const queryClient = new QueryClient();
    const original = host("deleted", "Deleted host");
    queryClient.setQueryData(queryKeys.sunshine.hosts, [original]);
    const deleteResponse = deferred<void>();
    const staleGet = deferred<SunshineHostInfo[]>();
    vi.spyOn(api, "sunshineHosts").mockReturnValue(staleGet.promise);
    const mutation = queryClient.getMutationCache().build<void, Error, string, unknown>(queryClient, {
      mutationKey: sunshineHostMutationKeys.delete,
      mutationFn: () => deleteResponse.promise,
      onSuccess: (_result, id) => queryClient.setQueryData<SunshineHostInfo[]>(
        queryKeys.sunshine.hosts,
        (current) => removeSunshineHost(current ?? [], id),
      ),
    });

    const deletion = mutation.execute(original.id);
    const refresh = querySunshineHosts(queryClient);
    deleteResponse.resolve();
    await deletion;
    staleGet.resolve([original]);

    expect(await refresh).toEqual([]);
  });
});
