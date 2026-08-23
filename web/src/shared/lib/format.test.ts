import { describe, expect, it } from "vitest";
import {
  addJsonU64,
  formatInteger,
  percent,
  subtractJsonU64,
} from "./format";

describe("lossless JSON u64 formatting", () => {
  it("keeps arithmetic exact beyond the JavaScript number boundary", () => {
    expect(subtractJsonU64("9007199254740993", "9007199254740992")).toBe(1n);
    expect(addJsonU64("18446744073709551614", 1)).toBe(18_446_744_073_709_551_615n);
    expect(percent("9007199254740993", "18014398509481986")).toBe(50);
  });

  it("formats large counters without first coercing them to number", () => {
    expect(formatInteger("9007199254740993")).toBe("9,007,199,254,740,993");
    expect(formatInteger(addJsonU64("18446744073709551615", "18446744073709551615")))
      .toBe("36,893,488,147,419,103,230");
  });

  it("fails closed for malformed or out-of-range strings", () => {
    expect(formatInteger("01")).toBe("-");
    expect(formatInteger("18446744073709551616")).toBe("-");
  });
});
