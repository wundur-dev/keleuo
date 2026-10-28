// model_from_text and model_from_image: Tripo. Starts a job, waits for it, and saves the GLB
// and its preview image (returned so the agent can look).
export default async function (args, kx) {
  const key = kx.secret("TRIPO_API_KEY");
  if (!key) return kx.fail("No Tripo key: add it in Settings > Tools > Packs.");
  const api = kx.setting("tripo_url") || "https://openapi.tripo3d.ai/v3";
  const auth = { authorization: `Bearer ${key}` };
  const failed = (res, doing) => {
    let why = res.text();
    try { why = res.json().message || why; } catch (_) {}
    return kx.fail(`Tripo said ${res.status} ${doing}: ${why}`);
  };
  const model = kx.setting("model_version") || "v3.1-20260211";
  let start, body, what;
  if (kx.tool === "model_from_image") {
    if (!args.image) return kx.fail("model_from_image needs the image.");
    let file;
    try { file = kx.readFile(args.image); } catch (e) { return kx.fail(`Couldn't read the image: ${e.message}`); }
    kx.progress("Tripo: uploading the image");
    const up = kx.fetch(`${api}/files`, { method: "POST", headers: auth, form: { file } });
    if (!up.ok) return failed(up, "to the upload");
    const token = (up.json().data || {}).file_token;
    if (!token) return kx.fail("Tripo sent no file token for the upload.");
    start = `${api}/generation/image-to-model`;
    body = { input: token, model };
    what = args.name || file.filename.replace(/\.[^.]+$/, "");
  } else {
    start = `${api}/generation/text-to-model`;
    body = { prompt: args.prompt, model };
    what = args.name || args.prompt;
  }
  if (args.faces) body.face_limit = Math.round(Number(args.faces));
  if (args.texture_quality) body.texture_quality = args.texture_quality;
  if (args.pbr) body.pbr = true;
  const res = kx.fetch(start, { method: "POST", headers: auth, json: body });
  if (!res.ok) return failed(res, "to the job");
  const task = (res.json().data || {}).task_id;
  if (!task) return kx.fail("Tripo sent no task id.");

  // Wait for it (Tripo asks for no more than a request a second).
  let job;
  for (;;) {
    kx.sleep(Number(kx.setting("poll_ms")) || 2000);
    const r = kx.fetch(`${api}/tasks/${encodeURIComponent(task)}`, { headers: auth });
    if (!r.ok) return failed(r, "about the job");
    job = r.json().data || {};
    if (job.status === "success") break;
    if (["failed", "cancelled", "banned", "expired", "unknown"].includes(job.status)) {
      return kx.fail(`The Tripo job ${task} ended: ${job.status}.`);
    }
    kx.progress(`Tripo: ${job.status || "waiting"} ${job.progress || 0}%`);
  }
  // The links last minutes: fetch them now.
  const out = job.output || {};
  const url = out.model_url || out.pbr_model_url || out.base_model_url;
  if (!url) return kx.fail(`The Tripo job ${task} finished without a model.`);
  const glb = kx.fetch(url);
  if (!glb.ok) return kx.fail(`Couldn't download the model (${glb.status}).`);
  const name = kx.slug(what);
  const folder = kx.setting("output_folder");
  const file = kx.writeFile(`${folder}/${name}.glb`, glb.bytes());
  const images = [];
  if (out.rendered_image_url) {
    const p = kx.fetch(out.rendered_image_url);
    if (p.ok) {
      const ext = (out.rendered_image_url.split("?")[0].match(/\.(png|jpe?g|webp)$/i) || [, "webp"])[1].toLowerCase();
      images.push(kx.writeFile(`${folder}/${name}-preview.${ext}`, p.bytes()));
    }
  }
  return { text: `Saved ${file}${images.length ? ` and its preview ${images[0]}` : ""} (Tripo task ${task}).`, files: [file], images };
}
