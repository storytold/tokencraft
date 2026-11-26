const el = (id) => document.getElementById(id);
const format = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
const clock = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "medium" });
const time = (seconds) => seconds == null ? "—" : clock.format(seconds * 1000);
const value = (number, suffix = "") => number == null ? "—" : format.format(number) + suffix;
const seconds = (number) => value(number, " s");
const text = (input) => String(input).replaceAll("_", " ");
let snapshot;
let incidentList = [];
let timer;
let historyReload;
let historyData;
let lastView;
let currentInterval = 30;
let activeView = "compute";
let visibleTargets = [];
let hours = 1;
let autoRefresh = true;
let chartLayout;
let chartIndex = null;

const viewTitles = {
  compute: "Compute overview", queues: "Request queues", workflows: "Workflows",
  delivery: "Delivery", incidents: "Incidents",
};
const healthLabels = { up: "Healthy", degraded: "Degraded", down: "Down", unknown: "Unknown" };

function node(tag, content, className) {
  const element = document.createElement(tag);
  if (content != null) element.textContent = content;
  if (className) element.className = className;
  return element;
}

function status(kind) {
  return node("span", healthLabels[kind] ?? text(kind), "status " + kind);
}

function displayTable(id, columns, rows, empty) {
  const parent = el(id);
  if (!rows.length) {
    parent.replaceChildren(node("p", empty, "empty"));
    return;
  }
  const table = node("table");
  table.setAttribute("aria-label", id.replaceAll("-", " "));
  const head = table.createTHead().insertRow();
  for (const column of columns) {
    const cell = node("th", column);
    cell.scope = "col";
    head.append(cell);
  }
  const body = table.createTBody();
  for (const entries of rows) {
    const row = body.insertRow();
    for (const entry of entries) row.insertCell().append(entry);
  }
  const scroll = parent.scrollLeft;
  parent.replaceChildren(table);
  parent.scrollLeft = scroll;
}

function tiles(id, entries) {
  el(id).replaceChildren(...entries.map(([title, result, detail, tone]) => {
    const tile = node("div", null, "tile");
    if (tone) tile.dataset.tone = tone;
    tile.append(node("span", title), node("strong", result));
    if (detail) tile.append(node("small", detail));
    return tile;
  }));
}

function showView(fromNavigation = false) {
  const view = location.hash.slice(1);
  if (view && !Object.hasOwn(viewTitles, view)) return;
  activeView = view || "compute";
  for (const section of document.querySelectorAll(".view")) section.hidden = section.id !== activeView;
  for (const link of document.querySelectorAll("nav a")) {
    if (link.hash === "#" + activeView) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  }
  el("page-title").textContent = viewTitles[activeView];
  if (activeView === "compute") loadHistory();
  else historyReload?.abort();
  if (fromNavigation) {
    window.scrollTo(0, 0);
    el("page-title").focus({ preventScroll: true });
  }
}

function setOptions(select, choices, fallback) {
  const selected = select.value;
  const unchanged = select.options.length === choices.length && choices.every(
    ([key, label], index) => select.options[index].value === key && select.options[index].text === label);
  if (!unchanged) select.replaceChildren(...choices.map(([key, label]) => new Option(label, key)));
  select.value = choices.some(([key]) => key === selected) ? selected : fallback;
}

function graphTarget(id) {
  const series = el("series");
  const option = Array.from(series.options).find(item => item.dataset.seriesKey === JSON.stringify(["probe_latency", id, ""]));
  if (!option) return;
  series.value = option.value;
  loadHistory();
  series.focus({ preventScroll: true });
  el("history").scrollIntoView({ block: "start" });
}

function targetLink(id) {
  const link = node("button", id, "target-link");
  link.type = "button";
  link.title = "View probe history";
  link.addEventListener("click", () => graphTarget(id));
  return link;
}

