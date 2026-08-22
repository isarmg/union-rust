import { chmodSync, existsSync, lstatSync, readdirSync, renameSync, rmSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const defaultAppRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function makeStaticTreeReadable(path) {
  const stat = lstatSync(path);
  if (stat.isSymbolicLink()) return;
  if (stat.isDirectory()) {
    chmodSync(path, 0o755);
    for (const entry of readdirSync(path)) {
      makeStaticTreeReadable(resolve(path, entry));
    }
    return;
  }
  if (stat.isFile()) chmodSync(path, 0o644);
}

export function publishStatic(
  appRoot = defaultAppRoot,
  makeReadable = makeStaticTreeReadable,
  removeCommittedPrevious = (path) => rmSync(path, { recursive: true, force: true }),
) {
  const next = resolve(appRoot, "dist.next");
  const current = resolve(appRoot, "dist");
  const previous = resolve(appRoot, "dist.previous");

  if (!existsSync(next)) throw new Error("dist.next does not exist");
  rmSync(previous, { recursive: true, force: true });
  if (existsSync(current)) renameSync(current, previous);
  try {
    renameSync(next, current);
    makeReadable(current);
  } catch (error) {
    // rename 已成功而 chmod 失败时，current 是一棵不完整的新版本。先删除它才能
    // 把 previous 原子恢复回来；仅在 current 不存在时回滚会把坏版本留在线上。
    if (existsSync(previous)) {
      rmSync(current, { recursive: true, force: true });
      renameSync(previous, current);
    }
    throw error;
  }

  // 新目录已经换入且权限已归一化，至此发布已经提交。旧备份的清理即使失败，
  // 也只能报告错误并留给后续清理，绝不能删除已提交的新版本再恢复残缺备份。
  if (existsSync(previous)) removeCommittedPrevious(previous);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  publishStatic();
}
