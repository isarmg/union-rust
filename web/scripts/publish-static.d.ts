export function publishStatic(
  appRoot?: string,
  makeReadable?: (path: string) => void,
  removeCommittedPrevious?: (path: string) => void,
  rename?: (from: string, to: string) => void,
  warnCleanupFailure?: (error: unknown, path: string) => void,
): void;
