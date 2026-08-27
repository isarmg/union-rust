// 共享 UI 基础组件。
//
// 所有视图页面都可能用到的通用展示和交互元素集中放在这里，
// 避免在每个 View 文件里重复写相同的结构和 CSS 类。

import { useEffect, useRef, useState } from "react";
import { BellDot, Loader2 } from "lucide-react";

// ─── 跑马灯文字 ───────────────────────────────────────────────────────────────
//
// 文字宽度超出容器时自动切换为水平循环滚动；未超出则保持静态单行显示。
// 通过隐藏的量测节点检测溢出，避免直接测量动画元素宽度造成误判。

export function TickerText({ children }: { children: string }) {
  const outerRef = useRef<HTMLSpanElement>(null);
  const measureRef = useRef<HTMLSpanElement>(null);
  const [isOverflow, setIsOverflow] = useState(false);

  useEffect(() => {
    const outer = outerRef.current;
    const measure = measureRef.current;
    if (!outer || !measure) return;

    const check = () => {
      setIsOverflow(measure.scrollWidth > outer.clientWidth + 1);
    };

    const raf = requestAnimationFrame(check);
    const observer = new ResizeObserver(check);
    observer.observe(outer);
    return () => { cancelAnimationFrame(raf); observer.disconnect(); };
  }, [children]);

  return (
    <span ref={outerRef} className="ticker-outer">
      {/* 隐藏量测节点，始终渲染单份文字用于宽度检测 */}
      <span ref={measureRef} className="ticker-measure" aria-hidden="true">{children}</span>
      {isOverflow ? (
        <span className="ticker-animate">
          <span className="ticker-unit">{children}</span>
          <span className="ticker-unit" aria-hidden="true">{children}</span>
        </span>
      ) : (
        <span className="ticker-static">{children}</span>
      )}
    </span>
  );
}

// ─── 内容块通用布局原语 ───────────────────────────────────────────────────────

/** 等边距内容框：使用内容块短边计算四边间距，内部等分六行 */
export function CardInner({ children }: { children: React.ReactNode }) {
  return <div className="sarmg-card__inner">{children}</div>;
}

/** 标准内容行：左列标题 + 2ch 间距 + 右列内容 */
export function CardRow({
  label,
  children,
  span,
  row,
  chart
}: {
  label: React.ReactNode;
  children?: React.ReactNode;
  span?: number;
  row?: number;
  chart?: boolean;
}) {
  const gridRow = row ? String(row) : span ? `span ${span}` : undefined;
  return (
    <div
      className={`sarmg-card__row${chart ? " sarmg-card__row-chart" : ""}`}
      style={gridRow ? { gridRow } : undefined}
    >
      <span className="sarmg-card__label">{label}</span>
      <div className="sarmg-card__content">{children}</div>
    </div>
  );
}

/** 内容块中的单行截断文字。 */
export function TruncatedText({
  children,
  muted = false,
  grow = false,
  className = "",
  ...spanProps
}: React.HTMLAttributes<HTMLSpanElement> & {
  muted?: boolean;
  grow?: boolean;
}) {
  const classes = [
    "sarmg-truncate",
    muted ? "sarmg-muted" : "",
    grow ? "sarmg-grow" : "",
    className
  ].filter(Boolean).join(" ");
  return <span {...spanProps} className={classes}>{children}</span>;
}

/** 固定在内容块第六行的通用操作区。 */
export function CardActions({
  children,
  label = "操作",
  className = "",
  onClick
}: {
  children: React.ReactNode;
  label?: React.ReactNode;
  className?: string;
  onClick?: React.MouseEventHandler<HTMLDivElement>;
}) {
  return (
    <CardRow label={label} row={6}>
      <div className={`sarmg-card__actions${className ? ` ${className}` : ""}`} onClick={onClick}>{children}</div>
    </CardRow>
  );
}

// ─── 指标卡片 ─────────────────────────────────────────────────────────────────