function displayTargets() {
  const query = el("target-search").value.trim().toLowerCase();
  const filter = el("target-status").value;
  const targets = visibleTargets.filter(row =>
    (!filter || row.status === filter) &&
    (!query || [row.id, row.provider, row.kind, row.stack, row.target, row.cloud, row.region, row.zone]
      .some(field => field?.toLowerCase().includes(query))));
  el("target-count").textContent = targets.length === visibleTargets.length
    ? format.format(targets.length) + (targets.length === 1 ? " configured target" : " configured targets")
    : format.format(targets.length) + " of " + format.format(visibleTargets.length) + " targets";
  displayTable("targets", ["Target", "Role", "Provider", "Route", "Location", "State", "Probe", "Capacity and checks"],
    targets.map(row => {
      const signals = row.signals;
      const parts = Object.entries(signals.checks ?? {}).map(([name, check]) => text(name) + " " + text(check));
      if (signals.sse_capacity != null) parts.push("streams " + signals.sse_readers + " / " + signals.sse_capacity);
      if (signals.provider_capacity != null) parts.push("tasks " + signals.provider_tasks + " / " + signals.provider_capacity);
      if (signals.expected_models != null)
        parts.push((signals.expected_models - signals.missing_models) + " / " + signals.expected_models + " models");
      if (signals.gpu_count != null) {
        parts.push(value(signals.gpu_count) + " GPUs", value(signals.gpu_utilization_percent, "%") + " utilization");
        if (signals.gpu_memory_used_mib != null && signals.gpu_memory_free_mib != null)
          parts.push(value(signals.gpu_memory_used_mib) + " / "
            + value(signals.gpu_memory_used_mib + signals.gpu_memory_free_mib) + " MiB");
        if (signals.gpu_temperature_celsius != null) parts.push(value(signals.gpu_temperature_celsius, " °C"));
      }
      if (row.reason) parts.push(text(row.reason));
      const location = [...new Set([row.cloud, row.region, row.zone].filter(Boolean))].join(" / ");
      const route = row.stack ? row.stack + " / " + row.target : "—";
      return [targetLink(row.id), text(row.kind), row.provider ?? "—", route, location || "—",
        status(row.status), value(row.latency_ms, " ms"), parts.join(" · ") || "—"];
    }), visibleTargets.length ? "No targets match these filters." : "No probes are configured for this cell.");
}

function displayIncidents() {
  const filter = el("incident-filter").value;
  const entries = incidentList.filter(row => !filter || filter === (row.resolved_at == null ? "open" : "resolved"));
  displayTable("incident-table", ["Alert", "Status", "Opened", "Resolved"], entries.map(row => [
    row.title, status(row.resolved_at == null ? "open" : "resolved"), time(row.opened_at), time(row.resolved_at),
  ]), filter ? "No " + filter + " incidents." : "No incidents in this monitor's history.");
}

function displayAttention(targets) {
  const counts = { up: 0, degraded: 0, down: 0, unknown: 0 };
  for (const row of targets) counts[row.status]++;
  el("health-breakdown").replaceChildren(...Object.entries(counts).map(([kind, count]) => {
    const item = node("div", null, "health-stat " + kind);
    item.append(node("span", null, "health-dot"), node("span", healthLabels[kind]), node("strong", value(count)));
    return item;
  }));
  const open = incidentList.filter(row => row.resolved_at == null);
  const entries = open.slice(0, 3).map(row => {
    const item = node("li");
    item.append(node("span", row.title), status("open"));
    return item;
  });
  if (!entries.length) entries.push(node("li", "No open incidents"));
  for (const target of targets.filter(row => row.status === "down" || row.status === "degraded").slice(0, 2)) {
    const item = node("li");
    item.append(targetLink(target.id), status(target.status));
    entries.push(item);
  }
  el("attention-list").replaceChildren(...entries);
  el("nav-incidents").hidden = open.length === 0;
  el("nav-incidents").textContent = value(open.length);
}

