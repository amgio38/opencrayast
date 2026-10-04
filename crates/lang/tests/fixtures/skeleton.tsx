export const App = () => <div className="x">hi</div>;

export function Badge({ label }: { label: string }) {
  return <span>{label}</span>;
}

export class Panel {
  render() {
    return <App />;
  }
}
