import { DispatchControls } from "./DispatchControls";
import { useOrg } from "@/lib/org";
import { ProviderManagement } from "@/surfaces/projects/providers/ProviderManagement";
import { ProjectProviderKeys } from "@/surfaces/projects/ProjectProviderKeys";
import { ProjectDisclosure } from "@/surfaces/projects/ProjectDisclosure";
import { useProject } from "@/surfaces/projects/ProjectScope";

export default function Providers() {
  const { current } = useOrg();
  const { projects } = useProject();
  if (!current) return null;

  const scope = { account_id: current.account_id };
  const canManage = current.role === "owner" || current.role === "admin";

  return (
    <div className="mx-auto max-w-7xl space-y-4">
      <header className="border-b border-border pb-4">
        <h1 className="font-display text-4xl font-extrabold tracking-tight sm:text-5xl">Providers</h1>
      </header>
      {canManage ? (
        <ProviderManagement key={current.account_id} scope={scope} projects={projects}>
          <ProjectDisclosure title="Provider credentials" description="Organization defaults" defer>
            <ProjectProviderKeys scope={scope} />
          </ProjectDisclosure>
          <ProjectDisclosure title="Dispatch policy" description="Pause or resume new provider claims" defer>
            <DispatchControls accountId={scope.account_id} />
          </ProjectDisclosure>
        </ProviderManagement>
      ) : <p className="text-sm text-muted">Organization owners and admins can manage providers.</p>}
    </div>
  );
}
