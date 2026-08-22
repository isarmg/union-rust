// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { api } from "../api";
import { queryKeys } from "../query-keys";
import { querySunshineHosts } from "../sunshine-host-query";
import type { SunshineHostInfo } from "../types";
import { appDraft, InlineHostField, SunshineView } from "./SunshineView";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function host(id: string, name: string): SunshineHostInfo {
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
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

describe("Sunshine inline password editing", () => {
  it("treats an untouched empty password blur as cancel", async () => {
    const save = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(
      <InlineHostField
        label="密码"
        value=""
        displayValue="已设置"
        inputType="password"
        validate={() => null}
        normalize={(value) => value}
        cancelEmpty
        onSave={save}
      />,
    );

    await user.click(screen.getByRole("button", { name: /修改密码/ }));
    expect(screen.getByLabelText("密码")).toBeTruthy();
    await user.tab();
    expect(save).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /修改密码/ })).toBeTruthy();
  });

  it("preserves leading and trailing password spaces", async () => {
    const save = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(
      <InlineHostField
        label="密码"
        value=""
        displayValue="已设置"
        inputType="password"
        validate={() => null}
        normalize={(value) => value}
        cancelEmpty
        onSave={save}
      />,
    );

    await user.click(screen.getByRole("button", { name: /修改密码/ }));
    await user.type(screen.getByLabelText("密码"), "  secret  ");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(save).toHaveBeenCalledWith("  secret  "));
  });
});

describe("Sunshine application editing", () => {
  it("keeps unknown and advanced fields in the editable draft", () => {
    const draft = appDraft({
      index: 2,
      name: "Game",
      cmd: "game.exe",
      "image-path": "cover.png",
      "prep-cmd": [{ do: "prepare" }],
      elevated: true,
      vendor_extension: { enabled: true },
    });

    expect(draft).toMatchObject({
      index: 2,
      name: "Game",
      "image-path": "cover.png",
      "prep-cmd": [{ do: "prepare" }],
      elevated: true,
      vendor_extension: { enabled: true },
    });
  });

  it("keeps the current Sunshine hyphenated application fields", () => {
    const draft = appDraft({
      index: 4,
      name: "Game",
      "image-path": "cover.png",
      "working-dir": "C:/workdir",
      "auto-detach": false,
      "wait-all": false,
      "exit-timeout": 29,
      "prep-cmd": [{ do: "prepare" }],
      "exclude-global-prep-cmd": false,
    });

    expect(draft).toMatchObject({
      "image-path": "cover.png",
      "working-dir": "C:/workdir",
      "auto-detach": false,
      "wait-all": false,
      "exit-timeout": 29,
      "prep-cmd": [{ do: "prepare" }],
      "exclude-global-prep-cmd": false,
    });
  });
});

describe("Sunshine host panel state", () => {
  it("remounts the panel when selecting another host", async () => {
    const hosts = [host("one", "Host one"), host("two", "Host two")];
    vi.spyOn(api, "sunshineHosts").mockResolvedValue(hosts);
    vi.spyOn(api, "sunshineApps").mockResolvedValue({ apps: [] });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    queryClient.setQueryData(queryKeys.sunshine.hosts, hosts);
    const user = userEvent.setup();
    render(
      <QueryClientProvider client={queryClient}>
        <SunshineView />
      </QueryClientProvider>,
    );

    await user.click(within(screen.getByRole("article", { name: /Host one/ })).getByRole("button", { name: "管理" }));
    await user.click(await screen.findByRole("button", { name: "新建" }));
    expect(screen.getByText("新建应用")).toBeTruthy();

    await user.click(within(screen.getByRole("article", { name: /Host two/ })).getByRole("button", { name: "管理" }));
    await waitFor(() => expect(screen.queryByText("新建应用")).toBeNull());
  });

  it("does not overwrite an in-progress config draft when cached config refreshes", async () => {
    const hosts = [host("one", "Host one")];
    vi.spyOn(api, "sunshineHosts").mockResolvedValue(hosts);
    vi.spyOn(api, "sunshineApps").mockResolvedValue({ apps: [] });
    vi.spyOn(api, "sunshineConfig").mockResolvedValue({ mode: "initial" });
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: Infinity } },
    });
    queryClient.setQueryData(queryKeys.sunshine.hosts, hosts);
    queryClient.setQueryData(queryKeys.sunshine.config("one"), { mode: "initial" });
    const user = userEvent.setup();
    render(
      <QueryClientProvider client={queryClient}>
        <SunshineView />
      </QueryClientProvider>,
    );

    await user.click(within(screen.getByRole("article", { name: /Host one/ })).getByRole("button", { name: "管理" }));
    await user.click(screen.getByRole("tab", { name: "配置" }));
    await user.click(screen.getByRole("button", { name: "编辑 JSON" }));
    const editor = screen.getByLabelText(/完整 JSON 配置/) as HTMLTextAreaElement;
    fireEvent.change(editor, { target: { value: "{\n  \"mode\": \"local draft\"\n}" } });

    act(() => queryClient.setQueryData(queryKeys.sunshine.config("one"), { mode: "remote refresh" }));
    expect(editor.value).toContain("local draft");
    expect(editor.value).not.toContain("remote refresh");
  });
});

