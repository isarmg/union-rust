import { request } from "../shared/api/client";

export const realtimeApi = {
  issueSseTicket: () => request<{ ticket: string }>("/api/events/ticket", { method: "POST" }),
};

