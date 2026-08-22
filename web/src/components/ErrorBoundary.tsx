import { Component, type ErrorInfo, type ReactNode } from "react";
import { AlertTriangle, RefreshCw } from "lucide-react";

interface ErrorBoundaryState {
  error: Error | null;
}

export class ErrorBoundary extends Component<{ children: ReactNode }, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("UnionC UI render failed", error, info.componentStack);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <main className="app-shell fatal-error-screen" role="alert">
        <section className="fatal-error-card">
          <AlertTriangle size={28} aria-hidden="true" />
          <h1>页面加载失败</h1>
          <p>{this.state.error.message || "前端遇到了未预期的错误。"}</p>
          <button className="action-button primary" type="button" onClick={() => window.location.reload()}>
            <RefreshCw size={16} aria-hidden="true" />
            <span>重新加载</span>
          </button>
        </section>
      </main>
    );
  }
}
