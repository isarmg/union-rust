import { lazy } from "react";
import { Gamepad2, MonitorCog } from "lucide-react";

const SunshineView = lazy(() => import("../features/sunshine/SunshineView")
  .then((module) => ({ default: module.SunshineView })));
const MonitoringView = lazy(() => import("../features/monitoring/MonitoringView")
  .then((module) => ({ default: module.MonitoringView })));

export interface EmbeddedModuleRenderContext {
  addTrigger: number;
  onAddTriggerHandled: (trigger: number) => void;
}

export interface EmbeddedModuleDefinition {
  id: string;
  fallbackLabel: string;
  icon: React.ComponentType<{ size?: number }>;
  createLabel: string;
  render: (context: EmbeddedModuleRenderContext) => React.ReactNode;
}

export const embeddedModules: readonly EmbeddedModuleDefinition[] = [
  {
    id: "host-monitoring",
    fallbackLabel: "主机",
    icon: MonitorCog,
    createLabel: "创建 Agent",
    render: ({ addTrigger, onAddTriggerHandled }) => (
      <MonitoringView
        addTrigger={addTrigger}
        onAddTriggerHandled={onAddTriggerHandled}
      />
    ),
  },
  {
    id: "sunshine",
    fallbackLabel: "Sunshine",
    icon: Gamepad2,
    createLabel: "新建 Sunshine 实例",
    render: ({ addTrigger, onAddTriggerHandled }) => (
      <SunshineView
        addTrigger={addTrigger}
        onAddTriggerHandled={onAddTriggerHandled}
      />
    ),
  },
] as const;

export const embeddedModuleById = new Map(
  embeddedModules.map((module) => [module.id, module] as const),
);
