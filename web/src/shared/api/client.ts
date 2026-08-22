const REQUEST_TIMEOUT_MS = 15_000;
let authSessionGeneration = 0;

export function currentAuthSessionGeneration(): number {
  return authSessionGeneration;
}

export function advanceAuthSessionGeneration(): number {
  authSessionGeneration += 1;
  return authSessionGeneration;
}

export type ApiRequestInit = RequestInit & {
  timeoutMs?: number;
  suppressAuthExpired?: boolean;
  expectedStatus?: number;
};

export class ApiError extends Error {
  constructor(message: string, readonly code: string | undefined, readonly status: number) {
    super(message);
    this.name = "ApiError";
  }
}

async function readApiError(response: Response): Promise<ApiError> {
  // Body transport failures (especially AbortError) are request failures, not
  // empty API errors. Let the outer request lifecycle normalize them.
  const text = await response.text();
  const fallback = text || `${response.status} ${response.statusText}`;
  try {
    const payload = JSON.parse(text) as Record<string, unknown>;
    const code = typeof payload.code === "string" ? payload.code : undefined;
    const message = payload.message;
    return new ApiError(typeof message === "string" ? message : fallback, code, response.status);
  } catch {
    return new ApiError(fallback, undefined, response.status);
  }
}

/** Read the double-submit CSRF cookie set alongside the HttpOnly session. */
function csrfToken(): string {
  for (const name of ["__Host-csrf", "csrf"]) {
    const match = document.cookie.match(new RegExp(`(?:^|;\\s*)${name}=([^;]*)`));
    if (match) return decodeURIComponent(match[1]);
  }
  return "";
}

export async function request<T>(path: string, init?: ApiRequestInit): Promise<T> {
  const requestSessionGeneration = currentAuthSessionGeneration();
  const {
    timeoutMs = REQUEST_TIMEOUT_MS,
    suppressAuthExpired = false,
    expectedStatus = 200,
    ...fetchInit
  } = init ?? {};
  const controller = new AbortController();
  let didTimeout = false;
  const timeoutId = timeoutMs > 0
    ? window.setTimeout(() => { didTimeout = true; controller.abort(); }, timeoutMs)
    : undefined;
  const callerSignal = fetchInit.signal;
  const abortFromCaller = () => controller.abort(callerSignal?.reason);
  if (callerSignal?.aborted) abortFromCaller();
  else callerSignal?.addEventListener("abort", abortFromCaller, { once: true });

  try {
    const shouldSendJson = Boolean(fetchInit.body) && !(fetchInit.body instanceof FormData);
    const response = await fetch(path, {
      ...fetchInit,
      credentials: "include",
      signal: controller.signal,
      headers: {
        ...(shouldSendJson ? { "Content-Type": "application/json" } : undefined),
        ...(!fetchInit.method || ["GET", "HEAD", "OPTIONS"].includes(fetchInit.method.toUpperCase())
          ? undefined
          : { "X-CSRF-Token": csrfToken() }),
        ...fetchInit.headers,
      },
    });

    if (response.status === 401) {
      if (suppressAuthExpired) throw await readApiError(response);
      await response.body?.cancel();
      if (requestSessionGeneration !== currentAuthSessionGeneration()) {
        throw new ApiError("请求所属会话已结束", "stale_session", 401);
      }
      window.dispatchEvent(new CustomEvent("unionc:auth-expired", {
        detail: requestSessionGeneration,
      }));
      throw new ApiError("认证已失效，请重新登录", "unauthorized", 401);
    }
    if (!response.ok) throw await readApiError(response);
    if (response.status !== expectedStatus) {
      await response.body?.cancel();
      throw new ApiError(
        `UnionC 返回了非当前契约状态：应为 ${expectedStatus}，实际为 ${response.status}`,
        "unexpected_status",
        response.status,
      );
    }
    if (expectedStatus === 204) return undefined as T;
    const mediaType = response.headers.get("Content-Type")?.split(";", 1)[0]?.trim().toLowerCase();
    if (mediaType !== "application/json") {
      await response.body?.cancel();
      throw new ApiError(
        "UnionC 返回了非当前契约媒体类型，应为 application/json",
        "unexpected_content_type",
        response.status,
      );
    }
    return await response.json() as T;
  } catch (error) {
    if (controller.signal.aborted || (error instanceof DOMException && error.name === "AbortError")) {
      throw new Error(
        didTimeout ? "请求超时，请检查 UnionC 是否可用" : "请求已取消",
        { cause: error },
      );
    }
    throw error;
  } finally {
    if (timeoutId !== undefined) window.clearTimeout(timeoutId);
    callerSignal?.removeEventListener("abort", abortFromCaller);
  }
}
