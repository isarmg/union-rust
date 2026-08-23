import { chmodSync, existsSync, lstatSync, opendirSync, renameSync, rmSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const defaultAppRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const MAX_STATIC_TREE_ENTRIES = 10_000;

function makeStaticTreeReadable(root) {
  let visited = 0;
  const pending = [root];
  while (pending.length > 0) {
    const path = pending.pop();
    visited += 1;
    if (visited > MAX_STATIC_TREE_ENTRIES) {
      throw new Error(`static tree exceeds ${MAX_STATIC_TREE_ENTRIES} entries`);
    }
    const stat = lstatSync(path);
    if (stat.isSymbolicLink()) throw new Error(`static tree contains a symbolic link: ${path}`);
    if (stat.isDirectory()) {
      chmodSync(path, 0o755);
      const directory = opendirSync(path);
      try {
        let entry;
        while ((entry = directory.readSync()) !== null) pending.push(resolve(path, entry.name));
      } finally {
        directory.closeSync();
      }
      continue;
    }
    if (stat.isFile()) {
      chmodSync(path, 0o644);
      continue;
    }
    throw new Error(`static tree contains an unsupported entry: ${path}`);
  }

  const index = resolve(root, "index.html");
  if (!existsSync(index) || !lstatSync(index).isFile()) {
    throw new Error("dist.next must contain a regular index.html file");
  }
}

function reportCleanupFailure(error, path) {
  process.emitWarning(`static publish committed but could not remove ${path}: ${error}`, {
    code: "UNIONC_STATIC_CLEANUP",
  });
}

export function publishStatic(
  appRoot = defaultAppRoot,
  makeReadable = makeStaticTreeReadable,
  removeCommittedPrevious = (path) => rmSync(path, { recursive: true, force: true }),
  rename = renameSync,
  warnCleanupFailure = reportCleanupFailure,
) {
  const next = resolve(appRoot, "dist.next");
  const current = resolve(appRoot, "dist");
  const previous = resolve(appRoot, "dist.previous");

  // A previous process may have stopped between the two directory renames. Restore service
  // before validating the next candidate; this is crash recovery, not a crash-atomic swap.
  if (!existsSync(current) && existsSync(previous)) rename(previous, current);
  if (!existsSync(next)) throw new Error("dist.next does not exist");

  // Permissions and shape must be ready before the live directory is moved away.
  makeReadable(next);
  rmSync(previous, { recursive: true, force: true });
  const hadCurrent = existsSync(current);
  if (hadCurrent) rename(current, previous);
  try {
    rename(next, current);
  } catch (error) {
    if (hadCurrent && existsSync(previous)) {
      try {
        rmSync(current, { recursive: true, force: true });
        rename(previous, current);
      } catch (rollbackError) {
        throw new AggregateError(
          [error, rollbackError],
          "static publish failed and the previous tree could not be restored",
          { cause: rollbackError },
        );
      }
    }
    throw error;
  }

  // The new tree is committed. Backup cleanup is best-effort: failing the build now would invite
  // an unsafe retry even though the live version already changed.
  if (existsSync(previous)) {
    try {
      removeCommittedPrevious(previous);
    } catch (error) {
      warnCleanupFailure(error, previous);
    }
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  publishStatic();
}