function display() {
  const cell = el("cell").value;
  const state = snapshot.cells[cell];
  if (!state) return;
  const targets = Object.values(snapshot.targets).filter(item => item.cell === cell)
    .sort((a, b) => a.id.localeCompare(b.id));
  const scopeNames = Object.keys(state.scopes).sort();
  setOptions(el("scope"), [["", "Whole cell"], ...scopeNames.map(name => [name, name])], "");
  el("scope").disabled = !scopeNames.length;
  const scope = el("scope").value;
  el("page-context").textContent = cell + (scope ? " / " + scope : "");
  const observed = scope ? state.scopes[scope] : state;
  const sample = observed.sample;
  const missing = state.status === "not_configured" ? "PostgreSQL sampling is disabled for this cell."
    : "PostgreSQL observations are unavailable.";
  const ready = targets.filter(({ status }) => status === "up").length;

  tiles("summary", [
    ["Healthy probes", ready + " / " + targets.length, "Configured targets", ready === targets.length && ready ? "good" : ""],
    ["Ready to dispatch", value(sample?.eligible), "Request queue"],
    ["Oldest eligible job", seconds(sample?.oldest_eligible_seconds), sample?.eligible === 0 ? "No work waiting" : "Time in queue"],
    ["Spare DB connections", value(sample?.database_connections_available), "After connection reserves",
      sample?.database_connections_available === 0 ? "alert" : ""],
  ]);
  visibleTargets = targets;
  displayTargets();
  displayAttention(targets);
  displayIncidents();
  displayTable("plugin-traffic", ["Plugin", "Provider", "Operation", "In flight", "Responses/s", "Succeeded",
    "Throttled", "Client errors", "Server errors", "Interrupted", "Successful p95"],
  targets.flatMap((target) => Object.entries(target.plugin?.counters ?? {}).map(([operation, counts]) => {
    const activity = target.plugin.operations[operation];
    const responses = activity?.responses;
    const p95 = activity?.p95_above_max ? "> 120 s" : value(activity?.p95_ms, " ms");
    return [target.id, target.provider, operation, value(counts.in_flight), value(activity?.responses_per_second),
      value(responses?.ok), value(responses?.throttled), value(responses?.client_error),
      value(responses?.server_error), value(responses?.interrupted), p95];
  })), "No provider plugin has reported traffic telemetry.");
  displayTable("providers", ["Provider", "Admitted", "Succeeded", "Failed", "Unconfirmed", "Queue p95", "Provider p95", "Result p95"],
    sample ? state.providers.map((id) => {
      const row = sample.provider[id] ?? {};
      return [id, value(row.admitted ?? 0), value(row.succeeded ?? 0), value(row.failed ?? 0), value(row.uncertain ?? 0),
        seconds(row.p95_queue_seconds), seconds(row.p95_provider_seconds), seconds(row.p95_total_seconds)];
    }) : [], missing);
  displayTable("attempts", ["Target", "Provider", "Attempts", "Response started", "Rate limited", "Connection failed",
    "Upstream error", "Unconfirmed", "Unobserved", "Deadline before send", "Response p95"],
  sample ? targets.filter((row) => row.kind === "compute").map((target) => {
    const row = sample.compute[target.id] ?? {};
    return [target.id, target.provider, value(row.attempted ?? 0), value(row.response_started ?? 0),
      value(row.rate_limited ?? 0), value(row.connection_failed ?? 0), value(row.upstream_error ?? 0),
      value(row.transport_unconfirmed ?? 0), value(row.pending_observation ?? 0), value(row.deadline_before_send ?? 0),
      seconds(row.p95_response_headers_seconds)];
  }) : [], sample ? "No compute targets are mapped to this cell." : missing);
  el("queue-coverage").textContent = state.queue_coverage === "fresh" ? "Eligibility uses current worker bindings"
    : state.queue_coverage === "cached" ? "Eligibility uses last observed worker bindings"
      : "Worker bindings unavailable for queue eligibility";
  const labels = [];
  if (sample?.truncated) labels.push("Traffic sample capped at 10,001 recent gateway records.");
  if (sample?.request_queues_capped) labels.push("Active request sample capped at 5,001.");
  if (sample?.workflow_queues_capped) labels.push("Active workflow sample capped at 5,001.");
  if (sample?.workflow_outcomes?.sampled >= 5001) labels.push("Recent workflow outcome sample capped at 5,001.");
  if (sample?.delivery_queues.some((row) => row.count >= 5001)) labels.push("Delivery counts at 5,001 reached the sample cap.");
  el("notice").textContent = labels.join(" ");
  el("notice").hidden = !labels.length;
  displayTable("request-queues", ["Provider", "Workload", "State", "Count", "Paused", "Oldest sampled"],
    (sample?.request_queues ?? []).map((row) => [row.provider, row.workload, text(row.state), value(row.count),
      value(row.paused), seconds(row.oldest_seconds)]), sample ? "No active requests." : missing);
  displayTable("workflow-queues", ["State", "Count", "Ready to claim", "Past deadline", "Oldest sampled"],
    (sample?.workflow_queues ?? []).map((row) => [text(row.status), value(row.count), value(row.due),
      value(row.overdue), seconds(row.oldest_seconds)]), sample ? "No active workflows." : missing);
  const outcomes = sample?.workflow_outcomes;
  tiles("workflow-outcomes", outcomes ? [["Succeeded", value(outcomes.succeeded)], ["Failed", value(outcomes.failed)],
    ["Canceled", value(outcomes.canceled)], ["Expired", value(outcomes.expired)],
    ["Successful run p95", seconds(outcomes.p95_succeeded_seconds)]] : []);
  displayTable("publication", ["Provider", "Workload", "Observed", "Published", "Pending", "Unconfirmed", "Oldest pending", "Publish p95"],
    (sample?.publication ?? []).map((row) => [row.provider, row.workload, value(row.observed), value(row.published),
      value(row.pending), value(row.unconfirmed), seconds(row.oldest_pending_seconds), seconds(row.p95_seconds)]),
    sample ? "No provider results observed in this window." : missing);
  displayTable("delivery-queues", ["Queue", "Count", "Oldest sampled"],
    (sample?.delivery_queues ?? []).map((row) => [text(row.kind), value(row.count), seconds(row.oldest_seconds)]), missing);
  const choices = targets.flatMap((target) => [
    ["probe_latency", target.id, "", target.id + " · Probe latency", "ms"],
    ...(target.kind === "gpu" ? [["gpu_utilization", target.id, "", target.id + " · GPU utilization", "%"]] : []),
    ...(target.kind === "compute" && state.status !== "not_configured"
      ? [["target_p95", cell, target.id, target.id + " · Response p95", "s"]] : []),
    ...(target.plugin_operations ?? []).flatMap((operation) => [
      ["plugin_p95", target.id, operation, target.id + " · " + operation + " p95", "ms"],
      ["plugin_throughput", target.id, operation, target.id + " · " + operation + " responses", "/s"],
    ]),
  ]);
  if (state.status !== "not_configured") {
    choices.push(["queue_age", cell, "", "Oldest eligible job", "s"], ["workflow_p95", cell, "", "Workflow p95", "s"]);
    for (const provider of state.providers) choices.push(["request_p95", cell, provider, provider + " · Result p95", "s"]);
  }
  const series = el("series");
  const selected = series.selectedOptions[0]?.dataset.seriesKey;
  const options = choices.map(([metric, id, subject, label, unit]) => {
    const targetMetric = ["probe_latency", "gpu_utilization", "plugin_p95", "plugin_throughput"].includes(metric);
    const option = new Option(label, JSON.stringify({ metric, id, subject, unit, scope: targetMetric ? "" : scope }));
    option.dataset.seriesKey = JSON.stringify([metric, id, subject]);
    return option;
  });
  const provider = state.providers.find(id => sample?.provider[id]?.p95_total_seconds != null);
  const compute = targets.find(target => target.kind === "compute" && target.status === "up");
  const preferred = provider ? ["request_p95", cell, provider] : ["probe_latency", compute?.id, ""];
  const next = options.find(option => option.dataset.seriesKey === selected) ??
    options.find(option => option.dataset.seriesKey === JSON.stringify(preferred));
  if (next) next.selected = true;
  series.replaceChildren(...options);
  series.disabled = !choices.length;
  if (activeView === "compute") loadHistory();
}

