/*
 * 页面展示用的小工具函数。
 *
 * 这些函数不负责业务逻辑，只负责把后端返回的数字、时间、状态名转换成用户容易阅读的文本。
 */

/**
 * 把字节数格式化成 B/KB/MB/GB/TB。
 *
 * 后端和系统接口经常返回纯数字，例如 1536000。直接展示数字不直观，
 * 转成 "1.5 MB" 后用户更容易理解。
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return "0 B";
  }

  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unitIndex = 0;

  // 每超过 1024 就换到下一级单位。
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }

  return `${value.toFixed(value >= 10 ? 0 : 1)} ${units[unitIndex]}`;
}

// sysinfo 返回的内存单位是 KiB，这里先乘以 1024 转成 bytes，再复用 formatBytes。
export function formatKib(kib: number): string {
  return formatBytes(kib * 1024);
}

// 网络吞吐量按 bytes/s 返回，这里自动选择 B/KB/MB/GB/TB 并加上每秒速率后缀。
export function formatBytesPerSecond(bytesPerSecond: number): string {
  return `${formatBytes(bytesPerSecond)}/s`;
}

/**
 * 格式化后端返回的时间字符串。
 *
 * 后端一般返回 RFC3339/ISO 时间字符串，例如 "2026-06-24T10:30:00Z"。
 * 如果解析失败，就原样返回，避免页面显示成 "Invalid Date"。
 */
export function formatDateTime(value: string | null | undefined): string {
  if (!value) {
    return "-";
  }

  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }

  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit"
  }).format(date);
}

// 计算百分比并限制在 0 到 100 之间，防止异常输入把进度条撑爆。
export function percent(used: number, total: number): number {
  if (!Number.isFinite(total) || total <= 0) {
    return 0;
  }
  return Math.max(0, Math.min(100, (used / total) * 100));
}
