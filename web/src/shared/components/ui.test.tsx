// @vitest-environment jsdom

import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { Sparkline } from "./ui";

afterEach(cleanup);

describe("Sparkline", () => {
  it("keeps valid negative temperature samples inside the chart", () => {
    const { container } = render(<Sparkline data={[-20, -10]} />);
    const paths = container.querySelectorAll("path");

    expect(paths).toHaveLength(2);
    expect(paths[1].getAttribute("d")).toBe("M 0 54 C 100 54 100 28 200 28");
  });
});