async function api(path, options) {
  const response = await fetch(path, { cache: "no-store", ...options });
  if (!response.ok) throw new Error("The operator endpoint returned " + response.status + ".");
  return response.json();
}

function collectorStatus(state, label, observedAt) {
  const badge = el("freshness");
  badge.dataset.state = state;
  if (badge.textContent !== label) badge.textContent = label;
  if (state !== "error") badge.removeAttribute("title");
  const sampled = el("sample-time");
  sampled.hidden = observedAt == null;
  if (observedAt != null) {
    sampled.dateTime = new Date(observedAt * 1000).toISOString();
    sampled.textContent = time(observedAt);
  }
}

function scheduleRefresh() {
  clearTimeout(timer);
  if (autoRefresh) timer = setTimeout(refresh, Math.min(currentInterval, 30) * 1000);
}

async function refresh() {
  if (el("refresh").disabled) return;
  clearTimeout(timer);
  el("refresh").disabled = true;
  try {
    const latest = await api("/api/overview");
    currentInterval = latest.interval_seconds;
    const observedAt = latest.snapshot?.observed_at;
    collectorStatus(observedAt == null ? "waiting" : latest.stale ? "stale" : "live",
      observedAt == null ? "Waiting for first sample" : latest.stale ? "Stale sample" : "Collector live",
      observedAt ?? snapshot?.observed_at);
    if (latest.snapshot && observedAt !== snapshot?.observed_at) {
      snapshot = latest.snapshot;
      incidentList = latest.incidents;
      const ids = Object.keys(snapshot.cells).sort();
      el("cell").disabled = false;
      setOptions(el("cell"), ids.map(name => [name, name]), ids[0]);
      display();
    }
  } catch (failure) {
    collectorStatus("error", "Refresh failed", snapshot?.observed_at);
    el("freshness").title = failure.message;
  } finally {
    el("refresh").disabled = false;
    scheduleRefresh();
  }
}

