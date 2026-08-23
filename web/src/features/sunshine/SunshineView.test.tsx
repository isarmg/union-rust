// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { sunshineApi as api } from "./api";
import { sunshineQueryKeys as queryKeys } from "./queryKeys";
import { querySunshineHosts } from "./queries";
import type { SunshineHostInfo } from "./types";
import { appDraft, InlineHostField, managementPanelLayout, SunshineView } from "./SunshineView";
import { HostCard } from "./components/HostCard";

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

function mutationCacheSnapshot(queryClient: QueryClient) {
  return JSON.stringify(queryClient.getMutationCache().getAll().map((mutation) => ({
    mutationKey: mutation.options.mutationKey,
    state: mutation.state,
  })));
}

describe("Sunshine empty state", () => {
  it("does not repeat the sidebar plus-button instruction", () => {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: Infinity } },
    });
    queryClient.setQueryData(queryKeys.sunshine.hosts, []);
    render(
      <QueryClientProvider client={queryClient}>
        <SunshineView />
      </QueryClientProvider>,
    );

    expect(screen.queryByText("暂无主机，点击 + 新建")).toBeNull();
    expect(screen.getByText("实例")).toBeTruthy();
  });

  it("honors an add trigger already present on first mount", async () => {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: Infinity } },
    });
    queryClient.setQueryData(queryKeys.sunshine.hosts, []);
    vi.spyOn(api, "sunshineCreateHost").mockResolvedValue(host("new", "Sunshine 1"));
    render(
      <QueryClientProvider client={queryClient}>
        <SunshineView addTrigger={1} />
      </QueryClientProvider>,
    );

    await waitFor(() => expect(api.sunshineCreateHost).toHaveBeenCalledWith({
      name: "Sunshine 1",
      host: "192.168.1.2",
      web_port: 47990,
      username: "admin",
      password: null,
      verify_tls: true,
    }));
  });
});

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

  it("clears a saved password from the host draft and mutation variables", async () => {
    const original = host("one", "Host one");
    const saved = { ...original, password_set: true };
    let submittedPassword = "";
    vi.spyOn(api, "sunshineUpdateHost").mockImplementation(async (_id, patch) => {
      submittedPassword = patch.password ?? "";
      return saved;
    });
    vi.spyOn(api, "sunshineHosts").mockResolvedValue([saved]);
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

    const card = screen.getByRole("article", { name: /Host one/ });
    await user.click(within(card).getByRole("button", { name: /修改密码/ }));
    await user.type(within(card).getByLabelText("密码"), "sunshine-secret-789");
    await user.keyboard("{Enter}");

    await waitFor(() => expect(submittedPassword).toBe("sunshine-secret-789"));
    await waitFor(() => expect(queryClient.isMutating()).toBe(0));
    expect(mutationCacheSnapshot(queryClient)).not.toContain("sunshine-secret-789");
    await user.click(within(card).getByRole("button", { name: /修改密码/ }));
    expect((within(card).getByLabelText("密码") as HTMLInputElement).value).toBe("");
  });
});

describe("Sunshine host quick patches", () => {
  it("consumes rejected fire-and-forget password and TLS updates", async () => {
    const current = host("one", "Host one");
    const onInlineUpdate = vi.fn(() => Promise.reject(new Error("update rejected")));
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const user = userEvent.setup();
    render(
      <HostCard
        host={current}
        selected={false}
        updating={false}
        onOpen={() => undefined}
        onDelete={() => undefined}
        onInlineUpdate={onInlineUpdate}
      />,
    );

    const card = screen.getByRole("article", { name: /Host one/ });
    await user.click(within(card).getByRole("button", { name: /清空 Host one/ }));
    await user.click(within(card).getByRole("button", { name: "验证证书" }));
    await waitFor(() => expect(onInlineUpdate).toHaveBeenCalledTimes(2));
    expect(onInlineUpdate.mock.calls).toEqual([
      [{ password: "" }],
      [{ verify_tls: false }],
    ]);
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
  it("uses three-card geometry on either side of the selected card", () => {
    const common = {
      cardWidth: 200,
      cardHeight: 120,
      columnGap: 16,
      rowGap: 16,
      columnCount: 6,
      top: 272,
    };

    for (const [column, left, placement] of [
      [0, 216, "right"],
      [1, 432, "right"],
      [2, 648, "right"],
      [3, 0, "left"],
      [4, 216, "left"],
      [5, 432, "left"],
    ] as const) {
      expect(managementPanelLayout({ ...common, column })).toEqual({
        left,
        top: 272,
        width: 632,
        height: 392,
        placement,
      });
    }
  });

  it("opens management beside the host grid and closes it with Escape", async () => {
    const hosts = [host("one", "Host one")];
    vi.spyOn(api, "sunshineHosts").mockResolvedValue(hosts);
    vi.spyOn(api, "sunshineApps").mockResolvedValue({ apps: [] });
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: Infinity } },
    });
    queryClient.setQueryData(queryKeys.sunshine.hosts, hosts);
    const user = userEvent.setup();
    const { container } = render(
      <QueryClientProvider client={queryClient}>
        <SunshineView />
      </QueryClientProvider>,
    );

    const managementTrigger = within(screen.getByRole("article", { name: /Host one/ }))
      .getByRole("button", { name: "管理" });
    await user.click(managementTrigger);

    const dialog = await screen.findByRole("dialog", { name: "Host one 管理面板" });
    expect(dialog.closest(".sunshine-master-detail")).toBeTruthy();
    expect(container.querySelector(".sunshine-master-detail.has-panel")).toBeNull();
    expect(within(dialog).queryByText("Host one 管理")).toBeNull();
    const closeButton = within(dialog).getByRole("button", { name: "关闭管理面板" });
    expect(closeButton.closest(".sunshine-panel-nav-row")).toBeTruthy();
    expect(document.activeElement).toBe(closeButton);

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "Host one 管理面板" })).toBeNull();
    await waitFor(() => expect(document.activeElement).toBe(managementTrigger));
  });

  it("removes a submitted Moonlight PIN from the mutation cache", async () => {
    const hosts = [host("one", "Host one")];
    vi.spyOn(api, "sunshineHosts").mockResolvedValue(hosts);
    vi.spyOn(api, "sunshineApps").mockResolvedValue({ apps: [] });
    vi.spyOn(api, "sunshinePin").mockResolvedValue({});
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

    await user.click(within(screen.getByRole("article", { name: /Host one/ })).getByRole("button", { name: "管理" }));
    await user.click(screen.getByRole("tab", { name: "配对" }));
    await user.type(screen.getByLabelText(/PIN 码/), "8675309");
    await user.click(screen.getByRole("button", { name: "提交配对" }));

    expect(await screen.findByText("配对请求已提交。")).toBeTruthy();
    expect(api.sunshinePin).toHaveBeenCalledWith("one", "8675309", "Moonlight Client");
    expect(mutationCacheSnapshot(queryClient)).not.toContain("8675309");
    expect(queryClient.getMutationCache().findAll({
      mutationKey: ["sunshine-pair", "one"],
      exact: true,
    })).toHaveLength(0);
  });

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
