// Byte / rate / duration formatting.
//
// Every value here is padded to a predictable character count on purpose.
// The numeric gutter is a fixed-width grid; if a value reflows the whole
// column twitches. Real instruments do not twitch.

const IEC = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

function unitIndex(n) {
  let i = 0;
  while (n >= 1024 && i < IEC.length - 1) {
    n /= 1024;
    i++;
  }
  return i;
}

/** Size in binary units. `n == null` means "we do not know". */
export function bytes(n) {
  if (n == null || !Number.isFinite(n) || n < 0) return "—";
  const i = unitIndex(n);
  const v = n / 1024 ** i;
  const d = i === 0 ? 0 : v < 10 ? 2 : v < 100 ? 1 : 0;
  return `${v.toFixed(d)} ${IEC[i]}`;
}

/**
 * "received / total", both in the unit of the total.
 * Rendered as one token so the cell never changes width mid-transfer
 * (e.g. 1023.9 MiB -> 1.0 GiB would otherwise reflow the column).
 */
export function transferred(received, total) {
  if (received == null) return "—";
  if (total == null) return `${bytes(received)}?`;
  if (total <= 0) return bytes(received);
  const i = unitIndex(total);
  const d = total / 1024 ** i;
  const dec = d < 10 ? 2 : d < 100 ? 1 : 0;
  return `${((received ?? 0) / 1024 ** i).toFixed(dec)}/${d.toFixed(dec)} ${IEC[i]}`;
}

/** Transfer rate in decimal units, because that is what networks report. */
export function rate(bps) {
  if (bps == null || !Number.isFinite(bps) || bps <= 0) return "—";
  if (bps < 1000) return `${Math.round(bps)} B/s`;
  const SI = ["kB/s", "MB/s", "GB/s", "TB/s"];
  let v = bps;
  let i = -1;
  while (v >= 1000 && i < SI.length - 1) {
    v /= 1000;
    i++;
  }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${SI[i]}`;
}

/** Compact duration. Returns "—" for anything not yet knowable. */
export function duration(sec) {
  if (sec == null || !Number.isFinite(sec) || sec < 0) return "—";
  if (sec < 1) return "<1s";
  if (sec < 60) return `${Math.round(sec)}s`;
  if (sec < 3600) {
    return `${Math.floor(sec / 60)}m ${String(Math.round(sec % 60)).padStart(2, "0")}s`;
  }
  const h = Math.floor(sec / 3600);
  const m = Math.round(((sec % 3600) / 60) % 60);
  return `${h}h ${String(m).padStart(2, "0")}m`;
}

/** Seconds remaining, from a total and a current rate. */
export function remaining(received, total, bps) {
  if (total == null || !bps) return "—";
  return duration((total - received) / bps);
}

/** Wall-clock time of day, for the event recorder. */
export function clock(ms) {
  const d = new Date(ms);
  const p = (n) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/** Percentage as an integer, or null when the length is unknown. */
export function percent(received, total) {
  if (total == null || total <= 0) return null;
  return Math.min(100, Math.floor((received / total) * 100));
}
