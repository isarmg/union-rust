import { describe, expect, it } from "vitest";
import { limitLogLines, MAX_RENDERED_LOG_LINES } from "./LogsView";

describe("Sunshine log rendering limit", () => {
  it("keeps only the newest lines and reports how much was omitted", () => {
    const source = Array.from({ length: MAX_RENDERED_LOG_LINES + 3 }, (_, index) => `line-${index}`);
    const rendered = limitLogLines(source);

    expect(rendered).toHaveLength(MAX_RENDERED_LOG_LINES + 1);
    expect(rendered[0]).toContain("已省略前 3 行");
    expect(rendered[1]).toBe("line-3");
    expect(rendered.at(-1)).toBe(`line-${MAX_RENDERED_LOG_LINES + 2}`);
    expect(source).toHaveLength(MAX_RENDERED_LOG_LINES + 3);
  });
});
