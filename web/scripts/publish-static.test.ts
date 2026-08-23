import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { publishStatic } from "./publish-static.mjs";

const scratch: string[] = [];

afterEach(() => {
  for (const path of scratch.splice(0)) rmSync(path, { recursive: true, force: true });
});

function newRoot(): string {
  const root = mkdtempSync(join(tmpdir(), "unionc-publish-"));
  scratch.push(root);
  return root;
}

function createTree(root: string, directory: string, version: string): void {
  const tree = join(root, directory);
  mkdirSync(join(tree, "assets"), { recursive: true });
  writeFileSync(join(tree, "index.html"), `<title>${version}</title>`);
  writeFileSync(join(tree, "assets", "version"), version);
}

function version(root: string, directory = "dist"): string {
  return readFileSync(join(root, directory, "assets", "version"), "utf8");
}

describe("static publishing", () => {
  it("prepares the candidate before moving the live tree", () => {
    const root = newRoot();
    createTree(root, "dist", "old");
    createTree(root, "dist.next", "new");

    expect(() => publishStatic(root, () => {
      throw new Error("simulated chmod failure");
    })).toThrow("simulated chmod failure");

    expect(version(root)).toBe("old");
    expect(version(root, "dist.next")).toBe("new");
    expect(existsSync(join(root, "dist.previous"))).toBe(false);
  });

  it("restores the previous tree when the candidate rename fails", () => {
    const root = newRoot();
    createTree(root, "dist", "old");
    createTree(root, "dist.next", "new");
    let renames = 0;

    expect(() => publishStatic(root, undefined, undefined, (from, to) => {
      renames += 1;
      if (renames === 2) throw new Error("simulated candidate rename failure");
      renameSync(from, to);
    })).toThrow("simulated candidate rename failure");

    expect(version(root)).toBe("old");
    expect(version(root, "dist.next")).toBe("new");
    expect(existsSync(join(root, "dist.previous"))).toBe(false);
  });

  it("reports both errors when rollback also fails", () => {
    const root = newRoot();
    createTree(root, "dist", "old");
    createTree(root, "dist.next", "new");
    let renames = 0;
    let failure: unknown;

    try {
      publishStatic(root, undefined, undefined, (from, to) => {
        renames += 1;
        if (renames === 2) throw new Error("candidate rename failed");
        if (renames === 3) throw new Error("rollback rename failed");
        renameSync(from, to);
      });
    } catch (error) {
      failure = error;
    }

    expect(failure).toBeInstanceOf(AggregateError);
    expect((failure as AggregateError).errors).toHaveLength(2);
    expect(existsSync(join(root, "dist"))).toBe(false);
    expect(version(root, "dist.previous")).toBe("old");
  });

  it("recovers a tree left between renames before publishing again", () => {
    const root = newRoot();
    createTree(root, "dist.previous", "old");
    createTree(root, "dist.next", "new");

    publishStatic(root);

    expect(version(root)).toBe("new");
    expect(existsSync(join(root, "dist.next"))).toBe(false);
    expect(existsSync(join(root, "dist.previous"))).toBe(false);
  });

  it("keeps the committed tree when cleaning the backup fails", () => {
    const root = newRoot();
    createTree(root, "dist", "old");
    createTree(root, "dist.next", "new");
    const warnings: Array<{ error: unknown; path: string }> = [];

    expect(() => publishStatic(
      root,
      undefined,
      () => { throw new Error("simulated post-commit cleanup failure"); },
      undefined,
      (error, path) => warnings.push({ error, path }),
    )).not.toThrow();

    expect(version(root)).toBe("new");
    expect(existsSync(join(root, "dist.next"))).toBe(false);
    expect(existsSync(join(root, "dist.previous"))).toBe(true);
    expect(warnings).toHaveLength(1);
  });

  it("normalizes candidate permissions before the first publish", () => {
    const root = newRoot();
    createTree(root, "dist.next", "new");

    publishStatic(root);

    if (process.platform !== "win32") {
      expect(statSync(join(root, "dist")).mode & 0o777).toBe(0o755);
      expect(statSync(join(root, "dist", "index.html")).mode & 0o777).toBe(0o644);
    }
  });

  it("rejects candidates without an index document", () => {
    const root = newRoot();
    createTree(root, "dist", "old");
    mkdirSync(join(root, "dist.next"));

    expect(() => publishStatic(root)).toThrow("regular index.html");
    expect(version(root)).toBe("old");
  });

  it.skipIf(process.platform === "win32")("rejects symbolic links in the static tree", () => {
    const root = newRoot();
    createTree(root, "dist", "old");
    createTree(root, "dist.next", "new");
    symlinkSync(root, join(root, "dist.next", "assets", "external"));

    expect(() => publishStatic(root)).toThrow("symbolic link");
    expect(version(root)).toBe("old");
  });
});
