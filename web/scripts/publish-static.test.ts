import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { publishStatic } from "./publish-static.mjs";

const scratch: string[] = [];

afterEach(() => {
  for (const path of scratch.splice(0)) rmSync(path, { recursive: true, force: true });
});

describe("static publishing rollback", () => {
  it("restores the previous tree when permission normalization fails", () => {
    const root = mkdtempSync(join(tmpdir(), "unionc-publish-"));
    scratch.push(root);
    mkdirSync(join(root, "dist"));
    mkdirSync(join(root, "dist.next"));
    writeFileSync(join(root, "dist", "version"), "old");
    writeFileSync(join(root, "dist.next", "version"), "new");

    expect(() => publishStatic(root, () => {
      throw new Error("simulated chmod failure");
    })).toThrow("simulated chmod failure");
    expect(readFileSync(join(root, "dist", "version"), "utf8")).toBe("old");
  });
});

