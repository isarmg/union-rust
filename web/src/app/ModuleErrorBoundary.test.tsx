// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ModuleErrorBoundary } from "./ModuleErrorBoundary";

function BrokenModule(): never {
  throw new Error("plugin render exploded");
}

describe("ModuleErrorBoundary", () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("contains a plugin render failure while the surrounding Shell stays mounted", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    render(
      <div>
        <nav aria-label="core shell">core navigation</nav>
        <ModuleErrorBoundary moduleId="broken" route="/modules/broken">
          <BrokenModule />
        </ModuleErrorBoundary>
      </div>,
    );

    expect(screen.getByRole("navigation", { name: "core shell" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "模块页面加载失败" })).toBeTruthy();
    expect(screen.getByText("plugin render exploded")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "重试此模块" }));
    expect(screen.getByRole("navigation", { name: "core shell" })).toBeTruthy();
  });
});
