// @vitest-environment jsdom
/// <reference types="node" />

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { InlineEditableField } from "./InlineEditableField";

afterEach(cleanup);

it("keeps a visible focus ring when inline editing enters an error state", async () => {
  const save = vi.fn(async () => undefined);
  const user = userEvent.setup();
  render(
    <InlineEditableField
      value="原名称"
      label="名称"
      validate={(value) => value ? null : "名称不能为空"}
      onSave={save}
    />,
  );

  await user.click(screen.getByRole("button", { name: /修改名称/ }));
  const input = screen.getByRole("textbox", { name: "名称" });
  fireEvent.change(input, { target: { value: "" } });
  fireEvent.keyDown(input, { key: "Enter" });

  await waitFor(() => expect(input.classList.contains("input-error")).toBe(true));
  expect(input.classList.contains("sunshine-inline-input")).toBe(true);
  expect(input.getAttribute("aria-invalid")).toBe("true");
  expect(document.activeElement).toBe(input);
  expect(save).not.toHaveBeenCalled();

  const css = readFileSync(
    join(process.cwd(), "src/features/sunshine/sunshine.css"),
    "utf8",
  );
  expect(css).toMatch(
    /\.sunshine-inline-input:focus-visible\s*\{[^}]*outline:\s*3px solid color-mix\(in srgb, var\(--primary\) 35%, transparent\);[^}]*outline-offset:\s*2px;/s,
  );
  expect(css).toMatch(
    /\.sunshine-inline-input\.input-error:focus-visible\s*\{[^}]*outline-color:\s*color-mix\(in srgb, var\(--danger\) 45%, transparent\);/s,
  );
});
