// make_image and edit_image: gpt-image, from OpenAI directly or through Azure OpenAI.
export default async function (args, kx) {
  const editing = kx.tool === "edit_image";
  const azure = kx.setting("image_provider") === "azure";
  const size = args.size || (editing ? "auto" : "1024x1024");
  const quality = args.quality || "medium";
  const background = args.transparent ? "transparent" : "auto";
  let base, headers, model;
  if (azure) {
    // The resource's address, as Azure shows it in any of its forms: Azure OpenAI
    // (https://x.openai.azure.com), Foundry (https://x.services.ai.azure.com/models), or with
    // /openai on the end.
    const endpoint = (kx.setting("azure_endpoint") || "")
      .trim()
      .replace(/\/+$/, "")
      .replace(/\/(models|openai\/v1|openai)$/i, "");
    const deployment = kx.setting("azure_deployment");
    const key = kx.secret("AZURE_OPENAI_KEY");
    if (!endpoint || !deployment || !key) {
      return kx.fail("Azure OpenAI isn't set up: give the endpoint, the deployment and the key in Settings > Tools > Packs.");
    }
    base = `${endpoint}/openai/deployments/${encodeURIComponent(deployment)}/images/`;
    headers = { "api-key": key };
  } else {
    const key = kx.secret("OPENAI_API_KEY");
    if (!key) return kx.fail("No OpenAI key: add it in Settings > Tools > Packs (or choose Azure).");
    base = `${kx.setting("openai_url") || "https://api.openai.com/v1"}/images/`;
    headers = { authorization: `Bearer ${key}` };
    model = kx.setting("image_model") || "gpt-image-2.5-sunburst";
  }
  const query = azure ? `?api-version=${kx.setting("azure_api_version")}` : "";
  let res;
  if (editing) {
    // The images to change (several: the first is changed, the rest are references), and an
    // optional mask whose transparent pixels say where.
    if (!args.image) return kx.fail("edit_image needs the image to change.");
    const images = String(args.image).split(",").map((p) => p.trim()).filter(Boolean);
    let files;
    try {
      files = images.map((p) => kx.readFile(p));
    } catch (e) {
      return kx.fail(`Couldn't read the image: ${e.message}`);
    }
    const form = { prompt: args.prompt, size, quality, background, output_format: "png", n: "1", "image[]": files };
    if (model) form.model = model;
    if (args.mask) {
      try { form.mask = kx.readFile(args.mask); } catch (e) { return kx.fail(`Couldn't read the mask: ${e.message}`); }
    }
    kx.progress(`edit_image: ${args.prompt.slice(0, 60)}`);
    res = kx.fetch(base + "edits" + query, { method: "POST", headers, form });
  } else {
    const body = { prompt: args.prompt, size, quality, background, output_format: "png", n: 1 };
    if (model) body.model = model;
    kx.progress(`make_image: ${args.prompt.slice(0, 60)}`);
    res = kx.fetch(base + "generations" + query, { method: "POST", headers, json: body });
  }
  if (!res.ok) {
    let why = res.text();
    try { why = res.json().error.message; } catch (_) {}
    return kx.fail(`The image service said ${res.status}: ${why}`);
  }
  const data = res.json();
  const image = data.data && data.data[0] && data.data[0].b64_json;
  if (!image) return kx.fail("The image service sent no image.");
  const from = editing ? String(args.image).split(",")[0].trim().split("/").pop().replace(/\.[^.]+$/, "") + "-edit" : args.prompt;
  const name = kx.slug(args.name || from);
  const file = kx.writeFile(`${kx.setting("output_folder")}/${name}.png`, kx.base64(image));
  return { text: `Saved ${file} (${size}, ${quality}${args.transparent ? ", transparent" : ""}).`, images: [file] };
}
