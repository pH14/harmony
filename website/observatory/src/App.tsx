import { useEffect, useMemo, useRef, useState } from "react";

type Metric = "selections" | "work" | "positions" | "new_places" | "territory";
type Cell = { area: number; map_x: number; map_y: number; value: number };
type Run = { run_id: string; started_unix_ms: number; latest_ms: number; selections: number; ended: number };
type Completeness = { complete: boolean; lost_events: number; sequence_gaps: number; qualification: string };
type Status = {
  status: string; telemetry: { latest_ms: number; selections: number; skipped: number; admissions: number; execution_work: number };
  finalized_through_ms: number; completeness: Completeness;
};
type MapData = { cells: Cell[]; unit: string; provisional: boolean; completeness: Completeness };
type Timeline = { points: Array<{ bucket_ms: number; selections: number; new_places: number }>; resolution_ms: number };
type Observation = { event_ms: number; event_id: number; admission_sequence: number; area: number; map_x: number; map_y: number; health: number; missiles: number; equipment: number; outcome: string };
type ObservationResult = { matching_sampled_observations: number; examples: Observation[]; next: string | null; interpretation: string; completeness: Completeness };
type Filters = { area: string; map_x: string; map_y: string; min_health: string; min_missiles: string; equipment_bits: string; outcome: string };

const areas = [
  { id: 16, name: "Brinstar" }, { id: 17, name: "Norfair" }, { id: 18, name: "Kraid" },
  { id: 19, name: "Tourian" }, { id: 20, name: "Ridley" }
];
const metrics: Array<{ id: Metric; name: string; description: string; max: number }> = [
  { id: "selections", name: "Selections", description: "Parent states selected here, including skipped duplicates", max: 128 },
  { id: "work", name: "Execution work", description: "Emulated frames charged to selections here", max: 100000 },
  { id: "positions", name: "Observed positions", description: "Valid gameplay observations admitted in this interval", max: 1024 },
  { id: "new_places", name: "New places", description: "Map cells first observed in this interval", max: 1 },
  { id: "territory", name: "Observed territory", description: "Distinct map cells known by this point", max: 1 }
];
const windows = [
  { name: "10 sec", value: 10000 }, { name: "30 sec", value: 30000 }, { name: "2 min", value: 120000 },
  { name: "10 min", value: 600000 }, { name: "1 hour", value: 3600000 }
];
const emptyFilters: Filters = { area: "", map_x: "", map_y: "", min_health: "", min_missiles: "", equipment_bits: "", outcome: "" };

function n(value: unknown): number { return Number(value || 0); }
function display(value: unknown): string { return n(value).toLocaleString(); }
function elapsed(ms: number): string {
  if (ms < 10000) return (Math.max(0, ms) / 1000).toFixed(1) + "s";
  const seconds = Math.floor(Math.max(0, ms) / 1000);
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor(seconds % 3600 / 60);
  return (hours ? hours + "h " : "") + minutes + "m " + String(seconds % 60).padStart(2, "0") + "s";
}
function key(area: number, x: number, y: number): string { return area + ":" + x + ":" + y; }
function heat(value: number, maximum: number): string {
  const intensity = Math.min(1, Math.log1p(value) / Math.log1p(maximum));
  return "hsl(" + (209 - intensity * 172) + " " + (78 + intensity * 10) + "% " + (42 + intensity * 19) + "%)";
}
async function read<T>(url: string, signal?: AbortSignal): Promise<T> {
  const response = await fetch(url, { signal, headers: { Accept: "application/json" } });
  const body = await response.json();
  if (!response.ok) throw new Error(body.error || "HTTP " + response.status);
  return body as T;
}

