import type { ServiceStatus } from "./types";
import {
  CardInner,
  CardRow,
  StatusLed,
  TickerText,
  TruncatedText,
} from "../../shared/components/ui";

function serviceLedTone(service: ServiceStatus): "good" | "warn" | "danger" {
  if (service.healthy) return "good";
  const stopped = ["stopped", "not-configured"].includes(service.runtime_state);
  return stopped ? "danger" : "warn";
}

export function ServiceCard({
  service,
  compact = false,
}: {
  service: ServiceStatus;
  compact?: boolean;
}) {
  return (
    <article className={compact ? "content-card service-card compact" : "content-card service-card"}>
      <CardInner>
        <CardRow label="名称">
          <TruncatedText grow><TickerText>{service.name}</TickerText></TruncatedText>
          <StatusLed tone={serviceLedTone(service)} />
        </CardRow>
        <CardRow label="状态"><TickerText>{service.runtime_state}</TickerText></CardRow>
        <CardRow label="PID">{service.pid ?? "-"}</CardRow>
        <CardRow label="地址">
          {service.address
            ? <TruncatedText><TickerText>{service.address}</TickerText></TruncatedText>
            : "-"}
        </CardRow>
        <CardRow label="消息">
          {!compact && service.message ? <TruncatedText muted>{service.message}</TruncatedText> : null}
        </CardRow>
      </CardInner>
    </article>
  );
}

