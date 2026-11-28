import { Button } from "@/components/ui";

export function RequestHistoryError({ error, showingPrevious, onRetry }: {
  error: string;
  showingPrevious: boolean;
  onRetry: () => void;
}) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      <p role="alert" className="text-sm text-rose-600 dark:text-rose-300">
        {error}{showingPrevious && " Showing the last loaded page."}
      </p>
      <Button type="button" variant="outline" size="sm" onClick={onRetry}>
        Retry history
      </Button>
    </div>
  );
}
