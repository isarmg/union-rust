// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";
import { authApi as api } from "../auth/api";
import { SettingsView } from "./SettingsView";

const clients: QueryClient[] = [];

afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  vi.restoreAllMocks();
});

describe("administrator password mutation lifetime", () => {
  it("embeds password inputs in rows two through four of the account card", async () => {
    vi.spyOn(api, "authenticate").mockResolvedValue({ username: "admin" });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    clients.push(queryClient);
    render(
      <QueryClientProvider client={queryClient}>
        <SettingsView onPasswordChanged={vi.fn()} />
      </QueryClientProvider>,
    );

    const form = await screen.findByRole("form", { name: "修改管理员密码" });
    const rows = form.querySelectorAll(".sarmg-card__row");
    expect(rows).toHaveLength(6);
    expect(rows[0].textContent).toContain("用户");
    expect(within(rows[1] as HTMLElement).getByLabelText("原密码")).toBeTruthy();
    expect(within(rows[2] as HTMLElement).getByLabelText("新密码")).toBeTruthy();
    expect(within(rows[3] as HTMLElement).getByLabelText("确认新密码")).toBeTruthy();
    expect(within(rows[5] as HTMLElement).getByRole("button", { name: "修改密码" })).toBeTruthy();
  });

  it("does not retain rejected passwords in the mutation cache", async () => {
    vi.spyOn(api, "authenticate").mockResolvedValue({ username: "admin" });
    vi.spyOn(api, "changePassword").mockRejectedValue(new Error("当前密码错误"));
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    clients.push(queryClient);
    const onPasswordChanged = vi.fn();
    render(
      <QueryClientProvider client={queryClient}>
        <SettingsView onPasswordChanged={onPasswordChanged} />
      </QueryClientProvider>,
    );

    const currentPassword = await screen.findByLabelText("原密码") as HTMLInputElement;
    const newPassword = screen.getByLabelText("新密码") as HTMLInputElement;
    fireEvent.change(currentPassword, { target: { value: "current-secret" } });
    fireEvent.change(newPassword, { target: { value: "replacement-secret" } });
    fireEvent.change(screen.getByLabelText("确认新密码"), { target: { value: "replacement-secret" } });
    fireEvent.submit(screen.getByRole("form", { name: "修改管理员密码" }));

    expect((await screen.findByRole("alert")).textContent).toContain("当前密码错误");
    expect(currentPassword.value).toBe("current-secret");
    expect(newPassword.value).toBe("replacement-secret");
    await waitFor(() => expect(queryClient.getMutationCache().getAll()).toHaveLength(0));
    expect(onPasswordChanged).not.toHaveBeenCalled();
  });
});
