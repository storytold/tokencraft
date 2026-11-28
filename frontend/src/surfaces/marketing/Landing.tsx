import { useEffect, useRef, useState } from "react";
import { AudioLines, Box, Check, ChevronDown, Database, GitBranch, Github, Globe, Image, KeyRound, ListTree, MessageSquare, Pause, Play, Terminal, Video, type LucideIcon } from "lucide-react";
import { Link } from "react-router-dom";
import { Container } from "@/components/ui";
import { useAuth } from "@/lib/auth";
import { getStartedPath } from "@/lib/onboarding";
import "./landing.css";

const catalogLinks = [
  { icon: MessageSquare, title: "Language", body: "Chat, reasoning and text generation.", to: "/models?modality=chat" },
  { icon: Image, title: "Images", body: "Create, edit and refine images.", to: "/models?modality=image" },
  { icon: Video, title: "Video", body: "Make clips from text or images.", to: "/models?modality=video" },
  { icon: AudioLines, title: "Audio", body: "Generate audio with the TokenCraft API.", to: "/docs/primitives/audio" },
  { icon: Box, title: "3D meshes", body: "Generate 3D assets from prompts or images.", to: "/models?modality=3d" },
  { icon: Globe, title: "3D worlds", body: "Explore models for spatial scenes.", to: "/models?modality=world" },
];

const features = [
  { icon: ListTree, title: "Trace every request", body: "Follow each request, investigate failures and review usage by project or client." },
  { icon: KeyRound, title: "Scope access by project", body: "Give each app or environment an API key with its own access." },
  { icon: Database, title: "Keep your usage data", body: "Export request and usage records or sync them to your own PostgreSQL database." },
];

type ExampleRoute = {
  model: string;
  provider: string;
  rating?: number;
  unavailable?: string;
};

type RoutingExample = {
  icon: LucideIcon;
  label: string;
  request: string;
  settings: string;
  supportsQuality: boolean;
  routes: ExampleRoute[];
};

const routingExamples: RoutingExample[] = [
  {
    icon: Image,
    label: "Image",
    request: "Create a product image",
    settings: "One image · 4:3 aspect ratio",
    supportsQuality: true,
    routes: [
      { model: "Fast image model", provider: "Provider A", rating: 7, unavailable: "4:3 unsupported" },
      { model: "Fast image model", provider: "Provider B", rating: 7 },
      { model: "Detail image model", provider: "Provider C", rating: 9 },
    ],
  },
  {
    icon: Video,
    label: "Video",
    request: "Animate a product shot",
    settings: "Eight seconds · 1080p",
    supportsQuality: true,
    routes: [
      { model: "Fast video model", provider: "Provider C", rating: 7, unavailable: "720p maximum" },
      { model: "Fast video model", provider: "Provider B", rating: 7 },
      { model: "Studio video model", provider: "Provider A", rating: 9 },
    ],
  },
  {
    icon: MessageSquare,
    label: "Language",
    request: "Summarize a long brief",
    settings: "128K context window",
    supportsQuality: false,
    routes: [
      { model: "Long-context model", provider: "Provider A", unavailable: "64K context limit" },
      { model: "Long-context model", provider: "Provider C" },
      { model: "Long-context model", provider: "Provider B" },
    ],
  },
];

const minimumRating = 8;

