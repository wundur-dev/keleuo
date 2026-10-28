// rig_model: Tripo. Checks the model can be rigged (and as what), rigs it, and, when asked,
// applies preset animations; saves the rigged model (and the animated one).
export default async function (args, kx) {
  const key = kx.secret("TRIPO_API_KEY");
  if (!key) return kx.fail("No Tripo key: add it in Settings > Tools > Packs.");
  const api = kx.setting("tripo_url") || "https://openapi.tripo3d.ai/v3";
  const auth = { authorization: `Bearer ${key}` };
  const said = (res) => {
    let why = res.text();
    try { why = res.json().message || why; } catch (_) {}
    return `${res.status}: ${why}`;
  };
  // Start a job and wait for it (Tripo asks for no more than a request a second): its output.
  const job = (path, body, doing) => {
    const res = kx.fetch(`${api}${path}`, { method: "POST", headers: auth, json: body });
    if (!res.ok) throw new Error(`Tripo said ${said(res)} to the ${doing}`);
    const task = (res.json().data || {}).task_id;
    if (!task) throw new Error(`Tripo sent no task id for the ${doing}.`);
    for (;;) {
      kx.sleep(Number(kx.setting("poll_ms")) || 2000);
      const r = kx.fetch(`${api}/tasks/${encodeURIComponent(task)}`, { headers: auth });
      if (!r.ok) throw new Error(`Tripo said ${said(r)} about the ${doing}`);
      const data = r.json().data || {};
      if (data.status === "success") return { task, output: data.output || {} };
      if (["failed", "cancelled", "banned", "expired", "unknown"].includes(data.status)) {
        throw new Error(`The Tripo ${doing} (${task}) ended: ${data.status}.`);
      }
      kx.progress(`Tripo ${doing}: ${data.status || "waiting"} ${data.progress || 0}%`);
    }
  };

  try {
    // The model: a Tripo task (from model_from_text or model_from_image), or a file here.
    const source = String(args.model || "").trim();
    if (!source) return kx.fail("rig_model needs the model: a Tripo task id (from model_from_*), or a GLB or FBX in the project.");
    let input, what;
    if (/^task[_-]|^[0-9a-f-]{20,}$/i.test(source) && !source.includes("/") && !/\.\w+$/.test(source)) {
      input = source;
      what = args.name || "model";
    } else {
      let file;
      try { file = kx.readFile(source); } catch (e) { return kx.fail(`Couldn't read the model: ${e.message}`); }
      kx.progress("Tripo: uploading the model");
      const up = kx.fetch(`${api}/files`, { method: "POST", headers: auth, form: { file } });
      if (!up.ok) return kx.fail(`Tripo said ${said(up)} to the upload`);
      input = (up.json().data || {}).file_token;
      if (!input) return kx.fail("Tripo sent no file token for the upload.");
      what = args.name || file.filename.replace(/\.[^.]+$/, "");
    }

    // What kind of skeleton: as asked, else as Tripo's check says (and whether it can be rigged).
    let rigType = args.rig_type;
    if (!rigType) {
      const check = job("/animations/rig-check", { input }, "rig check").output;
      if (check.riggable === false) return kx.fail("Tripo says this model can't be rigged (it needs a clear body: a character or an animal, in one piece).");
      rigType = check.rig_type || "biped";
    }
    const rigBody = { input, model: kx.setting("rig_version") || "v3.0-20260909", rig_type: rigType, out_format: args.format || "glb" };
    if (rigType === "biped" && args.skeleton) rigBody.spec = args.skeleton;
    const rigged = job("/animations/rig", rigBody, "rig");

    const folder = kx.setting("output_folder");
    // (A name already ending in -rigged doesn't get it twice.)
    const name = kx.slug(what).replace(/-rigged$/, "");
    const ext = rigBody.out_format;
    const files = [];
    if (!rigged.output.model_url) return kx.fail(`The Tripo rig (${rigged.task}) finished without a model.`);
    kx.progress("Tripo: downloading the rigged model");
    const r = kx.download(rigged.output.model_url, `${folder}/${name}-rigged.${ext}`);
    if (!r.ok) return kx.fail(`Couldn't download the rigged model (${r.status}).`);
    files.push(r.path);

    // Animations, if asked: Tripo's presets, up to five, in one file.
    const wanted = String(args.animations || "").split(",").map((a) => a.trim()).filter(Boolean).map((a) => (a.startsWith("preset:") ? a : `preset:${a}`));
    let animated = "";
    if (wanted.length) {
      if (wanted.length > 5) return kx.fail("Tripo takes up to five animations at a time.");
      const body = { input: rigged.task, out_format: ext, animate_in_place: !!args.in_place };
      if (wanted.length === 1) body.animation = wanted[0];
      else body.animations = wanted;
      const anim = job("/animations/retarget", body, "animation");
      if (anim.output.model_url) {
        kx.progress("Tripo: downloading the animated model");
        const a = kx.download(anim.output.model_url, `${folder}/${name}-animated.${ext}`);
        if (a.ok) {
          files.push(a.path);
          animated = ` and ${a.path} (${wanted.map((w) => w.replace("preset:", "")).join(", ")}; Tripo task ${anim.task})`;
        }
      }
    }
    return { text: `Rigged as ${rigType}: saved ${files[0]} (Tripo task ${rigged.task})${animated}.`, files };
  } catch (e) {
    return kx.fail(e.message);
  }
}
