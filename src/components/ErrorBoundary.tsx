import { Button } from "@mantine/core";
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
          <div className="callout bad mx-auto grid max-w-[560px] gap-3" role="alert">
            <span><strong>This view hit an error.</strong> Your tasks keep running in the machine.</span>
            <code className="selectable text-xs [overflow-wrap:anywhere]">{this.state.error.message}</code>
            <span className="flex gap-2">
              <Button size="xs" onClick={() => this.setState({ error: null })}>Try again</Button>
              <Button size="xs" variant="default" onClick={() => window.location.reload()}>Reload Cabin</Button>
            </span>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}