export function Sparkline({
  data,
  color = "var(--primary)",
  maxValue
}: {
  data: Array<number | null>;
  color?: string;
  /** 指定 Y 轴最大值以固定纵坐标范围（如 CPU/内存传 100）；
   *  不传时自适应到数据最大值，适合网络等量纲不固定的指标。 */
  maxValue?: number;
}) {
  const validValues = data.filter((value): value is number => typeof value === "number" && Number.isFinite(value));
  if (validValues.length < 2) return null;
  const W = 200;
  const H = 56;
  const verticalPad = 2;
  // Most metrics are non-negative, but temperature is allowed to drop below
  // zero. Keep zero in the automatic domain while expanding the lower bound
  // for negative samples, otherwise those points are projected below the SVG.
  const min = Math.min(0, ...validValues);
  const max = Math.max(maxValue ?? Math.max(...validValues), 0, min + 0.001);
  const range = max - min;
  // 横向端点贴齐 SVG 边界；纵向仍留出空间，避免峰值线被裁切。
  const tx = (i: number) => (i / (data.length - 1)) * W;
  const ty = (v: number) => H - verticalPad - ((v - min) / range) * (H - verticalPad * 2);

  const segments: Array<Array<{ index: number; value: number }>> = [];
  for (const [index, value] of data.entries()) {
    if (typeof value !== "number" || !Number.isFinite(value)) continue;
    const previous = segments.at(-1);
    if (!previous || previous.at(-1)!.index !== index - 1) segments.push([]);
    segments.at(-1)!.push({ index, value });
  }
  const pathFor = (segment: Array<{ index: number; value: number }>) => {
    let path = `M ${tx(segment[0].index)} ${ty(segment[0].value)}`;
    for (let position = 1; position < segment.length; position += 1) {
      const previous = segment[position - 1];
      const current = segment[position];
      const cx = (tx(previous.index) + tx(current.index)) / 2;
      path += ` C ${cx} ${ty(previous.value)} ${cx} ${ty(current.value)} ${tx(current.index)} ${ty(current.value)}`;
    }
    return path;
  };

  return (
    <svg
      viewBox={`0 0 ${W} ${H}`}
      preserveAspectRatio="none"
      width="100%"
      height="100%"
      style={{ display: "block", position: "absolute", inset: 0 }}
      aria-hidden="true"
    >
      {segments.map((segment, index) => {
        if (segment.length === 1) {
          const [{ index: pointIndex, value }] = segment;
          return (
            <line
              key={`${pointIndex}-${index}`}
              x1={tx(pointIndex)}
              x2={tx(pointIndex)}
              y1={ty(value)}
              y2={ty(value)}
              stroke={color}
              strokeWidth={4}
              strokeLinecap="round"
              vectorEffect="non-scaling-stroke"
            />
          );
        }
        const path = pathFor(segment);
        const fillPath = `${path} L ${tx(segment.at(-1)!.index)} ${H} L ${tx(segment[0].index)} ${H} Z`;
        return (
          <g key={`${segment[0].index}-${index}`}>
            <path d={fillPath} style={{ fill: color, fillOpacity: 0.12 }} />
            <path d={path} style={{ fill: "none", stroke: color, strokeWidth: 2 }} vectorEffect="non-scaling-stroke" />
          </g>
        );
      })}
    </svg>
  );
}

export function Metric({
  label,
  value,
  detail,
  tone,
  title,
  sparkData,
  sparkColor,
  sparkMax
}: {
  label: string;
  value: string;
  detail?: string;
  tone: "good" | "warn" | "neutral";
  title?: string;
  sparkData?: Array<number | null>;
  sparkColor?: string;
  sparkMax?: number;
}) {
  const hasChart = sparkData && sparkData.filter((value) => typeof value === "number" && Number.isFinite(value)).length >= 2;
  return (
    <article className={`sarmg-card metric ${tone}`} title={title}>
      <CardInner>
        <CardRow label={label}>
          <strong className="metric-row-value">{value}</strong>
        </CardRow>
        <CardRow label="详情">
          {detail ? <span className="metric-row-detail">{detail}</span> : null}
        </CardRow>
        {hasChart ? (
          <div className="card-spark-row metric-chart-slot">
            <Sparkline data={sparkData} color={sparkColor ?? "var(--primary)"} maxValue={sparkMax} />
          </div>
        ) : null}
      </CardInner>
    </article>
  );
}

