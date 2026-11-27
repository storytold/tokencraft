import { Button } from "@/components/ui";

export function ReceiptRecovery({ sending, retry, dismiss }: { sending: boolean; retry: () => void; dismiss: () => void }) {
  return (
    <div className="space-y-2 rounded-lg border border-border p-3">
      <p role="status" className="text-sm text-muted">The outcome is unknown. Recovery resends the original request with the same key. Check Request history before dismissing.</p>
      <div className="flex flex-wrap gap-2">
        <Button type="button" size="sm" variant="outline" disabled={sending} onClick={retry}>
          {sending ? "Recovering receipt…" : "Recover receipt"}
        </Button>
        <Button type="button" size="sm" variant="ghost" disabled={sending} onClick={dismiss}>Dismiss recovery</Button>
      </div>
    </div>
  );
}
