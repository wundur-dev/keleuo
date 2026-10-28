// make_image: gpt-image, from OpenAI directly or through Azure OpenAI.
export default async function (args, kx) {
  const azure = kx.setting("image_provider") === "azure";
  const body = {
    prompt: args.prompt,
    size: args.size || "1024x1024",
    quality: args.quality || "medium",
    background: args.transparent ? "transparent" : "auto",
    output_format: "png",
    n: 1,
  };
  let url, headers;
  if (azure) {
    const endpoint = (kx.setting("azure_endpoint") || "").replace(/\/+$/, "");
    const deployment = kx.setting("azure_deployment");
    const key = kx.secret("AZURE_OPENAI_KEY");
    if (!endpoint || !deployment || !key) {
      return kx.fail("Azure OpenAI isn't set up: give the endpoint, the deployment and the key in Settings > Tools > Packs.");
    }
    url = `${endpoint}/openai/deployments/${encodeURIComponent(deployment)}/images/generations?api-version=${kx.setting("azure_api_version")}`;
    headers = { "api-key": key };
  } else {
    const key = kx.secret("OPENAI_API_KEY");
    if (!key) return kx.fail("No OpenAI key: add it in Settings > Tools > Packs (or choose Azure).");
    url = "https://api.openai.com/v1/images/generations";
    headers = { authorization: `Bearer ${key}` };
    body.model = kx.setting("image_model") || "gpt-image-1";
  }
  kx.progress(`make_image: ${args.prompt.slice(0, 60)}`);
  const res = await kx.fetch(url, { method: "POST", headers, json: body });
  if (!res.ok) {
    let why = res.text();
    try { why = res.json().error.message; } catch (_) {}
    return kx.fail(`The image service said ${res.status}: ${why}`);
  }
  const data = res.json();
  const image = data.data && data.data[0] && data.data[0].b64_json;
  if (!image) return kx.fail("The image service sent no image.");
  const name = args.name ? kx.slug(args.name) : kx.slug(args.prompt);
  const file = kx.writeFile(`${kx.setting("output_folder")}/${name}.png`, kx.base64(image));
  return { text: `Saved ${file} (${body.size}, ${body.quality}${args.transparent ? ", transparent" : ""}).`, images: [file] };
}
