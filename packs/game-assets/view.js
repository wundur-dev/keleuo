// view_model: a page to look at a 3D model in, written next to it and opened in Keleuo's web
// panel: one HTML file holding the model and three.js, so it needs no server and opens in any
// browser too. Drag to turn, scroll to zoom; pick and play its animations.
export default async function (args, kx) {
  const model = String(args.model || "").trim();
  if (!/\.glb$/i.test(model)) return kx.fail("view_model shows a GLB file in the project (model_from_*, rig_model and most tools make them).");
  const base = model.replace(/\.glb$/i, "");
  const title = args.title || base.split("/").pop();
  const options = { title, animation: args.animation || "" };
  const out = args.name ? `${model.split("/").slice(0, -1).join("/")}/${kx.slug(args.name)}.html`.replace(/^\//, "") : `${base}-viewer.html`;
  const esc = (t) => String(t).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  let file;
  try {
    file = kx.writeFile(out, [
      `<!doctype html><html><head><meta charset="utf-8"><title>${esc(title)}</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  html, body { margin: 0; height: 100%; overflow: hidden; font: 13px -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; color: #222; }
  #stage { position: absolute; inset: 0; }
  #panel { position: absolute; left: 12px; bottom: 12px; right: 12px; display: flex; flex-wrap: wrap; gap: 10px 14px; align-items: center;
    padding: 10px 12px; border-radius: 10px; background: rgba(255,255,255,0.86); box-shadow: 0 2px 12px rgba(0,0,0,0.12); backdrop-filter: blur(8px); }
  body.dark #panel { background: rgba(30,30,34,0.86); color: #eee; }
  #info { position: absolute; left: 14px; top: 12px; opacity: 0.7; }
  #status { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; pointer-events: none; opacity: 0.7; }
  #anim { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
  #anim[hidden], #followrow[hidden] { display: none; }
  button, select { font: inherit; padding: 3px 10px; border-radius: 6px; border: 1px solid rgba(0,0,0,0.2); background: #fff; color: #222; }
  label { display: inline-flex; gap: 5px; align-items: center; white-space: nowrap; }
  #time { width: 180px; }
  .gap { flex: 1; }
</style></head><body>
<div id="stage"></div><div id="info"></div><div id="status"></div>
<div id="panel">
  <span id="anim" hidden>
    <select id="clip"></select><button id="play">Pause</button>
    <input id="time" type="range" min="0" max="1" step="0.001" value="0">
    <label>Speed <input id="speed" type="range" min="0.1" max="2" step="0.05" value="1"><span id="speedv">1.00×</span></label>
  </span>
  <label id="followrow" hidden><input id="follow" type="checkbox" checked> Follow</label>
  <span class="gap"></span>
  <label><input id="spin" type="checkbox"> Turn</label>
  <label><input id="floor" type="checkbox" checked> Floor</label>
  <label><input id="dark" type="checkbox"> Dark</label>
  <button id="reset">Reset view</button><button id="save">Save PNG</button>
</div>
<script>`,
      kx.packFile("vendor/three/three.min.js"),
      "</script><script>",
      kx.packFile("vendor/three/GLTFLoader.js"),
      "</script><script>",
      kx.packFile("vendor/three/OrbitControls.js"),
      '</script><script>const MODEL_B64="',
      kx.embed(model),
      `";const OPTIONS=${JSON.stringify(options).replace(/</g, "\\u003c")};</script><script>`,
      kx.packFile("viewer.js"),
      "</script></body></html>\n",
    ]);
  } catch (e) {
    return kx.fail(`Couldn't make the viewer: ${e.message}`);
  }
  return { text: `Saved ${file}, a viewer for ${model}: it opens in the web panel (and in any browser). Drag to turn, scroll to zoom${args.animation ? `; playing "${args.animation}"` : ""}.`, files: [file], show: file };
}