async function loadHistory() {
  historyReload?.abort();
  const choice = el("series").value;
  if (!choice) {
    el("history-summary").textContent = "No time series are configured.";
    historyData = null;
    chartIndex = null;
    draw();
    return;
  }
  const request = new AbortController();
  historyReload = request;
  const { metric, id, subject, unit, scope } = JSON.parse(choice);
  const query = new URLSearchParams({ metric, id, hours });
  if (subject) query.set("subject", subject);
  if (scope) query.set("scope", scope);
  const view = choice + hours;
  if (view !== lastView) {
    historyData = null;
    chartIndex = null;
    draw();
    el("history-summary").textContent = "Loading history…";
  }
  lastView = view;
  try {
    const data = await api("/api/history?" + query, { signal: request.signal });
    if (request.signal.aborted) return;
    const inspected = historyData?.points.filter(point => point.value != null)[chartIndex]?.observed_at;
    historyData = { ...data, unit, hours, until: Date.now() / 1000 };
    const observed = data.points.filter((point) => point.value != null);
    const priorIndex = observed.findIndex(point => point.observed_at === inspected);
    chartIndex = priorIndex < 0 ? null : priorIndex;
    const latest = observed.at(-1);
    const availability = data.availability;
    const checks = availability ? availability.up + availability.degraded + availability.down : 0;
    const uptime = checks ? " · " + value(100 * availability.up / checks, "%") + " healthy across " + checks + " known probes" : "";
    const unknown = availability?.unknown ? " · " + availability.unknown + " unknown" : "";
    el("history-summary").textContent = (latest
      ? value(latest.value, " " + unit) + " latest · " + observed.length + " plotted samples"
      : "No measurements in this range") + uptime + unknown;
    draw();
  } catch (failure) {
    if (!request.signal.aborted) el("history-summary").textContent = failure.message;
  }
}

function draw() {
  const canvas = el("chart");
  const width = canvas.clientWidth;
  const height = canvas.clientHeight;
  if (!width || !height) return;
  const density = window.devicePixelRatio || 1;
  canvas.width = Math.round(width * density);
  canvas.height = Math.round(height * density);
  const output = el("chart-point");
  output.textContent = "";
  chartLayout = null;
  const points = historyData?.points.filter(point => point.value != null) ?? [];
  el("chart-empty").hidden = points.length > 0;
  if (!points.length) return;

  const ctx = canvas.getContext("2d");
  ctx.scale(density, density);
  const theme = getComputedStyle(document.documentElement);
  const brand = theme.getPropertyValue("--brand").trim();
  const line = theme.getPropertyValue("--line");
  const muted = theme.getPropertyValue("--muted");
  const end = historyData.until;
  const span = historyData.hours * 3600;
  const maximum = Math.max(...points.map(point => point.value)) || 1;
  const left = 42, right = width - 10, top = 12, bottom = height - 23;
  const x = point => left + (right - left) * Math.min(1, Math.max(0, (point.observed_at - end + span) / span));
  const y = point => bottom - (bottom - top) * point.value / maximum;
  chartLayout = { points, x };

  ctx.font = "10px ui-monospace, monospace";
  ctx.strokeStyle = line;
  ctx.fillStyle = muted;
  for (let step = 0; step <= 4; step++) {
    const row = bottom - (bottom - top) * step / 4;
    ctx.beginPath();
    ctx.moveTo(left, row);
    ctx.lineTo(right, row);
    ctx.stroke();
    ctx.fillText(value(maximum * step / 4), 2, row + 3);
  }
  ctx.fillText(historyData.hours === 168 ? "7d ago" : historyData.hours + "h ago", left, height - 4);
  ctx.textAlign = "right";
  ctx.fillText("Now", right, height - 4);
  ctx.textAlign = "start";

  const segments = [];
  let segment = [];
  const gap = Math.max(currentInterval, historyData.hours * 5) * 3;
  for (const point of historyData.points) {
    if (point.value == null || (segment.length && point.observed_at - segment.at(-1).observed_at > gap)) {
      if (segment.length) segments.push(segment);
      segment = [];
    }
    if (point.value != null) segment.push(point);
  }
  if (segment.length) segments.push(segment);
  const fill = ctx.createLinearGradient(0, top, 0, bottom);
  fill.addColorStop(0, brand + "31");
  fill.addColorStop(1, brand + "03");
  ctx.lineWidth = 2;
  ctx.strokeStyle = brand;
  for (const group of segments) {
    const first = group[0], last = group.at(-1);
    if (group.length === 1) {
      ctx.fillStyle = brand;
      ctx.fillRect(x(first) - 2, y(first) - 2, 4, 4);
      continue;
    }
    ctx.beginPath();
    ctx.moveTo(x(first), y(first));
    for (const point of group.slice(1)) ctx.lineTo(x(point), y(point));
    ctx.stroke();
    ctx.lineTo(x(last), bottom);
    ctx.lineTo(x(first), bottom);
    ctx.closePath();
    ctx.fillStyle = fill;
    ctx.fill();
  }
  if (chartIndex != null) {
    const point = points[chartIndex];
    if (!point) return;
    ctx.beginPath();
    ctx.setLineDash([3, 3]);
    ctx.moveTo(x(point), top);
    ctx.lineTo(x(point), bottom);
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.beginPath();
    ctx.arc(x(point), y(point), 4, 0, Math.PI * 2);
    ctx.fillStyle = brand;
    ctx.fill();
    output.textContent = time(point.observed_at) + " · " + value(point.value, " " + historyData.unit);
  }
}

