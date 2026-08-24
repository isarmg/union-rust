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

  it("renders valid singleton segments separated by a gap as visible points", () => {
    const { container } = render(<Sparkline data={[25, null, 75]} color="rgb(1, 2, 3)" />);
    const points = container.querySelectorAll("line");

    expect(points).toHaveLength(2);
    expect(container.querySelectorAll("path")).toHaveLength(0);
    expect(Array.from(points, (point) => ({
      x1: point.getAttribute("x1"),
      x2: point.getAttribute("x2"),
      stroke: point.getAttribute("stroke"),
      linecap: point.getAttribute("stroke-linecap"),
      vectorEffect: point.getAttribute("vector-effect"),
    }))).toEqual([
      { x1: "0", x2: "0", stroke: "rgb(1, 2, 3)", linecap: "round", vectorEffect: "non-scaling-stroke" },
      { x1: "200", x2: "200", stroke: "rgb(1, 2, 3)", linecap: "round", vectorEffect: "non-scaling-stroke" },
    ]);
  });
});
