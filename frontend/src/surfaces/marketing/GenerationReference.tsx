import { useEffect } from "react";
import { ArrowLeft, ArrowRight } from "lucide-react";
import { Link, useParams } from "react-router-dom";
import { CodeTabs } from "@/components/CodeBlock";
import { API_BASE_URL } from "@/lib/api";
import { MODEL_BY_SLUG } from "@/lib/catalog";
import { generationEndpoint, modelContract, modelPrimitive, modelsFor, primitiveById, primitives, scopedMediaExample, type GenerationField } from "@/lib/generation-contract";
import { demoEnabled } from "@/lib/gateway";
import { apiReferencePath } from "./api-reference-path";
import "./docs.css";

export default function GenerationReference() {
  const { primitive, model } = useParams();
  const referenced = model ? modelContract(model) : undefined;
  const catalogModel = model && Object.hasOwn(MODEL_BY_SLUG, model) ? MODEL_BY_SLUG[model] : undefined;
  const id = referenced?.profile.primitive ?? (primitive ? primitiveById(primitive) : modelPrimitive(catalogModel?.modality ?? ""));
  const contract = id ? primitives[id] : undefined;
  const text = id === "text";
  const title = referenced?.label ?? catalogModel?.name ?? (text ? "Text" : contract?.label) ?? "Schema not found";

  useEffect(() => {
    const previous = document.title;
    document.title = `${title} API schema | TokenCraft`;
    window.scrollTo(0, 0);
    return () => { document.title = previous; };
  }, [title]);

  const projectExample = !model ? scopedMediaExample(id) : undefined;
  const list = !model && id ? modelsFor(id) : [];
  const rules = referenced?.profile.rules ?? contract?.rules;
  const wirePath = generationEndpoint(id);

  return (
    <article className="docs-schema" aria-labelledby="schema-title">
      <nav className="docs-schema-crumb" aria-label="Breadcrumb">
        <Link to="/docs#model-and-job-guide"><ArrowLeft size={15} aria-hidden /> API documentation</Link>
        {model && id && <Link to={`/docs/primitives/${id}`}>{contract?.label} primitive</Link>}
      </nav>
      <header className="docs-schema-header">
        <p className="docs-schema-status">{model ? "Model capabilities" : text ? "Text API" : projectExample ? "Generation API" : "Schema preview"}</p>
        <h1 id="schema-title">{title}</h1>
        {contract && <p>{referenced?.profile.detail ?? contract.summary}</p>}
        {wirePath && <p className="docs-schema-endpoint"><span>POST</span> <code>{wirePath}</code></p>}
        {wirePath && <div className="docs-links"><Link to={apiReferencePath("POST", wirePath)}>Gateway request and response schema <ArrowRight size={15} aria-hidden /></Link></div>}
      </header>
      {text ? (
        <>
          <section aria-labelledby="schema-compat">
            <h2 id="schema-compat">Chat completions</h2>
            <p>The local provider-routing profile exposes <code>POST /v1/chat/completions</code> with an OpenAI-compatible chat body.</p>
            {catalogModel && <p>Check the active profile’s catalog for an alias that can execute a text request.</p>}
            {demoEnabled && <Link to="/docs#chat-completions">Chat request examples <ArrowRight size={15} aria-hidden /></Link>}
          </section>
          <section aria-labelledby="schema-fields">
            <h2 id="schema-fields">Request fields</h2>
            <FieldTable fields={primitives.text.fields} />
          </section>
          <section aria-labelledby="schema-constraints">
            <h2 id="schema-constraints">Constraints</h2>
            <ul className="docs-schema-rules">{primitives.text.rules.map((rule) => <li key={rule}>{rule}</li>)}</ul>
          </section>
        </>
      ) : contract ? (
        <>
          {projectExample && <section aria-labelledby="schema-project">
            <h2 id="schema-project">Project key requests</h2>
            <p>Send a project bearer key with <code>requests:submit</code> and choose a model alias available in your gateway. Preview the estimate with this body.</p>
            <CodeTabs tabs={[{ label: "JSON", lang: "json", code: JSON.stringify({ ...projectExample, dry_run: true }, null, 2) }]} />
            <p>To submit, remove <code>dry_run</code> and supply an <code>Idempotency-Key</code>. Poll <code>Location</code> with <code>requests:read</code>. Include a <code>quote_id</code> from the preview to require the same estimate.</p>
          </section>}
          {model && <p className="docs-schema-notice" role="note">Use the gateway’s scoped catalog to verify which aliases and capabilities are active in your project.</p>}
          {!wirePath && !model && <p className="docs-schema-notice" role="note">Capability preview. Consult the API reference for the currently registered generation routes.</p>}
          {referenced && (
            <>
              <section aria-labelledby="schema-fields">
                <h2 id="schema-fields">Reference preset capabilities</h2>
                <FieldTable fields={referenced.fields} />
              </section>
              <section aria-labelledby="schema-constraints">
                <h2 id="schema-constraints">Constraints</h2>
                <ul className="docs-schema-rules">{rules?.map((rule) => <li key={rule}>{rule}</li>)}</ul>
              </section>
            </>
          )}
          {list.length > 0 && <section aria-labelledby="schema-models">
            <h2 id="schema-models">Model schemas</h2>
            <div className="docs-schema-models">{list.map(({ slug, label }) => (
              <Link to={`/docs/models/${slug}`} key={slug}>{label}<ArrowRight size={15} aria-hidden /></Link>
            ))}</div>
          </section>}
          <footer className="docs-schema-footer">
            {model && id && <Link to={`/docs/primitives/${id}`}>{contract.label} schema</Link>}
            {catalogModel && <Link to={`/models/${catalogModel.slug}#api`}>Model catalog page</Link>}
            <a href={API_BASE_URL.replace(/\/$/, "") + "/v1/schemas/primitives"}>Connector primitive registry (JSON)</a>
          </footer>
        </>
      ) : <p>Use the generation guide to find a primitive or model schema.</p>}
    </article>
  );
}

function FieldTable({ fields }: { fields: GenerationField[] }) {
  return (
    <div className="docs-schema-table">
      <table aria-label="Generation request fields">
        <thead><tr><th scope="col">Field</th><th scope="col">Type</th><th scope="col">Rules and allowed values</th></tr></thead>
        <tbody>{fields.map((field) => (
          <tr key={field.name}>
            <th scope="row"><code>{field.name}</code>{field.required && <small>Required</small>}</th>
            <td>{field.type}</td>
            <td>
              {field.detail}
              {field.values && <div className="docs-schema-values">{field.values.map((item) => <code key={item}>{item}</code>)}</div>}
              {field.default !== undefined && <div>Default <code>{field.default}</code></div>}
            </td>
          </tr>
        ))}</tbody>
      </table>
    </div>
  );
}