export default function Landing() {
  const { user } = useAuth();
  const start = getStartedPath(Boolean(user));
  const agent = getStartedPath(Boolean(user), true);
  const page = useRef<HTMLDivElement>(null);
  const [prismPaused, setPrismPaused] = useState(false);
  const prismAction = prismPaused ? "Resume prism animation" : "Pause prism animation";
  const PrismIcon = prismPaused ? Play : Pause;

  useEffect(() => {
    const root = page.current;
    if (!root || !("IntersectionObserver" in window)) return;

    const heroArt = root.querySelector(".landing-hero-art");
    const updateTabVisibility = () => {
      heroArt?.toggleAttribute("data-tab-hidden", document.hidden);
    };

    const observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (entry.target === heroArt) {
          heroArt.toggleAttribute("data-in-view", entry.isIntersecting);
          continue;
        }
        if (!entry.isIntersecting) continue;
        entry.target.setAttribute("data-visible", "");
        observer.unobserve(entry.target);
      }
    }, { rootMargin: "0px 0px -6% 0px" });

    if (heroArt) {
      heroArt.setAttribute("data-motion-observed", "");
      observer.observe(heroArt);
      updateTabVisibility();
      document.addEventListener("visibilitychange", updateTabVisibility);
    }
    root.querySelectorAll(".landing-reveal:not([data-visible])").forEach((element) => observer.observe(element));
    return () => {
      observer.disconnect();
      document.removeEventListener("visibilitychange", updateTabVisibility);
    };
  }, []);

  return (
    <div className="landing-page" ref={page}>
      <section className="landing-hero" aria-labelledby="landing-title">
        <div className="landing-hero-art" data-motion-paused={prismPaused ? "" : undefined}>
          <img
            className="landing-hero-image"
            src="/brand/tokencraft-hero-centered-prism.webp"
            alt="A centered diamond prism in a square amber-glass frame fans one beam into four copper rays."
            width={1536}
            height={1024}
            fetchPriority="high"
            onLoad={(event) => { event.currentTarget.parentElement?.setAttribute("data-loaded", ""); }}
          />
          <div className="landing-prism-flow" aria-hidden="true">
            <span className="landing-prism-glow" />
            <span className="landing-prism-halo landing-prism-halo-inlet" />
            <span className="landing-prism-halo landing-prism-halo-outlet" />
            <span className="landing-prism-fleck landing-prism-inlet" />
            <span className="landing-prism-fleck landing-prism-outlet landing-prism-outlet-upper" />
            <span className="landing-prism-fleck landing-prism-outlet landing-prism-outlet-upper-inner" />
            <span className="landing-prism-fleck landing-prism-outlet landing-prism-outlet-lower-inner" />
            <span className="landing-prism-fleck landing-prism-outlet landing-prism-outlet-lower" />
          </div>
        </div>
        <button
          className="landing-prism-toggle"
          type="button"
          aria-label={prismAction}
          title={prismAction}
          onClick={() => setPrismPaused((paused) => !paused)}
        >
          <PrismIcon size={17} strokeWidth={1.85} aria-hidden />
        </button>
        <Container className="landing-hero-content">
          <p className="landing-kicker">The open-source AI gateway</p>
          <h1 id="landing-title">The right AI for <span>what you need.</span></h1>
          <p className="landing-hero-description">
            One integration for text, images, video, audio and 3D. Route requests across connected models and providers using your workload and routing policy.
          </p>
          <div className="landing-actions">
            <Link to={start} className="landing-button landing-button-primary"><KeyRound size={18} aria-hidden /> Get API key</Link>
            <Link to="/models" className="landing-button landing-button-secondary">Explore models</Link>
          </div>
        </Container>
      </section>

      <section id="how-routing-works" className="landing-section" aria-labelledby="landing-routing-title">
        <Container>
          <div className="landing-section-header landing-routing-intro landing-reveal">
            <div>
              <p className="landing-kicker">How routing works</p>
              <h2 id="landing-routing-title" className="landing-heading">Connect once. Reach many providers.</h2>
            </div>
            <p className="landing-lede">Send requests through one API. TokenCraft checks which routes fit the workload and applies your routing policy. Use a favorite model or let TokenCraft choose where automatic model routing is supported.</p>
          </div>
          <RoutingExplorer />
        </Container>
      </section>

      <section className="landing-section landing-catalog" aria-labelledby="landing-catalog-title">
        <Container>
          <div className="landing-section-header landing-reveal">
            <div>
              <p className="landing-kicker">Model catalog</p>
              <h2 id="landing-catalog-title" className="landing-heading">Many models. One place.</h2>
              <p className="landing-lede">Browse by what you’re making, then compare model capabilities and recorded outputs.</p>
            </div>
            <Link to="/models" className="landing-button landing-button-outline">Browse all models</Link>
          </div>
          <ul className="landing-catalog-grid landing-reveal">
            {catalogLinks.map(({ icon: Icon, title, body, to }) => (
              <li key={title}>
                <Link to={to}>
                  <span className="landing-icon"><Icon size={21} strokeWidth={1.65} aria-hidden /></span>
                  <div><h3>{title}</h3><p>{body}</p></div>
                </Link>
              </li>
            ))}
          </ul>
        </Container>
      </section>

      <section className="landing-open-source" aria-labelledby="landing-open-source-title">
        <Container>
          <div className="landing-open-source-panel landing-reveal">
            <span className="landing-icon"><Github size={25} strokeWidth={1.65} aria-hidden /></span>
            <div>
              <h2 id="landing-open-source-title">The best part? It’s open source.</h2>
              <p>Read the gateway code, see how it routes requests, and run TokenCraft on your own infrastructure.</p>
            </div>
            <a className="landing-button landing-button-outline" href="https://github.com/storytold/tokencraft">View on GitHub</a>
          </div>
        </Container>
      </section>

      <section className="landing-section landing-platform" aria-labelledby="landing-platform-title">
        <Container>
          <div className="landing-reveal">
            <p className="landing-kicker">For developers</p>
            <h2 id="landing-platform-title" className="landing-heading">Built for real workflows.</h2>
          </div>
          <ul className="landing-feature-grid landing-reveal">
            {features.map(({ icon: Icon, title, body }) => (
              <li key={title}>
                <Icon size={23} strokeWidth={1.65} aria-hidden />
                <h3>{title}</h3>
                <p>{body}</p>
              </li>
            ))}
          </ul>
          <div className="landing-cta landing-reveal">
            <div>
              <h3>Put TokenCraft to work.</h3>
              <p>Get an API key for your app or connect the agent you already use.</p>
            </div>
            <div className="landing-actions">
              <Link to={start} className="landing-button landing-button-solid"><KeyRound size={17} aria-hidden /> Get API key</Link>
              <Link to={agent} className="landing-button landing-button-outline"><Terminal size={18} aria-hidden /> Use your agent</Link>
            </div>
          </div>
        </Container>
      </section>
    </div>
  );
}

