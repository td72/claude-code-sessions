// How numbers and times are written, as the terminal viewer writes them.

export function tokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

/** `$0.0042`, `$1.23`: more digits only where cents say nothing. */
export function usd(amount: number): string {
  return amount > 0 && amount < 0.01 ? `$${amount.toFixed(4)}` : `$${amount.toFixed(2)}`;
}

/** `~$1.23` when any of it is an estimate, `—` when there is nothing to go on. */
export function cost(c: { usd: number | null; source: string; incomplete: boolean }): string {
  if (c.usd === null) return "—";
  const estimated = c.source === "estimated" || c.source === "reported_plus_estimate";
  return `${estimated ? "~" : ""}${usd(c.usd)}${c.incomplete ? "+" : ""}`;
}

export function duration(ms: number | null): string {
  if (ms === null) return "";
  if (ms < 1000) return `${ms}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m${String(Math.floor(seconds % 60)).padStart(2, "0")}s`;
  return `${Math.floor(minutes / 60)}h${String(minutes % 60).padStart(2, "0")}m`;
}

/** How long ago: `now`, `12s`, `5m`, `3h`, `2d`. */
export function ago(now: number, then: number | null): string {
  if (then === null) return "—";
  const seconds = Math.max(0, Math.floor((now - then) / 1000));
  if (seconds < 2) return "now";
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`;
  return `${Math.floor(seconds / 86400)}d`;
}

/** The local time of day, or the date and time when it is not today. */
export function when(now: number, then: number | null): string {
  if (then === null) return "—";
  const at = new Date(then);
  const today = new Date(now);
  const time = at.toLocaleTimeString([], { hour12: false });
  if (at.toDateString() === today.toDateString()) return time;
  const date = `${String(at.getMonth() + 1).padStart(2, "0")}-${String(at.getDate()).padStart(2, "0")}`;
  return `${date} ${time.slice(0, 5)}`;
}

/** `mcp__server__tool` as `server/tool`. */
export function toolName(name: string): string {
  const mcp = /^mcp__(.+?)__(.+)$/.exec(name);
  return mcp ? `${mcp[1]}/${mcp[2]}` : name;
}

export function modelName(model: string | null): string {
  return model ? model.replace(/^claude-/, "") : "";
}
