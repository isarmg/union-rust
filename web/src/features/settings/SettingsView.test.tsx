// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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

    fireEvent.click(await screen.findByRole("button", { name: "修改密码" }));
    const currentPassword = screen.getByLabelText("当前密码") as HTMLInputElement;
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