describe("Sunshine host PATCH concurrency", () => {
  it("keeps a PATCH response when an older GET resolves afterward", async () => {
    const original = host("one", "Original name");
    const saved = { ...original, name: "Renamed host" };
    const patchResponse = deferred<SunshineHostInfo>();
    const staleGet = deferred<SunshineHostInfo[]>();
    vi.spyOn(api, "sunshineUpdateHost").mockReturnValue(patchResponse.promise);
    vi.spyOn(api, "sunshineHosts").mockReturnValue(staleGet.promise);
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: Infinity } },
    });
    queryClient.setQueryData(queryKeys.sunshine.hosts, [original]);
    const user = userEvent.setup();
    render(
      <QueryClientProvider client={queryClient}>
        <SunshineView />
      </QueryClientProvider>,
    );

    const card = screen.getByRole("article", { name: /Original name/ });
    await user.click(within(card).getByRole("button", { name: /修改名称/ }));
    const nameInput = within(card).getByLabelText("名称");
    await user.clear(nameInput);
    await user.type(nameInput, "Renamed host");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(api.sunshineUpdateHost).toHaveBeenCalledWith("one", { name: "Renamed host" }));

    await queryClient.invalidateQueries({ queryKey: queryKeys.sunshine.hosts, exact: true, refetchType: "none" });
    const refresh = queryClient.fetchQuery({
      queryKey: queryKeys.sunshine.hosts,
      queryFn: () => querySunshineHosts(queryClient),
      staleTime: 0,
    });
    await waitFor(() => expect(api.sunshineHosts).toHaveBeenCalledTimes(1));

    await act(async () => {
      patchResponse.resolve(saved);
      await patchResponse.promise;
    });
    await waitFor(() => expect(
      queryClient.getQueryData<SunshineHostInfo[]>(queryKeys.sunshine.hosts)?.[0]?.name,
    ).toBe("Renamed host"));

    await act(async () => {
      staleGet.resolve([original]);
      await refresh;
    });
    expect(queryClient.getQueryData(queryKeys.sunshine.hosts)).toEqual([saved]);
    expect(screen.getByRole("article", { name: /Renamed host/ })).toBeTruthy();
  });

  it("allows different hosts to update concurrently while disabling only the matching host", async () => {
    const hosts = [host("one", "Host one"), host("two", "Host two")];
    const firstPatch = deferred<SunshineHostInfo>();
    const secondPatch = deferred<SunshineHostInfo>();
    vi.spyOn(api, "sunshineUpdateHost").mockImplementation((id) => (
      id === "one" ? firstPatch.promise : secondPatch.promise
    ));
    vi.spyOn(api, "sunshineHosts").mockResolvedValue([
      { ...hosts[0], name: "First renamed" },
      { ...hosts[1], name: "Second renamed" },
    ]);
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: Infinity } },
    });
    queryClient.setQueryData(queryKeys.sunshine.hosts, hosts);
    const user = userEvent.setup();
    render(
      <QueryClientProvider client={queryClient}>
        <SunshineView />
      </QueryClientProvider>,
    );

    const firstCard = screen.getByRole("article", { name: /Host one/ });
    await user.click(within(firstCard).getByRole("button", { name: /修改名称/ }));
    await user.clear(within(firstCard).getByLabelText("名称"));
    await user.type(within(firstCard).getByLabelText("名称"), "First renamed");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(api.sunshineUpdateHost).toHaveBeenCalledTimes(1));

    const secondCard = screen.getByRole("article", { name: /Host two/ });
    const secondEdit = within(secondCard).getByRole("button", { name: /修改名称/ }) as HTMLButtonElement;
    expect(secondEdit.disabled).toBe(false);
    await user.click(secondEdit);
    await user.clear(within(secondCard).getByLabelText("名称"));
    await user.type(within(secondCard).getByLabelText("名称"), "Second renamed");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(api.sunshineUpdateHost).toHaveBeenCalledTimes(2));

    await act(async () => {
      firstPatch.resolve({ ...hosts[0], name: "First renamed" });
      secondPatch.resolve({ ...hosts[1], name: "Second renamed" });
      await Promise.all([firstPatch.promise, secondPatch.promise]);
    });
  });
});
