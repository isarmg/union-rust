import { Component, type ErrorInfo, type ReactNode } from "react";
import { AlertTriangle, RefreshCw } from "lucide-react";

interface Props {
  moduleId: string;
  route: string;
  children: ReactNode;
}
interface State {
  error: Error | null;
  reset: number;
}

/** Keeps a broken plugin component from taking down authentication, navigation, or other modules. */
export class ModuleErrorBoundary extends Component<Props, State> {
  state: State = { error: null, reset: 0 };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error(`UnionC module ${this.props.moduleId} render failed`, error, info.componentStack);
  }

  componentDidUpdate(previous: Props) {
    if (this.state.error && previous.route !== this.props.route) {
      this.setState(({ reset }) => ({ error: null, reset: reset + 1 }));
    }
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <section className="module-error-card" role="alert">
        <AlertTriangle size={24} aria-hidden="true" />
        <h1>模块页面加载失败</h1>
        <p>{this.state.error.message || `${this.props.moduleId} 遇到了未预期的错误。`}</p>
        <button
          className="action-button primary"
          type="button"
          onClick={() => this.setState(({ reset }) => ({ error: null, reset: reset + 1 }))}
        >
          <RefreshCw size={16} aria-hidden="true" /><span>重试此模块</span>
        </button>
      </section>
    );
  }
}
