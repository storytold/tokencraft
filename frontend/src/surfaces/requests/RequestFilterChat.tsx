import { useEffect, useRef, useState } from "react";
import { ArrowUp, Sparkles } from "lucide-react";
import { Button, Input } from "@/components/ui";
import { suggestHistoryFilters, type FilterReply } from "./historyAssistant";
import type { InvestigationFilters } from "./historyDiagnostics";

const examples = ["Failed or warnings in the last 7 days", "Unconfirmed requests today"];

export function RequestFilterChat({ filters, providers, describe, onApply }: {
  filters: InvestigationFilters;
  providers: readonly string[];
  describe: (value: InvestigationFilters) => string;
  onApply: (value: InvestigationFilters) => void;
}) {
  const [draft, setDraft] = useState("");
  const [messages, setMessages] = useState<{ question: string; reply?: FilterReply }[]>([]);
  const [applied, setApplied] = useState(false);
  const pending = useRef<AbortController | null>(null);
  const latest = messages.at(-1)?.reply;
  const proposal = latest && "filters" in latest ? latest.filters : undefined;

  useEffect(() => () => pending.current?.abort(), []);

  async function ask(question: string) {
    question = question.trim();
    if (!question || pending.current) return;
    const controller = new AbortController();
    pending.current = controller;
    setDraft("");
    setApplied(false);
    setMessages((current) => [...current.slice(-2), { question }]);
    try {
      const reply = await suggestHistoryFilters(question, proposal && !applied ? proposal : filters, providers, controller.signal);
      if (!controller.signal.aborted) {
        setMessages((current) => [...current.slice(0, -1), { question, reply }]);
      }
    } catch {
      if (!controller.signal.aborted) {
        setMessages((current) => [...current.slice(0, -1), { question, reply: { error: "Could not build a filter. Try again." } }]);
      }
    } finally {
      if (pending.current === controller) pending.current = null;
    }
  }

  const busy = messages.length > 0 && !latest;
  return (
    <div className="tc-request-chat">
      <div className="mb-2 flex items-center gap-2 text-sm font-semibold">
        <Sparkles className="h-4 w-4 text-brand" aria-hidden /> Ask AI
      </div>
      {!messages.length && <div className="mb-3 flex flex-wrap gap-1.5">
        {examples.map((example) => (
          <button key={example} type="button" onClick={() => void ask(example)}
            className="rounded border border-border bg-surface px-2 py-1 text-left text-xs text-muted hover:border-brand hover:text-fg">
            {example}
          </button>
        ))}
      </div>}
      <div className="space-y-2" role="log" aria-label="Filter conversation">
        {messages.map(({ question, reply }, index) => (
          <div key={index} className="space-y-1.5 text-xs">
            <p className="ml-auto w-fit max-w-full break-words rounded-lg bg-brand/10 px-2.5 py-1.5">{question}</p>
            <p className="break-words text-muted">
              {!reply ? "Building filters…" : "error" in reply ? reply.error
                : <><span className="font-semibold text-fg">{reply.engine === "ai" ? "AI proposal" : "On-device filter preview"}</span> · {describe(reply.filters)}</>}
            </p>
          </div>
        ))}
      </div>
      {proposal && (
        <Button type="button" variant="outline" size="sm" className="mt-2" disabled={applied} onClick={() => {
          onApply(proposal);
          setApplied(true);
        }}>{applied ? "Applied" : "Apply suggested filters"}</Button>
      )}
      <form className="mt-2 flex gap-2" onSubmit={(event) => { event.preventDefault(); void ask(draft); }}>
        <Input aria-label="Describe request filters" className="min-w-0" value={draft}
          maxLength={400} autoComplete="off" placeholder="Describe the requests to find"
          onChange={(event) => setDraft(event.target.value)} />
        <Button type="submit" size="sm" className="h-10 px-2.5" disabled={busy || !draft.trim()} aria-label="Send filter question">
          <ArrowUp className="h-4 w-4" aria-hidden />
        </Button>
      </form>
    </div>
  );
}