// ─── 通用按钮 ─────────────────────────────────────────────────────────────────

export function ActionButton({
  icon: Icon,
  label,
  busy,
  disabled,
  tone = "primary",
  onClick
}: {
  icon: React.ComponentType<{ size?: number }>;
  label: string;
  busy?: boolean;
  disabled?: boolean;
  tone?: "primary" | "danger";
  onClick: () => void;
}) {
  return (
    <button
      className={`action-button ${tone}`}
      type="button"
      onClick={onClick}
      disabled={busy || disabled}
      title={label}
    >
      {busy ? <Loader2 className="spin" size={16} /> : <Icon size={16} />}
      <span>{label}</span>
    </button>
  );
}

// ─── 区域标题 ─────────────────────────────────────────────────────────────────

export function SectionHeader({
  icon: Icon,
  title,
  description,
  actions
}: {
  icon: React.ComponentType<{ size?: number }>;
  title: string;
  description?: string;
  actions?: React.ReactNode;
}) {
  return (
    <div className="section-header">
      <ContentTitle icon={Icon} title={title} description={description} />
      {actions ? <div className="section-actions">{actions}</div> : null}
    </div>
  );
}

/** 内容块网格统一使用“图标 + 名称”，图标和名称高度均为 18px。 */
export function ContentTitle({ icon: Icon, title, description }: {
  icon: React.ComponentType<{ size?: number }>;
  title: string;
  description?: string;
}) {
  return (
    <div className="section-title">
      <Icon size={18} />
      <div>
        <h2>{title}</h2>
        {description ? <p>{description}</p> : null}
      </div>
    </div>
  );
}

// ─── 状态标记 ─────────────────────────────────────────────────────────────────

/** 圆形 LED 状态指示灯：green=正常，yellow=繁忙/检测中，red=错误/离线 */
export function StatusLed({ tone }: { tone: "good" | "warn" | "danger" }) {
  return <span className={`sarmg-status-led sarmg-status-${tone}`} aria-hidden="true" />;
}

// ─── 通知与错误 ───────────────────────────────────────────────────────────────

export function InlineNotice({
  tone,
  text
}: {
  tone: "warn" | "danger";
  text: string;
}) {
  return (
    <div className={`inline-notice ${tone}`} role={tone === "danger" ? "alert" : "status"} aria-live={tone === "danger" ? "assertive" : "polite"}>
      <BellDot size={16} aria-hidden="true" />
      <span>{text}</span>
    </div>
  );
}

export function MutationError({
  mutation
}: {
  mutation: { error: Error | null; isError: boolean };
}) {
  if (!mutation.isError || !mutation.error) {
    return null;
  }
  return <InlineNotice tone="danger" text={mutation.error.message} />;
}

// ─── 进度条 ───────────────────────────────────────────────────────────────────

export function ProgressBar({ value }: { value: number }) {
  const normalized = Math.max(0, Math.min(value, 100));
  return (
    <div className="progress" role="progressbar" aria-label="使用率" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(normalized)}>
      <span style={{ width: `${normalized}%` }} />
    </div>
  );
}

// ─── 加载占位 ─────────────────────────────────────────────────────────────────

export function LoadingBlock({ label }: { label: string }) {
  return (
    <div className="loading-block" role="status" aria-live="polite">
      <Loader2 className="spin" size={18} aria-hidden="true" />
      <span>{label}</span>
    </div>
  );
}