export default function App() {
  const [runs, setRuns] = useState<Run[]>([]);
  const [runId, setRunId] = useState("");
  const [status, setStatus] = useState<Status | null>(null);
  const [metric, setMetric] = useState<Metric>("selections");
  const [area, setArea] = useState(16);
  const [windowMs, setWindowMs] = useState(30000);
  const [cursorMs, setCursorMs] = useState(1);
  const [follow, setFollow] = useState(true);
  const [mapResult, setMapResult] = useState<{ scope: string; values: MapData; footprint: MapData } | null>(null);
  const [timelineResult, setTimelineResult] = useState<{ runId: string; data: Timeline } | null>(null);
  const [selected, setSelected] = useState<{ x: number; y: number } | null>(null);
  const [zoom, setZoom] = useState(1);
  const [filters, setFilters] = useState<Filters>(emptyFilters);
  const [observations, setObservations] = useState<ObservationResult | null>(null);
  const [queriedScope, setQueriedScope] = useState("");
  const queryAbort = useRef<AbortController | null>(null);
  const [queryBusy, setQueryBusy] = useState(false);
  const [queryError, setQueryError] = useState("");
  const [error, setError] = useState("");
  const [mapBusy, setMapBusy] = useState(false);
  const cache = useRef(new Map<string, MapData>());
  const info = metrics.find(item => item.id === metric)!;
  const latest = n(status?.telemetry.latest_ms);
  const at = follow ? latest + 1 : Math.max(1, cursorMs);
  const from = Math.max(0, at - windowMs);
  const areaName = areas.find(item => item.id === area)?.name || "Unknown";
  const finalized = at <= n(status?.finalized_through_ms);
  const currentScope = JSON.stringify([runId, from, at, filters]);
  const currentObservations = queriedScope === currentScope ? observations : null;
  const viewScope = JSON.stringify([runId, metric, area, from, at]);
  const map = mapResult?.scope === viewScope ? mapResult.values : null;
  const territory = mapResult?.scope === viewScope ? mapResult.footprint : null;
  const timeline = timelineResult?.runId === runId ? timelineResult.data : null;

  useEffect(() => {
    let alive = true;
    const load = () => read<{ runs: Run[] }>("/api/v1/runs").then(value => {
      if (!alive) return;
      setRuns(value.runs);
      setError("");
      if (!runId && value.runs.length) setRunId(value.runs[0].run_id);
    }).catch(value => { if (alive) setError(String(value)); });
    void load();
    const timer = window.setInterval(load, 4000);
    return () => { alive = false; window.clearInterval(timer); };
  }, [runId]);

  useEffect(() => {
    if (!runId) return;
    let alive = true;
    setStatus(null); setMapResult(null); setTimelineResult(null); setMapBusy(false); setSelected(null); setObservations(null); setQueriedScope(""); queryAbort.current?.abort(); cache.current.clear();
    const load = () => read<Status>("/api/v1/runs/" + runId + "/status").then(value => {
      if (!alive) return;
      setStatus(value); setError("");
    }).catch(value => { if (alive) setError(String(value)); });
    void load();
    const timer = window.setInterval(load, 2000);
    return () => { alive = false; window.clearInterval(timer); };
  }, [runId]);

  useEffect(() => {
    if (!runId || !status) return;
    const abort = new AbortController();
    const to = Math.max(1, latest + 1);
    const start = Math.max(0, to - 3600000);
    const bucket = Math.max(1000, Math.ceil((to - start) / 180 / 1000) * 1000);
    const params = new URLSearchParams({ from_ms: String(start), to_ms: String(to), bucket_ms: String(bucket) });
    read<Timeline>("/api/v1/runs/" + runId + "/timeline?" + params, abort.signal)
      .then(value => { if (!abort.signal.aborted) setTimelineResult({ runId, data: value }); })
      .catch(value => { if (value.name !== "AbortError") setError(String(value)); });
    return () => abort.abort();
  }, [runId, latest]);

  useEffect(() => {
    if (!runId || !status) return;
    const abort = new AbortController();
    const scope = JSON.stringify([runId, metric, area, from, at]);
    const timer = window.setTimeout(async () => {
      setMapBusy(true);
      const load = async (wanted: Metric) => {
        const id = [runId, wanted, area, from, at].join(":");
        const cached = cache.current.get(id);
        if (cached) return cached;
        const params = new URLSearchParams({ metric: wanted, area: String(area), from_ms: String(from), to_ms: String(at) });
        const result = await read<MapData>("/api/v1/runs/" + runId + "/map?" + params, abort.signal);
        if (!result.provisional) {
          cache.current.set(id, result);
          while (cache.current.size > 24) cache.current.delete(cache.current.keys().next().value!);
        }
        return result;
      };
      try {
        const [values, footprint] = await Promise.all([load(metric), load("territory")]);
        if (!abort.signal.aborted) { setMapResult({ scope, values, footprint }); setError(""); }
      } catch (value) {
        if (!abort.signal.aborted) setError(String(value));
      } finally {
        if (!abort.signal.aborted) setMapBusy(false);
      }
    }, follow ? 80 : 180);
    return () => { abort.abort(); window.clearTimeout(timer); };
  }, [runId, status, area, metric, from, at, follow]);

  const values = useMemo(() => new Map((map?.cells || []).map(cell => [key(cell.area, cell.map_x, cell.map_y), n(cell.value)])), [map]);
  const footprint = useMemo(() => new Set((territory?.cells || []).map(cell => key(cell.area, cell.map_x, cell.map_y))), [territory]);
  const ranked = useMemo(() => (map?.cells || []).filter(cell => cell.area === area)
    .sort((a, b) => n(b.value) - n(a.value)).slice(0, 8), [map, area]);
  const selectedValue = selected ? values.get(key(area, selected.x, selected.y)) || 0 : 0;
  const size = 640 / zoom;
  const focusX = selected ? 24 + selected.x * 18 + 9 : 320;
  const focusY = selected ? 24 + selected.y * 18 + 9 : 320;
  const viewX = Math.max(0, Math.min(640 - size, focusX - size / 2));
  const viewY = Math.max(0, Math.min(640 - size, focusY - size / 2));

  async function search(next: Filters, after?: string) {
    if (!runId) return;
    queryAbort.current?.abort();
    const controller = new AbortController();
    queryAbort.current = controller;
    setQueryBusy(true); setQueryError("");
    const scope = JSON.stringify([runId, from, at, next]);
    const params = new URLSearchParams({ from_ms: String(from), to_ms: String(at), limit: "50" });
    Object.entries(next).forEach(([name, value]) => { if (value) params.set(name, value); });
    if (after) params.set("after", after);
    try {
      const result = await read<ObservationResult>("/api/v1/runs/" + runId + "/observations?" + params, controller.signal);
      if (controller.signal.aborted) return;
      setObservations(previous => after && previous && queriedScope === scope ? { ...result, examples: [...previous.examples, ...result.examples].slice(0, 200) } : result);
      setQueriedScope(scope);
    } catch (value) { if (!controller.signal.aborted) setQueryError(String(value)); }
    finally { if (!controller.signal.aborted) setQueryBusy(false); }
  }
  function example(kind: "door" | "health" | "missiles") {
    const next = kind === "door"
      ? { ...emptyFilters, area: String(area), map_x: selected ? String(selected.x) : "", map_y: selected ? String(selected.y) : "", min_missiles: "5" }
      : kind === "health" ? { ...emptyFilters, min_health: "800" } : { ...emptyFilters, min_missiles: "25" };
    setFilters(next); void search(next);
  }

  return <div className="app-shell">
    <header className="topbar">
      <div className="brand"><span className="brand-mark">H<span>·</span></span><span><strong>HARMONY</strong><small>SEARCH OBSERVATORY</small></span></div>
      <div className="topbar-right"><span className="system-tag">METROID · NATIVE QUICKNES</span><span className="live-indicator"><span />{follow ? "LIVE FOLLOW" : "PAUSED"}</span></div>
    </header>
    <main>
      <section className="intro-row">
        <div><div className="eyebrow">EXPLORATION / TELEMETRY</div><h1>Watch the search unfold.</h1><p>Follow new territory across branches, inspect where work went, and ask what a recorded state held.</p></div>
        <div className="run-picker"><label htmlFor="run-select">ACTIVE RUN</label><select id="run-select" value={runId} onChange={event => { setRunId(event.target.value); setFollow(true); }}>
          {!runs.length && <option value="">No runs available</option>}
          {runs.map(run => <option key={run.run_id} value={run.run_id}>{run.run_id.slice(0, 8)} · {new Date(n(run.started_unix_ms)).toLocaleString()}</option>)}
        </select><span className="run-subline">{runId ? "Run " + runId : "Start a Metroid search to populate the observatory."}</span></div>
      </section>
      {error && <div className="alert error" role="alert">Connection or query issue: {error}. Retrying automatically.</div>}
      {!runId && <div className="empty-state"><div className="empty-symbol">⌁</div><h2>No search runs yet</h2><p>Start Metroid with the observatory runner, then this page will follow its telemetry.</p></div>}
      {runId && <>
        <section className="summary-grid" aria-label="Run summary">
          <div className="summary-card"><span className="summary-label">SEARCH WORK</span><strong>{display(status?.telemetry.execution_work)}</strong><small>emulated frames</small></div>
          <div className="summary-card"><span className="summary-label">PARENT SELECTIONS</span><strong>{display(status?.telemetry.selections)}</strong><small>{display(status?.telemetry.skipped)} skipped · {display(status?.telemetry.admissions)} admitted</small></div>
          <div className="summary-card"><span className="summary-label">OBSERVED TERRITORY</span><strong>{display(territory?.cells.reduce((sum, cell) => sum + n(cell.value), 0))}</strong><small>distinct map cells as of cursor</small></div>
          <div className="summary-card status-card"><span className="summary-label">TELEMETRY STATE</span><strong className="status-value"><span className={status?.status === "completed" ? "status-dot complete" : "status-dot"} />{status?.status === "completed" ? status.completeness.complete ? "Complete" : "Partial" : status?.status === "failed" ? "Failed" : status ? "Following" : "Loading"}</strong><small>{status?.completeness.lost_events || status?.completeness.sequence_gaps ? display(status.completeness.lost_events) + " lost · " + display(status.completeness.sequence_gaps) + " gaps" : "No recorded gaps"} · {status ? elapsed(latest) : "Loading"}</small></div>
        </section>
        <section className="workspace-grid">
          <div className="main-column">
            <article className="panel map-panel">
              <div className="panel-head"><div><div className="eyebrow">SPATIAL EXPLORATION</div><h2>Search map</h2></div><div className="area-switch" aria-label="Area">{areas.map(item => <button key={item.id} className={area === item.id ? "active" : ""} onClick={() => { setArea(item.id); setSelected(null); }}>{item.name}</button>)}</div></div>
              <div className="metric-switch" aria-label="Map metric">{metrics.map(item => <button key={item.id} title={item.description} className={metric === item.id ? "active" : ""} onClick={() => setMetric(item.id)}>{item.name}</button>)}</div>
              <div className="map-meta"><span><strong>{areaName}</strong> · {info.description}</span><span>{elapsed(from)} – {elapsed(at)} · {finalized ? "Finalized" : "Provisional"}</span></div>
              <div className="map-layout">
                <div className="map-frame">
                  <svg role="img" aria-label={areaName + " map of " + info.name + " at " + elapsed(at)} viewBox={[viewX, viewY, size, size].join(" ")}>
                    <rect x="0" y="0" width="640" height="640" fill="#0d1929" />
                    {Array.from({ length: 1024 }, (_, index) => {
                      const x = index % 32; const y = Math.floor(index / 32); const identity = key(area, x, y);
                      const value = values.get(identity) || 0; const known = footprint.has(identity);
                      const active = selected?.x === x && selected.y === y;
                      return <rect key={identity} x={24 + x * 18} y={24 + y * 18} width="16" height="16" rx="2"
                        fill={value ? heat(value, info.max) : known ? "#24405c" : "#142235"}
                        stroke={active ? "#ffffff" : known ? "#365879" : "#1a2c43"} strokeWidth={active ? 2 : 0.7}
                        className="map-cell" tabIndex={known || value ? 0 : -1}
                        onClick={() => setSelected({ x, y })}
                        onKeyDown={event => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); setSelected({ x, y }); } }}
                        aria-label={"Map cell " + x + ", " + y + ": " + display(value) + " " + (map?.unit || info.name)}>
                        <title>{areaName + " · map " + x + ", " + y + " · " + display(value) + " " + (map?.unit || info.name)}</title>
                      </rect>;
                    })}
                  </svg>
                  <div className="map-controls" aria-label="Map zoom"><button onClick={() => setZoom(value => Math.max(1, value / 2))} disabled={zoom === 1} aria-label="Zoom out">−</button><span>{zoom}×</span><button onClick={() => setZoom(value => Math.min(4, value * 2))} disabled={zoom === 4} aria-label="Zoom in">+</button></div>
                  {mapBusy && <div className="map-loading">Updating map…</div>}
                </div>
                <aside className="map-side">
                  <div className="legend-title">INTENSITY <span>fixed scale</span></div><div className="legend-gradient" /><div className="legend-labels"><span>0</span><span>{display(info.max)}+</span></div>
                  <p>Faint blue cells were observed before this window. Bright cells show <strong>{info.name.toLowerCase()}</strong> in the selected view.</p>
                  <div className="selected-card"><div className="eyebrow">SELECTED REGION</div>{selected ? <><strong>{areaName + " / " + selected.x + ", " + selected.y}</strong><span>{display(selectedValue) + " " + (map?.unit || info.name)}</span><button onClick={() => setFilters(previous => ({ ...previous, area: String(area), map_x: String(selected.x), map_y: String(selected.y) }))}>Use in resource query ↗</button></> : <p>Select a map cell for its coordinate and value.</p>}</div>
                  <div className="top-cells"><div className="eyebrow">HIGHEST IN VIEW</div>{ranked.length ? ranked.map((cell, index) => <button key={key(cell.area, cell.map_x, cell.map_y)} onClick={() => setSelected({ x: cell.map_x, y: cell.map_y })}><span>{String(index + 1).padStart(2, "0") + " · " + cell.map_x + ", " + cell.map_y}</span><strong>{display(cell.value)}</strong></button>) : <p>No cells in this interval.</p>}</div>
                </aside>
              </div>
            </article>
            <article className="panel timeline-panel">
              <div className="panel-head"><div><div className="eyebrow">SEARCH HISTORY</div><h2>Timeline</h2></div><div className="time-chip">{follow ? "LIVE" : "HISTORICAL"} · {elapsed(at)}</div></div>
              <div className="timeline-chart" aria-label="Selection activity over time">
                {(timeline?.points || []).map((point, index) => <div key={point.bucket_ms + "-" + index} className="timeline-bar" style={{ height: Math.max(3, n(point.selections) / Math.max(1, ...(timeline?.points || []).map(item => n(item.selections))) * 100) + "%" }} title={elapsed(point.bucket_ms) + ": " + display(point.selections) + " selections, " + display(point.new_places) + " new cells"} />)}
                {!timeline?.points.length && <span className="no-activity">No activity in available history</span>}
              </div>
              <input className="timeline-range" aria-label="Historical point" type="range" min="1" max={Math.max(1, latest + 1)} step="1" value={Math.min(Math.max(1, at), Math.max(1, latest + 1))} onChange={event => { setFollow(false); setCursorMs(Number(event.target.value)); }} />
              <div className="timeline-actions"><button className={follow ? "live-button active" : "live-button"} onClick={() => { setFollow(true); setCursorMs(latest + 1); }}>{follow ? "● Following live" : "↗ Return to live"}</button><div className="window-picker"><label htmlFor="window-size">WINDOW</label><select id="window-size" value={windowMs} onChange={event => setWindowMs(Number(event.target.value))}>{windows.map(item => <option key={item.value} value={item.value}>{item.name}</option>)}</select></div><span className="resolution">Resolution {elapsed(timeline?.resolution_ms || 1000)} · polls every 2 sec</span></div>
            </article>
          </div>
          <div className="side-column">
            <article className="panel query-panel">
              <div className="eyebrow">STRUCTURED EXPLORATION</div><h2>Ask the observations</h2><p>Filters apply to the same recorded state in each sampled action observation.</p>
              <div className="example-buttons"><button onClick={() => example("door")}>Doorway · ≥5 missiles</button><button onClick={() => example("health")}>High health</button><button onClick={() => example("missiles")}>High missiles</button></div>
              <form onSubmit={event => { event.preventDefault(); void search(filters); }}>
                <div className="form-row"><label>Area<select value={filters.area} onChange={event => setFilters({ ...filters, area: event.target.value })}><option value="">Any area</option>{areas.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select></label><label>Outcome<select value={filters.outcome} onChange={event => setFilters({ ...filters, outcome: event.target.value })}><option value="">Any outcome</option><option value="runnable">Runnable</option><option value="terminal">Terminal</option><option value="failed">Failed</option></select></label></div>
                <div className="form-row three"><label>Map X<input type="number" min="0" max="31" placeholder="Any" value={filters.map_x} onChange={event => setFilters({ ...filters, map_x: event.target.value })} /></label><label>Map Y<input type="number" min="0" max="31" placeholder="Any" value={filters.map_y} onChange={event => setFilters({ ...filters, map_y: event.target.value })} /></label><label>Gear bits<input type="number" min="0" max="255" placeholder="e.g. 4" value={filters.equipment_bits} onChange={event => setFilters({ ...filters, equipment_bits: event.target.value })} /></label></div>
                <div className="form-row"><label>Min health<input type="number" min="0" max="6999" placeholder="Any" value={filters.min_health} onChange={event => setFilters({ ...filters, min_health: event.target.value })} /></label><label>Min missiles<input type="number" min="0" max="255" placeholder="Any" value={filters.min_missiles} onChange={event => setFilters({ ...filters, min_missiles: event.target.value })} /></label></div>
                <button className="submit-button" disabled={queryBusy} type="submit">{queryBusy ? "Searching…" : "Search this time window →"}</button>
              </form>
              {queryError && <div className="inline-error" role="alert">{queryError}</div>}
              {observations && !currentObservations && <p className="stale-results">Time or filters changed. Search this interval to refresh the results.</p>}
              {currentObservations && <div className="query-results"><div className="results-heading"><strong>{display(currentObservations.matching_sampled_observations)} matching sampled observations</strong><span>{currentObservations.next ? "More available" : "End of page"}</span></div>
                {currentObservations.examples.length ? <div className="result-list">{currentObservations.examples.map(item => <div className="result" key={item.event_ms + "-" + item.event_id}><div><strong>{(areas.find(known => known.id === item.area)?.name || item.area) + " / " + item.map_x + ", " + item.map_y}</strong><span>#{item.admission_sequence} · {elapsed(item.event_ms)}</span></div><div><b>{item.health}</b> HP <b>{item.missiles}</b> missiles</div><small>gear 0x{Number(item.equipment).toString(16).padStart(2, "0")} · {item.outcome}</small></div>)}</div> : <p className="no-results">No matching sampled observation in this interval.</p>}
                {currentObservations.next && <button className="more-button" disabled={queryBusy} onClick={() => void search(filters, currentObservations.next!)}>Load more examples</button>}
                <p className="sample-note">{currentObservations.interpretation}</p>
              </div>}
            </article>
            <article className="panel table-panel"><div className="eyebrow">INPUT POLICY</div><h2>Draw table</h2><div className="table-empty"><span>∅</span><strong>No empirical table in this run</strong><p>Metroid currently draws controller chords from its fresh alphabet. There is no published empirical distribution or historical table version to inspect. Selection, work, and observed state history remain available.</p></div></article>
            <article className="panel integrity-panel"><div className="eyebrow">DATA INTEGRITY</div><h2>What this view can say</h2><ul><li><strong>Separate branches.</strong> Cells were visited by search branches, not one continuous play.</li><li><strong>Work attribution.</strong> Emulated frames belong to their originating selection window.</li><li><strong>Joint state.</strong> Resource filters use health, missiles, and equipment from one sampled observation.</li><li><strong>Completeness.</strong> {status?.completeness.qualification || "Loading run status."}</li></ul></article>
          </div>
        </section>
      </>}
    </main>
    <footer><span>HARMONY / METROID SEARCH OBSERVATORY</span><span>Telemetry schema v1 · Spatial grid 32 × 32 per area · No ROM imagery</span></footer>
  </div>;
}
