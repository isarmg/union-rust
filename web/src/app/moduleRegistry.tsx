import { lazy } from "react";
import { Gamepad2, MonitorCog } from "lucide-react";

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

const selectedModules: EmbeddedModuleDefinition[] = [];

if (__UNIONC_MODULE_HOST_MONITORING__) {
  const MonitoringView = lazy(() => import("../features/monitoring/MonitoringView")
    .then((module) => ({ default: module.MonitoringView })));
  selectedModules.push({
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
  });
}

if (__UNIONC_MODULE_SUNSHINE__) {
  const SunshineView = lazy(() => import("../features/sunshine/SunshineView")
    .then((module) => ({ default: module.SunshineView })));
  selectedModules.push({
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
  });
}

export const embeddedModules: readonly EmbeddedModuleDefinition[] = selectedModules;

export const embeddedModuleById = new Map(
  embeddedModules.map((module) => [module.id, module] as const),
);
