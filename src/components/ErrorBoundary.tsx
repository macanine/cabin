import { Component, type ReactNode } from "react";
import { reportFrontendDiagnostic } from "../services/ipc/common";

interface State {
  error: Error | null;
}

/** Keeps a rendering bug in one view from blanking the whole window. */
export class ErrorBoundary extends Component<{ children: ReactNode; resetKey: string }, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidUpdate(previous: { resetKey: string }) {
    if (previous.resetKey !== this.props.resetKey && this.state.error) this.setState({ error: null });
  }

  componentDidCatch() {
    reportFrontendDiagnostic("viewCrash");
  }

  render() {
    if (this.state.error) {
      return (
        <div className="page">
          <div className="callout bad">
            This view hit an error: {this.state.error.message}. Pick another item in the sidebar, or reload with Ctrl+R.
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}