function RoutingExplorer() {
  const [example, setExample] = useState(routingExamples[0]);
  const [requireQuality, setRequireQuality] = useState(false);
  const RequestIcon = example.icon;
  const checkRating = example.supportsQuality && requireQuality;
  const routes = example.routes.map((route) => ({
    ...route,
    excluded: route.unavailable ?? (checkRating && (route.rating ?? 0) < minimumRating ? "Below quality bar" : undefined),
  }));
  const selected = routes.find((route) => !route.excluded);

  return (
    <div className="landing-explorer landing-reveal" role="group" aria-labelledby="landing-explorer-title">
      <div className="landing-explorer-header">
        <div>
          <h3 id="landing-explorer-title"><GitBranch size={18} aria-hidden /> Routing explorer</h3>
          <p>Illustrative provider routes</p>
        </div>
        <div className="landing-explorer-controls" role="group" aria-label="Choose an example workload">
          {routingExamples.map((option) => {
            const OptionIcon = option.icon;
            return (
              <button key={option.label} type="button" aria-pressed={example === option} aria-controls="landing-routing-example" onClick={() => setExample(option)}>
                <OptionIcon size={16} aria-hidden /> {option.label}
              </button>
            );
          })}
        </div>
      </div>

      <div id="landing-routing-example">
        <div className="landing-route-flow" key={example.label}>
          <div className="landing-routing-journey">
            <div className="landing-request">
              <span className="landing-icon"><RequestIcon size={21} strokeWidth={1.7} aria-hidden /></span>
              <div><p>Your request</p><strong>{example.request}</strong><p>{example.settings}</p></div>
            </div>
            <div className="landing-request landing-gateway-node">
              <img src="/brand/tokencraft-mark.png" width={42} height={42} alt="" loading="lazy" />
              <div><strong>TokenCraft</strong><p>One API across providers</p></div>
            </div>
          </div>
          <div className="landing-route-candidates">
            <p className="landing-offers-label">Model and provider options</p>
            <ul className="landing-offers">
              {routes.map((route) => (
                <li key={route.provider} className={route === selected ? "landing-offer-selected" : undefined} data-excluded={Boolean(route.excluded)}>
                  <div>
                    <strong>{route.model}</strong>
                    <span>{route.provider}{checkRating && route.rating !== undefined && ` · ${route.rating}/10`}</span>
                  </div>
                  <span className="landing-offer-result">{route.excluded ?? (route === selected ? "Selected" : "Eligible")}</span>
                </li>
              ))}
            </ul>
          </div>
        </div>
        <div className="landing-route-total" role="status" aria-atomic="true">
          <Check size={20} aria-hidden />
          {selected ? (
            <div><strong>Selected route</strong><p>{selected.model} · {selected.provider}</p></div>
          ) : <strong>No eligible route for this request</strong>}
        </div>
      </div>
      {example.supportsQuality && (
        <details className="landing-route-quality">
          <summary><ChevronDown size={16} aria-hidden /> Optional quality bar{requireQuality && <span>{minimumRating}/10 active</span>}</summary>
          <p>Where supported, automatic model routing can use a minimum community rating. Try it here to see which route qualifies at {minimumRating}/10 or higher.</p>
          <label>
            <input type="checkbox" checked={requireQuality} onChange={(event) => setRequireQuality(event.currentTarget.checked)} aria-controls="landing-routing-example" />
            Require a rating of at least {minimumRating}/10
          </label>
        </details>
      )}
    </div>
  );
}
