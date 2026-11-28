import { useState, type FormEvent } from "react";
import { Button, Card, Input, Label } from "@/components/ui";
import { accountDisplayName, api, ApiError, type ApiAccount } from "@/lib/api";
import { useOrg } from "@/lib/org";

export function OrganizationGeneral({ account }: { account: ApiAccount }) {
  const { reload } = useOrg();
  const [draft, setDraft] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ tone: "ok" | "err"; text: string } | null>(null);
  const canEdit = account.role === "owner" || account.role === "admin";
  const value = draft ?? account.business_name ?? "";

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!canEdit || busy) return;
    setBusy(true);
    setNotice(null);
    try {
      const saved = await api.updateBusinessName(account.account_id, value.trim() || null);
      const refreshed = await reload();
      setDraft(refreshed ? null : saved.business_name ?? "");
      setNotice({
        tone: "ok",
        text: saved.business_name === null
          ? `Using the generated name ${saved.placeholder_business_name}.`
          : "Organization name saved.",
      });
    } catch (error) {
      setNotice({ tone: "err", text: error instanceof ApiError ? error.message : "Couldn't save the organization name." });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card className="p-6">
      <form className="space-y-4" onSubmit={submit}>
        <div>
          <Label htmlFor="organization-name">Organization name</Label>
          <Input
            id="organization-name"
            aria-describedby="organization-name-help"
            value={canEdit ? value : accountDisplayName(account)}
            placeholder={account.placeholder_business_name}
            onChange={(event) => setDraft(event.target.value)}
            readOnly={!canEdit || busy}
          />
          <p id="organization-name-help" className="mt-1.5 text-xs text-muted">
            {canEdit ? "Leave blank to use the generated name." : "Owners and admins can change the name."}
          </p>
        </div>
        {notice && (
          <p
            role={notice.tone === "ok" ? "status" : "alert"}
            className={notice.tone === "ok" ? "text-sm text-emerald-600" : "text-sm text-rose-500"}
          >
            {notice.text}
          </p>
        )}
        {canEdit && <Button type="submit" disabled={busy}>{busy ? "Saving…" : "Save"}</Button>}
      </form>
    </Card>
  );
}