function selectSample(index, announce = false) {
  if (chartIndex === index) return;
  el("chart-point").setAttribute("aria-live", announce ? "polite" : "off");
  chartIndex = index;
  draw();
}

function toggleTheme() {
  applyTheme(document.documentElement.dataset.theme === "dark" ? "light" : "dark");
}

function applyTheme(theme) {
  document.documentElement.dataset.theme = theme;
  el("theme").textContent = theme === "dark" ? "Light mode" : "Dark mode";
  el("theme").setAttribute("aria-label", "Switch to " + el("theme").textContent.toLowerCase());
  try { localStorage.setItem("theme:operations", theme); } catch { /* theme remains active for this session */ }
  draw();
}

let theme;
try { theme = localStorage.getItem("theme:operations"); } catch { /* use the system preference */ }
applyTheme(theme === "dark" || theme === "light" ? theme : matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");

el("cell").addEventListener("change", () => {
  el("scope").value = "";
  display();
});
el("scope").addEventListener("change", display);
el("series").addEventListener("change", loadHistory);
el("target-search").addEventListener("input", displayTargets);
el("target-status").addEventListener("change", displayTargets);
el("incident-filter").addEventListener("change", displayIncidents);
el("refresh").addEventListener("click", refresh);
el("theme").addEventListener("click", toggleTheme);
el("auto-refresh").addEventListener("click", event => {
  autoRefresh = !autoRefresh;
  event.currentTarget.setAttribute("aria-pressed", String(autoRefresh));
  event.currentTarget.textContent = autoRefresh ? "Auto-refresh" : "Refresh paused";
  event.currentTarget.title = autoRefresh ? "Pause automatic refresh" : "Resume automatic refresh";
  if (autoRefresh) refresh();
  else clearTimeout(timer);
});
for (const button of document.querySelectorAll("[data-hours]")) {
  button.addEventListener("click", () => {
    hours = Number(button.dataset.hours);
    for (const choice of button.parentElement.children) choice.setAttribute("aria-pressed", String(choice === button));
    loadHistory();
  });
}
const chart = el("chart");
chart.addEventListener("pointermove", event => {
  if (!chartLayout) return;
  const { points, x } = chartLayout;
  const position = event.clientX - chart.getBoundingClientRect().left;
  const nearest = points.reduce((best, point, index) =>
    Math.abs(x(point) - position) < Math.abs(x(points[best]) - position) ? index : best, 0);
  selectSample(nearest);
});
chart.addEventListener("pointerleave", () => selectSample(null));
chart.addEventListener("keydown", event => {
  if (!chartLayout || !["ArrowLeft", "ArrowRight", "Home", "End", "Escape"].includes(event.key)) return;
  event.preventDefault();
  const last = chartLayout.points.length - 1;
  const index = chartIndex ?? last;
  const next = { ArrowLeft: Math.max(0, index - 1), ArrowRight: Math.min(last, index + 1),
    Home: 0, End: last, Escape: null };
  selectSample(next[event.key], true);
});
window.addEventListener("hashchange", () => showView(true));
new ResizeObserver(draw).observe(chart);
showView();
refresh();
