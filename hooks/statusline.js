#!/usr/bin/env node
const fs = require("fs");
const path = require("path");

const file = path.join(process.env.APPDATA || "", "notify", "claude-limits.json");
const num = (v) => (typeof v === "number" && Number.isFinite(v) ? v : null);

function window(w) {
  const used = num(w && w.used_percentage);
  if (used === null) return null;
  const out = { used_percentage: used };
  const reset = num(w.resets_at);
  if (reset !== null) out.resets_at = reset;
  return out;
}

let raw = "";
process.stdin.on("data", (c) => (raw += c));
process.stdin.on("end", () => {
  let input = {};
  try {
    input = JSON.parse(raw);
  } catch {}

  let old = {};
  try {
    old = JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {}

  const rate = input.rate_limits || {};
  const cw = input.context_window || {};
  const ctxUsed = num(cw.used_percentage);
  const context = ctxUsed === null ? old.context || null : {
    used_percentage: ctxUsed,
    ...(num(cw.context_window_size) !== null && { size: cw.context_window_size }),
  };

  const data = {
    updated_at: Math.floor(Date.now() / 1000),
    five_hour: window(rate.five_hour) || old.five_hour || null,
    seven_day: window(rate.seven_day) || old.seven_day || null,
    context,
  };

  try {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, JSON.stringify(data));
  } catch {}

  const pct = (w) => (w ? Math.round(w.used_percentage) + "%" : "-");
  const model = (input.model && input.model.display_name) || "Claude";
  console.log(`${model} | 5h ${pct(data.five_hour)} | 7d ${pct(data.seven_day)} | ctx ${pct(data.context)}`);
});
