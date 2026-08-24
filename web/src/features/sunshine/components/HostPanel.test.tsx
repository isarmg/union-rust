// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, expect, it, vi } from "vitest";
import { sunshineApi as api } from "../api";
import type { SunshineHostInfo } from "../types";
import { HostPanel } from "./HostPanel";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const host: SunshineHostInfo = {
  id: "sunshine-one",
  name: "客厅 Sunshine",
  host: "sunshine.example.test",
  web_port: 47990,
  username: "admin",
  password_set: true,
  verify_tls: true,
  web_url: "https://sunshine.example.test:47990",
  probe_status: "complete",
  reachable: true,
  connected: true,
};

it("associates Sunshine tabs with stable panels and activates them from the keyboard", () => {
  vi.spyOn(api, "sunshineApps").mockResolvedValue({ apps: [] });
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={queryClient}>
      <HostPanel host={host} onClose={() => undefined} />
    </QueryClientProvider>,
  );

  const tablist = screen.getByRole("tablist", { name: "客厅 Sunshine 管理功能" });
  const tabs = within(tablist).getAllByRole("tab");
  const panels = screen.getAllByRole("tabpanel", { hidden: true });
  expect(tabs).toHaveLength(5);
  expect(panels).toHaveLength(5);
  for (const tab of tabs) {
    const panel = document.getElementById(tab.getAttribute("aria-controls")!);
    expect(panel).not.toBeNull();
    expect(panel?.getAttribute("aria-labelledby")).toBe(tab.id);
  }
  expect(tabs.map((tab) => tab.tabIndex)).toEqual([0, -1, -1, -1, -1]);

  tabs[0].focus();
  fireEvent.keyDown(tabs[0], { key: "ArrowRight" });
  expect(document.activeElement).toBe(tabs[1]);
  expect(tabs[1].getAttribute("aria-selected")).toBe("true");

  fireEvent.keyDown(tabs[1], { key: "End" });
  expect(document.activeElement).toBe(tabs[4]);
  fireEvent.keyDown(tabs[4], { key: "Home" });
  expect(document.activeElement).toBe(tabs[0]);
  fireEvent.keyDown(tabs[0], { key: "ArrowLeft" });
  expect(document.activeElement).toBe(tabs[4]);
  expect(tabs.map((tab) => tab.tabIndex)).toEqual([-1, -1, -1, -1, 0]);
});
